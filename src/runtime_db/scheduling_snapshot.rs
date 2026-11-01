//! Scheduling qualification and explicit same-transaction preparation.
#[cfg(test)]
use super::context_inputs::ContextDurableRead;
use super::{context_inputs::ContextScopeFacts, repositories};
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
        resident_scheduling::*,
    },
    types::AgentRegistryStatus,
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
mod deletion_maintenance;
pub(crate) use deletion_maintenance::{DeletionSchedulingAck, DeletionSchedulingSelection};
mod cold_query;
mod token_discovery;
pub(crate) use cold_query::{
    prepare_cold_scheduling_query_tx, read_cold_scheduling_scope_tx, ColdSchedulingQuery,
};
pub(crate) use token_discovery::{
    discover_external_trigger_token_tx, read_external_trigger_token_compatibility_tx,
    ExternalTriggerTokenLocator, ExternalTriggerTokenRecordRead,
};
mod history_selection;
mod loader;
#[cfg(test)]
pub(in crate::runtime_db) mod material_cost_fixture;
mod pending_wake;
mod qualification;
mod seed_maintenance;
mod selector_sql;
pub(in crate::runtime_db) mod startup_maintenance;
mod stored;
pub(crate) use startup_maintenance::{StartupSchedulingAck, StartupSchedulingMaterial};
#[cfg(test)]
mod tests;
#[cfg(test)]
mod trigger_writer_tests;
mod writes;
#[cfg(test)]
pub(crate) use loader::{
    load_scheduling_domain_snapshot_tx, prepare_original_scheduling_recovery_snapshot_tx,
};
pub(crate) use loader::{
    prepare_scheduling_domain_snapshot_tx, prepare_scheduling_with_context_scope_tx,
};
pub(in crate::runtime_db) use qualification::{
    capture_fixed_checkpoint_on, capture_scheduling_seed_if_ready_tx, consume_scheduling_upgrade_tx,
};
pub(crate) use writes::ExternalTriggerWriterObservation;
pub(crate) use writes::{
    prepare_bootstrap_wake_normalization_tx, prepare_existing_scheduling_maintenance_tx,
    prepare_original_scheduling_cleanup_tx, prepare_scheduling_write_tx,
    prepare_scheduling_write_with_memory_budget_tx, register_existing_scheduling_cleanup_tx,
    register_existing_scheduling_maintenance_tx, register_scheduling_writes_tx,
    register_scheduling_writes_with_memory_budget_tx, reserve_scheduling_write_keys_tx,
    CommittedSchedulingFacts, ExistingSchedulingMaintenanceProof, SchedulingMaintenanceAck,
    SchedulingScopeInvalidation, SchedulingWriteProof,
};
pub(super) const SCHEDULING_SCHEMA: &str = include_str!("scheduling_snapshot/schema.sql");
const MAX_RECORDS: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
pub(crate) enum SchedulingSnapshotError {
    #[error("Scheduling needs preparation: {0:?}")]
    NeedsPreparation(SchedulingKey),
    #[error("Scheduling canonical body/projection changed: {0:?}")]
    CanonicalChanged(SchedulingKey),
    #[error("Scheduling record belongs to a foreign owner: {0:?}")]
    ForeignRecord(SchedulingKey),
    #[error("Scheduling scope retired or lacks State")]
    RetiredScope,
    #[error("Scheduling legacy import not sealed")]
    LegacyImportPending,
    #[error("Scheduling revision mismatch: expected {expected}, actual {actual:?}")]
    RevisionMismatch { expected: u64, actual: Option<u64> },
    #[error("Scheduling ResourceExhausted: {0}")]
    ResourceExhausted(&'static str),
}
#[derive(Debug, Clone)]
pub(crate) struct SchedulingDomainLoadRequest {
    pub capture: SchedulingCaptureRequest,
    pub record_limit: usize,
    pub byte_limit: usize,
}
#[derive(Debug)]
pub(crate) struct SchedulingCurrentSnapshot {
    pub facts: ContextScopeFacts,
    pub resident: ResidentSchedulingDomain,
    pub input_roots: BTreeSet<InputEvidenceKey>,
    pub(crate) material_usage: (usize, usize),
    pub(crate) _read_material: Vec<Arc<BudgetViewGuard>>,
}
#[derive(Debug)]
pub(crate) enum SchedulingBootstrapSnapshot {
    Current(Box<SchedulingCurrentSnapshot>),
    Retired {
        // Original negative-read provenance, never successor or Ready authority.
        _scope: InputEvidenceScope,
        _read_material: Vec<Arc<BudgetViewGuard>>,
    },
}
impl SchedulingBootstrapSnapshot {
    pub(crate) fn require_current(self) -> Result<SchedulingCurrentSnapshot> {
        match self {
            Self::Current(x) => Ok(*x),
            Self::Retired { .. } => Err(SchedulingSnapshotError::RetiredScope.into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::runtime_db) struct SchedulingContinuityCheckpoint {
    pub policy_version: u32,
    pub inventory_id: String,
    pub agent_id: String,
    pub observed_tables: BTreeSet<SchedulingFamily>,
    pub member_count: u64,
    pub metadata_bytes: u64,
    pub content_digest: String,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn family_name(f: SchedulingFamily) -> &'static str {
    match f {
        SchedulingFamily::Timer => "timer",
        SchedulingFamily::TimerWake => "timer_wake",
        SchedulingFamily::ExternalTrigger => "external_trigger",
    }
}

fn bound(records: usize, bytes: usize, request: &SchedulingDomainLoadRequest) -> Result<()> {
    ensure!(
        request.record_limit > 0 && request.byte_limit > 0,
        "zero Scheduling budget"
    );
    ensure!(
        records <= request.record_limit.min(MAX_RECORDS)
            && bytes <= request.byte_limit.min(MAX_BYTES),
        SchedulingSnapshotError::ResourceExhausted("combined read material")
    );
    Ok(())
}
struct Material<'a, 'memory> {
    scope: &'a InputEvidenceScope,
    request: &'a SchedulingDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
    memory_budget: Option<&'memory mut super::dispatch_memory::DispatchMemoryTransactionBudget>,
}
impl<'a, 'memory> Material<'a, 'memory> {
    fn new(
        scope: &'a InputEvidenceScope,
        request: &'a SchedulingDomainLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
    ) -> Self {
        Self {
            scope,
            request,
            ledger,
            bytes: 0,
            records: 0,
            guards: Vec::new(),
            memory_budget: None,
        }
    }
    fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        let phase_bytes = bytes
            .checked_add(self.scope.agent_id.len())
            .and_then(|n| n.checked_add(128))
            .context("Scheduling guard metadata overflow")?;
        let phase_records = records;
        let bytes = phase_bytes
            .checked_mul(8)
            .context("Scheduling read bytes overflow")?;
        let records = records
            .checked_mul(8)
            .context("Scheduling read records overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Scheduling combined bytes overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Scheduling combined records overflow")?;
        bound(self.records, self.bytes, self.request)?;
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(phase_bytes)?,
            phase_records,
        )?;
        if let Some(memory) = &mut self.memory_budget {
            memory.additional_material(bytes, records, 1)?;
        }
        if let Some(guard) =
            crate::diagnostics::resource_io::ResourceReadMaterial::active_owned_guard(
                self.ledger,
                self.scope,
            )?
        {
            // The phase already owns the charged bytes. Retain that view, not
            // a second ledger allocation for the same owning material.
            if !self.guards.iter().any(|held| Arc::ptr_eq(held, &guard)) {
                self.guards.push(guard);
            }
        } else {
            self.guards.push(self.ledger.register_owned_bytes(
                self.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: u64::try_from(bytes)?,
                    metadata_estimate_bytes: 0,
                },
            )?);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    revision: u64,
    availability: SchedulingAvailability,
    content: Option<String>,
    projection: Option<String>,
    status: Option<String>,
    sort_at: Option<String>,
    created_at: Option<String>,
}
fn read_alias(
    tx: &Connection,
    scope: &InputEvidenceScope,
    key: &SchedulingKey,
    material: &mut Material<'_, '_>,
) -> Result<Option<Alias>> {
    let size:Option<usize>=tx.query_row("SELECT length(CAST(record_id AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0)+length(CAST(availability AS BLOB))+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(sort_at AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+64 FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id],|r|r.get(0)).optional()?;
    let Some(size) = size else { return Ok(None) };
    material.charge(size, 1)?;
    let (revision,availability,content,projection,status,sort_at,created_at):(u64,String,Option<String>,Option<String>,Option<String>,Option<String>,Option<String>)=tx.query_row("SELECT revision,availability,content_digest,projection_digest,status,sort_at,created_at FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let availability = match availability.as_str() {
        "present" => SchedulingAvailability::Present,
        "absent" => SchedulingAvailability::Absent,
        "pruned" => SchedulingAvailability::Pruned,
        _ => anyhow::bail!("invalid Scheduling alias"),
    };
    ensure!(
        revision > 0
            && if availability == SchedulingAvailability::Present {
                content.as_ref().is_some_and(|s| s.len() == 64)
                    && projection.as_ref().is_some_and(|s| s.len() == 64)
                    && status.is_some()
                    && sort_at.is_some()
                    && created_at.is_some()
            } else {
                content.is_none() && projection.is_none()
            },
        "invalid Scheduling alias digest"
    );
    Ok(Some(Alias {
        revision,
        availability,
        content,
        projection,
        status,
        sort_at,
        created_at,
    }))
}
fn write_alias(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &SchedulingKey,
    revision: u64,
    availability: SchedulingAvailability,
    content: Option<&str>,
    projection: Option<&str>,
    header: Option<(&str, &str, &str)>,
) -> Result<()> {
    let status = match availability {
        SchedulingAvailability::Present => "present",
        SchedulingAvailability::Absent => "absent",
        SchedulingAvailability::Pruned => "pruned",
        SchedulingAvailability::HistoricalUnavailable => {
            anyhow::bail!("historical table observation is not an alias")
        }
    };
    tx.execute("INSERT INTO scheduling_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest,status,sort_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,status=excluded.status,sort_at=excluded.sort_at,created_at=excluded.created_at",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id,revision,status,content,projection,header.map(|h|h.0),header.map(|h|h.1),header.map(|h|h.2)])?;
    Ok(())
}
