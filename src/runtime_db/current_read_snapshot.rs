//! Explicit finite owner preparation; ordinary CurrentRead capture has no SQL path.
use super::repositories;
use crate::{
    runtime::{
        current_read_facts::*,
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentIdentityRecord, AgentRegistryStatus, AgentState, AuditEvent, QueueEntryRecord},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
mod business_writer;
mod stop_events;
pub(in crate::runtime_db) use stop_events::OriginalStopCurrentEvents;
mod cleanup;
mod event_queries;
mod fixed;
#[cfg(test)]
mod import_tests;
mod loader;
mod record_invalidation;
mod stored;
#[cfg(test)]
mod tests;
mod writes;
pub(super) const CURRENT_READ_SCHEMA: &str = include_str!("current_read_snapshot/schema.sql");
pub(super) const CURRENT_EVENT_BIRTH_SCHEMA: &str =
    include_str!("current_read_snapshot/event_births.sql");
pub(crate) use business_writer::CurrentReadBusinessTransaction;
pub(in crate::runtime_db) use cleanup::{
    observe_original_incomplete_recovery_using_input_tx, qualified_existing_cleanup_tx,
};
pub(crate) use fixed::capture_current_read_upgrade_tx;
pub(in crate::runtime_db) use fixed::{
    capture_current_read_import_if_ready_tx, capture_current_read_legacy_seed_if_ready_tx,
    record_current_read_import_tx,
};
pub(in crate::runtime_db) use loader::prepare_current_read_using_input_material_tx;
pub(crate) use loader::{
    prepare_current_read_snapshot_tx, prepare_original_current_read_recovery_tx,
};
pub(crate) use writes::{
    append_current_read_event_tx, prepare_current_read_write_tx, register_current_read_writes_tx,
    upsert_current_read_queue_tx, CommittedCurrentReadFacts, CommittedCurrentReadMaintenance,
    CurrentReadActualWrite, CurrentReadWriteProof,
};
#[derive(Debug)]
pub(crate) struct CurrentReadSnapshot {
    pub resident: Arc<ResidentCurrentReadFacts>,
    pub control_revision: u64,
    pub(crate) _material: Vec<Arc<BudgetViewGuard>>,
}
fn family(f: CurrentReadFamily) -> &'static str {
    match f {
        CurrentReadFamily::Queue => "queue",
        CurrentReadFamily::Event => "event",
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(in crate::runtime_db) fn typed_digest(value: &impl serde::Serialize) -> Result<String> {
    // Hash the canonical typed value without a second encoded body allocation.
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}
/// Borrow only this actual Current decoder's still-held allocation guards.
/// It carries accounting, never a source or mutation capability.
pub(in crate::runtime_db) struct ObservedCurrentReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    usage: (usize, usize),
    guards: &'a [Arc<BudgetViewGuard>],
}
impl<'a> ObservedCurrentReadMaterial<'a> {
    /// Borrow only newly allocated material of this same concrete writer. The
    /// enclosing Input observer stores the original admitted counter itself.
    pub(in crate::runtime_db) fn after_admitted(&self, previous: (usize, usize)) -> Result<Self> {
        Ok(Self { scope: self.scope, ledger: self.ledger, guards: self.guards,
            usage: (self.usage.0.checked_sub(previous.0).context("Current writer byte counter retired")?,
                self.usage.1.checked_sub(previous.1).context("Current writer node counter retired")?) })
    }
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Current material differs from original Input scope/ledger"
        );
        ensure!(
            !self.guards.is_empty(),
            "Current material has no original guards"
        );
        for guard in self.guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(self.usage)
    }
}
struct Material<'a> {
    scope: &'a InputEvidenceScope,
    request: &'a CurrentReadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
struct SelectedMaterialMark {
    bytes: usize,
    records: usize,
    guards: usize,
}
impl<'a> Material<'a> {
    fn selected_mark(&self) -> SelectedMaterialMark {
        SelectedMaterialMark {
            bytes: self.bytes,
            records: self.records,
            guards: self.guards.len(),
        }
    }
    /// Only after this row's raw/normalized/expected decoder scratch is dead.
    /// The exact qualified Arc remains observed through the real handoff.
    fn finish_selected_record(
        &mut self,
        mark: SelectedMaterialMark,
        record: Option<&Arc<QualifiedCurrentReadFact>>,
    ) -> Result<()> {
        ensure!(
            mark.guards <= self.guards.len()
                && self.guards[mark.guards..]
                    .iter()
                    .all(|guard| Arc::strong_count(guard) == 1),
            "CurrentRead selected decoder guard escaped its phase"
        );
        let (bytes, nodes) = record.map_or(Ok((0, 0)), |record| -> Result<_> {
            Ok((
                record
                    .resident_bytes
                    .checked_add(256)
                    .context("CurrentRead selected retained bytes overflow")?,
                record
                    .native_records
                    .checked_add(4)
                    .context("CurrentRead selected retained nodes overflow")?,
            ))
        })?;
        let next_bytes = mark
            .bytes
            .checked_add(bytes)
            .context("CurrentRead selected phase bytes overflow")?;
        let next_records = mark
            .records
            .checked_add(nodes)
            .context("CurrentRead selected phase nodes overflow")?;
        self.request.check_material(next_bytes, next_records)?;
        let retained = record
            .map(|record| {
                self.ledger.register_current_read_view(
                    self.scope,
                    InputMemoryCategory::Candidate,
                    std::iter::once(record),
                )
            })
            .transpose()?;
        self.guards.truncate(mark.guards);
        self.bytes = next_bytes;
        self.records = next_records;
        if let Some(guard) = retained {
            self.guards.push(guard);
        }
        Ok(())
    }
    fn new(
        scope: &'a InputEvidenceScope,
        request: &'a CurrentReadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
    ) -> Self {
        Self {
            scope,
            request,
            ledger,
            bytes: 0,
            records: 0,
            guards: Vec::new(),
        }
    }
    fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        let bytes = bytes
            .checked_add(self.scope.agent_id.len())
            .and_then(|n| n.checked_add(256))
            .and_then(|n| n.checked_mul(8))
            .context("CurrentRead material bytes overflow")?;
        let records = records
            .checked_mul(8)
            .context("CurrentRead material count overflow")?;
        self.reserve(bytes, records)
    }
    fn reserve(&mut self, bytes: usize, records: usize) -> Result<()> {
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context("CurrentRead combined bytes overflow")?;
        let next_records = self
            .records
            .checked_add(records)
            .context("CurrentRead combined count overflow")?;
        self.request.check_material(next_bytes, next_records)?;
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: 0,
            },
        )?;
        self.bytes = next_bytes;
        self.records = next_records;
        self.guards.push(guard);
        Ok(())
    }
    fn charge_command(&mut self, bytes: usize, nodes: usize) -> Result<()> {
        // The already-owned command and original repository return can coexist;
        // subsequent readback trees are admitted separately by native().
        self.reserve(
            bytes
                .checked_add(256)
                .and_then(|n| n.checked_mul(8))
                .context("CurrentRead command bytes overflow")?,
            nodes
                .checked_mul(2)
                .and_then(|n| n.checked_add(32))
                .context("CurrentRead command nodes overflow")?,
        )
    }
    fn share_existing(&mut self, bytes: usize, records: usize) -> Result<()> {
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context("CurrentRead shared phase bytes overflow")?;
        let next_records = self
            .records
            .checked_add(records)
            .context("CurrentRead shared phase records overflow")?;
        self.request.check_material(next_bytes, next_records)?;
        // This is the same already-accounted resident allocation, not a new body.
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }
    fn charge_native(&mut self, nodes: usize) -> Result<()> {
        // One decoded body tree; small normalized/expected projections coexist.
        // Arc pins share that tree, so they do not multiply its native nodes.
        self.reserve(
            nodes
                .checked_mul(256)
                .context("CurrentRead native bytes overflow")?,
            nodes
                .checked_add(32)
                .context("CurrentRead native count overflow")?,
        )
    }
    fn native(
        &mut self,
        connection: &Connection,
        table: &str,
        column: &str,
        key: &str,
        value: &str,
    ) -> Result<usize> {
        // All identifiers originate from the closed Queue/Event/State sources.
        let nodes:usize=connection.query_row(&format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree((SELECT {column} FROM {table} WHERE {key}=?1))"),[value],|r|r.get(0))?;
        self.charge_native(nodes)?;
        Ok(nodes)
    }
}
