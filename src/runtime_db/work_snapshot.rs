//! Same-transaction Work/wait/frame qualification and exact canonical facts.
//! A fixed seed or an actual new write admits a record; acquisition alone does not.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    context_inputs::{ContextDurableRead, ContextScopeFacts},
    repositories,
};
use crate::{
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
        resident_work::{
            CanonicalWorkRecord, CapturedWork, CommittedWorkDelta, QualifiedWorkRecord,
            ResidentWorkDomain, WorkAvailability, WorkCaptureRequest, WorkCompletionRef,
            WorkCoverage, WorkFamily, WorkHistoryCoverage, WorkKey, WORK_NONOPEN_WINDOW,
        },
    },
    types::{
        AgentRegistryStatus, AgentState, BriefKind, BriefRecord, TranscriptEntry,
        TranscriptEntryKind,
    },
};

mod completion;
mod completion_writes;
mod loader;
mod qualification;
mod relations;
mod seed_wait_cleanup;
#[cfg(test)]
mod tests;
mod writes;

pub(crate) use completion::completion_source_observations_tx;
pub(crate) use completion_writes::{
    finish_work_completion_write_tx, prepare_work_completion_write_tx,
};
pub(crate) use loader::{
    current_recovery_owning_input_roots, prepare_original_work_recovery_snapshot_tx,
    prepare_work_domain_snapshot_tx, validate_work_domain_observation_tx,
};
#[cfg(test)]
pub(crate) use loader::{load_work_domain_snapshot_tx, preflight_work_domain_snapshot_tx};
pub(super) use qualification::{
    capture_work_domain_seed_if_ready_tx, capture_work_domain_upgrade_tx,
};
pub(crate) use seed_wait_cleanup::{
    fixed_unbound_startup_wait_versions_tx, prepare_unbound_work_wait_cleanup_tx,
    register_unbound_work_wait_cleanup_tx,
};
pub(crate) use writes::{
    preflight_work_write_snapshot_tx, prepare_existing_work_wait_cleanup_tx,
    prepare_original_work_write_tx, prepare_work_write_tx, register_existing_work_wait_cleanup_tx,
    register_work_writes_tx, register_work_writes_with_view_tx, work_wait_write_keys_tx,
    WorkWriteProof,
};
#[cfg(test)]
pub(crate) use writes::{
    prepare_work_wait_cleanup_tx, register_work_pruned_tx, register_work_result_writes_tx,
    register_work_wait_cleanup_tx, CommittedWorkFacts,
};
pub(super) const WORK_DOMAIN_SCHEMA: &str = include_str!("work_snapshot/schema.sql");
#[cfg(test)]
pub(crate) fn install_work_domain_test_schema(tx: &Transaction<'_>) -> Result<()> {
    let ready: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_domain_current')",
        [],
        |row| row.get(0),
    )?;
    if !ready {
        tx.execute_batch(WORK_DOMAIN_SCHEMA)?;
        capture_work_domain_upgrade_tx(tx)?;
    }
    Ok(())
}
const MAX_METADATA_RECORDS: usize = 65_536;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLOSURE_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkSnapshotError {
    #[error("Work domain requires preparation: {0:?}")]
    NeedsPreparation(WorkKey),
    #[error("qualified Work canonical record changed outside its original transaction: {0:?}")]
    CanonicalChanged(WorkKey),
    #[error("Work scope is retired")]
    RetiredScope,
    #[error("Work domain revision mismatch: expected={expected}, actual={actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("Work legacy import is not sealed")]
    LegacyImportPending,
    #[error("Work domain resource exhausted: {0}")]
    ResourceExhausted(&'static str),
}

#[derive(Debug, Clone)]
pub(crate) struct WorkDomainLoadRequest {
    pub capture: WorkCaptureRequest,
    pub record_limit: usize,
    pub byte_limit: usize,
}
#[derive(Debug)]
pub(crate) struct WorkCurrentSnapshot {
    pub facts: ContextScopeFacts,
    pub resident: ResidentWorkDomain,
    /// D3 owning roots, prepared by the caller in this same original transaction.
    pub owning_input_roots: BTreeSet<InputEvidenceKey>,
}
#[derive(Debug)]
pub(crate) enum WorkBootstrapSnapshot {
    Current(Box<WorkCurrentSnapshot>),
    Retired { scope: InputEvidenceScope },
}
impl WorkBootstrapSnapshot {
    pub(crate) fn require_current(self) -> Result<WorkCurrentSnapshot> {
        match self {
            Self::Current(snapshot) => Ok(*snapshot),
            Self::Retired { scope } => Err(anyhow::Error::from(WorkSnapshotError::RetiredScope))
                .with_context(|| {
                    format!(
                        "Work scope {} incarnation {} is retired",
                        scope.agent_id, scope.identity_incarnation
                    )
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    projection: String,
    payload_bytes: usize,
}
#[derive(Debug, Clone)]
struct StoredRecord {
    record: CanonicalWorkRecord,
    payload: String,
    fingerprint: Fingerprint,
}
impl StoredRecord {
    fn digest(&self) -> String {
        digest(self.payload.as_bytes())
    }
    fn qualified(
        &self,
        scope: &InputEvidenceScope,
        revision: u64,
    ) -> Result<Arc<QualifiedWorkRecord>> {
        let result = Arc::new(QualifiedWorkRecord {
            scope: scope.clone(),
            key: self.record.key(),
            revision,
            content_digest: self.digest(),
            resident_bytes: serde_json::to_vec(&self.record)?.len(),
            record: self.record.clone(),
        });
        result.validate(scope)?;
        Ok(result)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    revision: u64,
    availability: WorkAvailability,
    digest: Option<String>,
    fingerprint: Option<Fingerprint>,
}

fn family_name(family: WorkFamily) -> &'static str {
    match family {
        WorkFamily::Work => "work",
        WorkFamily::Wait => "wait",
        WorkFamily::Frame => "frame",
    }
}
fn family_table(family: WorkFamily) -> (&'static str, &'static str) {
    match family {
        WorkFamily::Work => ("work_items", "work_item_id"),
        WorkFamily::Wait => ("wait_conditions", "wait_condition_id"),
        WorkFamily::Frame => ("work_item_continuations", "continuation_id"),
    }
}
fn availability_name(value: WorkAvailability) -> &'static str {
    match value {
        WorkAvailability::Present => "present",
        WorkAvailability::Absent => "absent",
        WorkAvailability::Pruned => "pruned",
    }
}
fn parse_availability(value: &str) -> Result<WorkAvailability> {
    match value {
        "present" => Ok(WorkAvailability::Present),
        "absent" => Ok(WorkAvailability::Absent),
        "pruned" => Ok(WorkAvailability::Pruned),
        _ => anyhow::bail!("invalid Work availability"),
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bound_metadata(count: usize, bytes: usize) -> Result<()> {
    if count > MAX_METADATA_RECORDS || bytes > MAX_METADATA_BYTES {
        return Err(
            WorkSnapshotError::ResourceExhausted(if count > MAX_METADATA_RECORDS {
                "metadata count"
            } else {
                "metadata bytes"
            })
            .into(),
        );
    }
    Ok(())
}
fn binding(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<Option<String>> {
    let (mode, seed): (String, Option<String>) = tx.query_row(
        "SELECT mode,seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",
        params![scope.agent_id,i64::try_from(scope.identity_incarnation)?], |row| Ok((row.get(0)?,row.get(1)?)))?;
    ensure!(
        (mode == "fresh_empty" && seed.is_none()) || (mode == "legacy_adopted" && seed.is_some()),
        "invalid Work continuity binding"
    );
    if let Some(seed_id) = &seed {
        let sealed: bool = tx.query_row("SELECT NOT import_required OR EXISTS(SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id=s.seed_id) FROM episode_continuity_seeds s WHERE seed_id=?1", [seed_id], |r|r.get(0))?;
        if !sealed {
            return Err(WorkSnapshotError::LegacyImportPending.into());
        }
    }
    Ok(seed)
}
fn scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    original_recovery: bool,
) -> Result<Option<ContextScopeFacts>> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Work scope"
    );
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    if let Some(identity) = &facts.identity {
        let status: String = tx.query_row(
            "SELECT status FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |row| row.get(0),
        )?;
        ensure!(
            identity.agent_id == scope.agent_id
                && identity.incarnation > 0
                && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
            "Work identity normalized/payload projection mismatch"
        );
    }
    let current = facts.identity.as_ref().is_some_and(|identity| {
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    });
    Ok(current.then_some(facts))
}
fn alias(tx: &Transaction<'_>, scope: &InputEvidenceScope, key: &WorkKey) -> Result<Option<Alias>> {
    let row: Option<(i64,String,Option<String>,Option<String>)> = tx.query_row(
        "SELECT revision,availability,content_digest,fingerprint_json FROM work_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
        params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,family_name(key.family),key.id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    row.map(|(revision, availability, digest, fingerprint)| {
        Ok(Alias {
            revision: u64::try_from(revision)?,
            availability: parse_availability(&availability)?,
            digest,
            fingerprint: fingerprint.map(|s| serde_json::from_str(&s)).transpose()?,
        })
    })
    .transpose()
}
fn upsert_alias(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &WorkKey,
    alias: &Alias,
) -> Result<()> {
    tx.execute("INSERT INTO work_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,fingerprint_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,fingerprint_json=excluded.fingerprint_json",
        params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,family_name(key.family),key.id,i64::try_from(alias.revision)?,availability_name(alias.availability),alias.digest,
            alias.fingerprint.as_ref().map(serde_json::to_string).transpose()?])?;
    Ok(())
}
fn stored_on(tx: &Transaction<'_>, agent: &str, key: &WorkKey) -> Result<Option<StoredRecord>> {
    let (table, id) = family_table(key.family);
    let row: Option<(String, String)> = tx
        .query_row(
            &format!("SELECT agent_id,payload_json FROM {table} WHERE {id}=?1"),
            [&key.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((stored_agent, payload)) = row else {
        return Ok(None);
    };
    ensure!(
        stored_agent == agent,
        "Work canonical ID belongs to another agent"
    );
    let record = match key.family {
        WorkFamily::Work => CanonicalWorkRecord::Work(repositories::decode_work_item_payload(&payload)?),
        WorkFamily::Wait => CanonicalWorkRecord::Wait(tx.query_row(
            "SELECT wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,wake_sources_json,continuation_json FROM wait_conditions WHERE wait_condition_id=?1", [&key.id],
            |row|repositories::decode_wait_condition_row(row).map_err(|error|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,format!("{error:#}").into())))?),
        WorkFamily::Frame => CanonicalWorkRecord::Frame(repositories::decode_work_item_continuation_payload(&payload)?),
    };
    ensure!(
        record.agent_id() == agent && record.key() == *key,
        "Work canonical payload scope/key mismatch"
    );
    let payload_record = match key.family {
        WorkFamily::Wait => {
            CanonicalWorkRecord::Wait(repositories::decode_wait_condition_payload(&payload)?)
        }
        _ => record.clone(),
    };
    let (projection_sql, expected) = projection(&payload_record)?;
    let projection: String = tx.query_row(
        &format!("SELECT {projection_sql} FROM {table} WHERE {id}=?1"),
        [&key.id],
        |r| r.get(0),
    )?;
    ensure!(
        normalized_projection(&payload_record, &projection)? == expected,
        "Work normalized/payload projection mismatch: {:?}",
        key
    );
    Ok(Some(StoredRecord {
        fingerprint: Fingerprint {
            projection,
            payload_bytes: payload.len(),
        },
        record,
        payload,
    }))
}
fn stamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn optional_stamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(stamp)
}
fn enum_value<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}
fn projection(record: &CanonicalWorkRecord) -> Result<(&'static str, serde_json::Value)> {
    use serde_json::json;
    Ok(match record {
        CanonicalWorkRecord::Work(x) => ("json_array(work_item_id,agent_id,state,revision,created_at,updated_at)",
            json!([x.id,x.agent_id,enum_value(&x.state)?,x.revision,stamp(x.created_at),stamp(x.updated_at)])),
        CanonicalWorkRecord::Wait(x) => ("json_array(wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,json(wake_sources_json),json(continuation_json))",
            json!([x.id,x.agent_id,x.work_item_id,enum_value(&x.status)?,enum_value(&x.kind)?,x.source,x.subject_ref,x.waiting_for,stamp(x.created_at),stamp(x.updated_at),optional_stamp(x.expires_at),optional_stamp(x.resolved_at),optional_stamp(x.cancelled_at),x.turn_id,x.trigger_message_id,optional_stamp(x.triggered_at),x.wake_sources,x.continuation])),
        CanonicalWorkRecord::Frame(x) => ("json_array(continuation_id,agent_id,suspended_work_item_id,active_work_item_id,return_policy,state,created_at,updated_at,resolved_at,cancelled_at,resolution_reason,last_turn_id)",
            json!([x.id,x.agent_id,x.suspended_work_item_id,x.active_work_item_id,enum_value(&x.return_policy)?,enum_value(&x.state)?,stamp(x.created_at),stamp(x.updated_at),optional_stamp(x.resolved_at),optional_stamp(x.cancelled_at),x.resolution_reason,x.turn_id])),
    })
}

fn normalized_projection(record: &CanonicalWorkRecord, json: &str) -> Result<serde_json::Value> {
    let mut actual: serde_json::Value = serde_json::from_str(json)?;
    let indices: &[usize] = match record {
        CanonicalWorkRecord::Work(_) => &[4, 5],
        CanonicalWorkRecord::Wait(_) => &[8, 9, 10, 11, 12, 15],
        CanonicalWorkRecord::Frame(_) => &[6, 7, 8, 9],
    };
    for index in indices {
        if let Some(value) = actual[*index].as_str() {
            actual[*index] = serde_json::Value::String(stamp(
                chrono::DateTime::parse_from_rfc3339(value)?.with_timezone(&chrono::Utc),
            ));
        }
    }
    Ok(actual)
}
