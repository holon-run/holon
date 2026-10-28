//! Exact Task/result qualification. Readers cannot adopt canonical history.
use super::{
    context_inputs::{ContextDurableRead, ContextScopeFacts},
    repositories,
};
use crate::{
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
        resident_task::*,
    },
    types::{AgentRegistryStatus, AgentState},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
mod deletion_maintenance;
mod loader;
pub(in crate::runtime_db) use loader::load_selected_using_material;
mod maintenance;
mod original_cleanup;
mod original_result_predicate;
pub(in crate::runtime_db) mod qualification;
mod qualified_input_read;
mod query_sources;
mod recovery_sources;
mod running_sources;
#[cfg(test)]
mod running_sources_tests;
pub(in crate::runtime_db) use qualified_input_read::TaskInputReadAuthorization;
mod result_routing;
#[cfg(test)]
mod result_routing_tests;
pub(crate) use result_routing::{prepare_task_result_parent_read_tx, TaskResultParentRead};
mod output_read;
#[cfg(test)]
mod output_read_tests;
pub(in crate::runtime_db) use output_read::TaskOutputReadAuthorization;
pub(crate) use output_read::{
    prepare_task_output_input_batch_tx, prepare_task_output_input_recall_tx,
};
pub(in crate::runtime_db) mod monitor_relations;
#[cfg(test)]
#[path = "task_snapshot/monitor_relations_tests.rs"]
mod monitor_relations_tests;
#[cfg(test)]
#[path = "task_snapshot/query_relations_tests.rs"]
mod query_relation_tests;
mod query_relations;
mod source_writes;
mod status_control_source;
mod status_delegation_source;
pub(in crate::runtime_db) mod status_metadata;
mod status_metadata_recovery;
mod status_observation;
mod status_upgrade;
pub(in crate::runtime_db) use query_sources::register_message_source_tx;
pub(crate) use source_writes::register_task_source_metadata_tx;
pub(in crate::runtime_db) use status_control_source::register_control_worktree_completion_tx;
pub(in crate::runtime_db) use status_delegation_source::register_delegation_write_tx;
pub(in crate::runtime_db) use status_metadata::refresh_task_status_metadata_tx;
pub(in crate::runtime_db) use status_metadata_recovery::read_status_metadata_using_material_tx as read_child_status_metadata_using_material_tx;
pub(crate) use status_metadata_recovery::read_task_status_metadata_recovery_tx;
pub(in crate::runtime_db) use status_observation::{
    observe_tx as observe_task_status_headers_tx, sync_child_observers_tx,
};
pub(in crate::runtime_db) use status_upgrade::capture_status_sources_upgrade_tx;
pub(in crate::runtime_db) mod read_preparation;
mod result_relation;
mod seed_maintenance;
mod startup_metadata;
mod stop_observation;
mod task_stop_readback;
#[cfg(test)]
mod tests;
mod writes;
pub(crate) use deletion_maintenance::DeletionTaskAck;
#[cfg(test)]
pub(crate) use loader::{
    load_task_domain_snapshot_tx, prepare_task_domain_snapshot_tx,
    prepare_task_output_snapshot_with_budget_tx, validate_task_domain_observation_tx,
};
pub(crate) use loader::{
    preflight_named_task_snapshot_tx, prepare_original_task_recovery_snapshot_with_budget_tx,
    prepare_task_domain_snapshot_with_budget_tx, validate_task_domain_observation_with_scope_tx,
};
pub(crate) use maintenance::{
    prepare_existing_task_maintenance_tx, register_existing_task_maintenance_tx,
    task_maintenance_material_tx, ExistingTaskMaintenanceProof, TaskMaintenanceAck,
    TaskMaintenanceMaterial,
};
pub(crate) use original_cleanup::{
    original_task_result_cleanup_material_tx, prepare_original_task_result_cleanup_tx,
    register_original_task_result_cleanup_tx, OriginalTaskResultCleanupProof,
    OriginalTaskResultCleanupSource,
};
pub(crate) use original_result_predicate::{
    prepare_original_result_predicate_tx, OriginalResultPredicateProof,
    OriginalResultPredicateRetention,
};
pub(super) use qualification::{
    capture_task_domain_seed_if_ready_tx, capture_task_domain_upgrade_tx,
};
pub(crate) use read_preparation::qualified_task_recovery_selectors_tx;
pub(in crate::runtime_db) use recovery_sources::prepare_settled_recovery_source_material_tx;
pub(in crate::runtime_db) use result_relation::{
    checkpoint_result_dependency_on, prepare_pending_task_result_dependency_using_material_tx,
    TaskCheckpointDependency,
};
pub(crate) use result_relation::{
    prepare_pending_task_result_dependency_tx, PreparedTaskResultDependency,
};
#[cfg(test)]
pub(crate) use seed_maintenance::{
    prepare_unbound_task_maintenance_tx, qualified_unbound_task_maintenance_keys_tx,
    register_unbound_task_maintenance_tx, unbound_task_maintenance_material_tx,
};
pub(crate) use startup_metadata::prepare_exclusive_startup_task_metadata_tx;
pub(crate) use task_stop_readback::prepare_task_sql_readback_tx;
pub(in crate::runtime_db) use writes::register_original_task_writes_using_material_tx;
#[cfg(test)]
pub(crate) use writes::{
    preflight_task_write_snapshot_tx, register_task_writes_tx, CommittedTaskFacts,
};
pub(crate) use writes::{
    preflight_task_write_snapshot_with_budget_tx,
    preflight_task_write_snapshot_with_memory_budget_tx, prepare_original_task_cleanup_tx,
    prepare_task_write_tx, register_task_writes_with_budget_tx,
    register_task_writes_with_memory_budget_tx, register_task_writes_with_stop_observation_tx,
    register_task_writes_with_stop_result_observation_tx, TaskWriteProof,
};
mod stop_progress;
pub(crate) use stop_progress::invalidate_task_scope_for_stop_progress_tx;

pub(super) const TASK_DOMAIN_VERSION: i64 = 98;
pub(super) const TASK_DOMAIN_SCHEMA: &str = include_str!("task_snapshot/schema.sql");
const MAX_RECORDS: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskSnapshotError {
    #[error("Task domain needs preparation: {0:?}")]
    NeedsPreparation(TaskKey),
    #[error("Task canonical body/projection changed outside original transaction: {0:?}")]
    CanonicalChanged(TaskKey),
    #[error("Task ID belongs to another agent")]
    ForeignRecord(TaskKey),
    #[error("Task scope retired or has no State")]
    RetiredScope,
    #[error("Task legacy import is not sealed")]
    LegacyImportPending,
    #[error("Task output sources require original qualified preparation")]
    OutputSourcesIncomplete,
    #[error("Task domain revision mismatch: expected {expected}, actual {actual:?}")]
    RevisionMismatch { expected: u64, actual: Option<u64> },
    #[error("Task domain ResourceExhausted: {0}")]
    ResourceExhausted(&'static str),
}
#[derive(Debug, Clone)]
pub(crate) struct TaskDomainLoadRequest {
    pub query: crate::runtime::resident_task::TaskQuerySelection,
    pub capture: TaskCaptureRequest,
    /// Frozen scheduler time required by nonempty recovery selectors.
    pub recovery_at: Option<chrono::DateTime<chrono::Utc>>,
    pub record_limit: usize,
    pub byte_limit: usize,
}
impl TaskDomainLoadRequest {
    /// One read phase includes retained selections and all later SQL material.
    pub(crate) fn bounded_material_limits(records: usize, bytes: usize) -> (usize, usize) {
        (records.min(MAX_RECORDS), bytes.min(MAX_BYTES))
    }
}
#[derive(Debug)]
pub(crate) struct TaskCurrentSnapshot {
    pub facts: ContextScopeFacts,
    pub resident: ResidentTaskDomain,
    pub input_roots: BTreeSet<InputEvidenceKey>,
    /// Test observation only. Absence alone cannot prove turnless provenance;
    /// without an original producer proof every parent remains an Input root.
    #[cfg(test)]
    pub turnless_parent_turns: BTreeSet<InputEvidenceKey>,
    pub work_roots: BTreeSet<String>,
}
#[derive(Debug)]
pub(crate) enum TaskBootstrapSnapshot {
    Current(Box<TaskCurrentSnapshot>),
    Retired { scope: InputEvidenceScope },
}
impl TaskBootstrapSnapshot {
    pub(crate) fn require_current(self) -> Result<TaskCurrentSnapshot> {
        match self {
            Self::Current(x) => Ok(*x),
            Self::Retired { scope } => Err(TaskSnapshotError::RetiredScope).with_context(|| {
                format!(
                    "Task scope {} incarnation {} is retired",
                    scope.agent_id, scope.identity_incarnation
                )
            }),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
struct StoredTask {
    fact: CanonicalTaskFact,
    payload: String,
    projection_digest: String,
}
impl StoredTask {
    fn digest(&self) -> String {
        digest(self.payload.as_bytes())
    }
    fn qualified(
        &self,
        scope: &InputEvidenceScope,
        revision: u64,
    ) -> Result<Arc<QualifiedTaskFact>> {
        let fact = Arc::new(QualifiedTaskFact {
            locator: TaskFactLocator {
                scope: scope.clone(),
                key: self.fact.key(),
                revision,
                content_digest: self.digest(),
                projection_digest: self.projection_digest.clone(),
            },
            resident_bytes: usize::try_from(
                crate::runtime::input_evidence_budget::serialized_bytes(&self.fact)?,
            )?,
            fact: self.fact.clone(),
        });
        fact.validate(scope)?;
        Ok(fact)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    revision: u64,
    availability: TaskAvailability,
    digest: Option<String>,
    projection_digest: Option<String>,
}
fn digest(x: &[u8]) -> String {
    format!("{:x}", Sha256::digest(x))
}
fn family_name(x: TaskFamily) -> &'static str {
    match x {
        TaskFamily::Task => "task",
        TaskFamily::ResultSettlement => "result_settlement",
    }
}
fn family_table(x: TaskFamily) -> (&'static str, &'static str, &'static str) {
    match x {
        TaskFamily::Task => ("tasks", "task_id", "owner_agent_id"),
        TaskFamily::ResultSettlement => ("task_result_settlements", "result_identity", "agent_id"),
    }
}
fn availability_name(x: TaskAvailability) -> &'static str {
    match x {
        TaskAvailability::Present => "present",
        TaskAvailability::Absent => "absent",
        TaskAvailability::Pruned => "pruned",
    }
}
fn parse_availability(x: &str) -> Result<TaskAvailability> {
    match x {
        "present" => Ok(TaskAvailability::Present),
        "absent" => Ok(TaskAvailability::Absent),
        "pruned" => Ok(TaskAvailability::Pruned),
        _ => anyhow::bail!("invalid Task availability"),
    }
}
fn bound(count: usize, bytes: usize, request: Option<&TaskDomainLoadRequest>) -> Result<()> {
    let (max_records, max_bytes) = request.map_or((MAX_RECORDS, MAX_BYTES), |r| {
        (r.record_limit.min(MAX_RECORDS), r.byte_limit.min(MAX_BYTES))
    });
    ensure!(
        max_records > 0 && max_bytes > 0,
        "zero Task acquisition limit"
    );
    if count > max_records || bytes > max_bytes {
        return Err(
            TaskSnapshotError::ResourceExhausted(if count > max_records {
                "records"
            } else {
                "bytes"
            })
            .into(),
        );
    }
    Ok(())
}
fn scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    original_recovery: bool,
) -> Result<Option<ContextScopeFacts>> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Task scope"
    );
    // Inspect identity first: a retired predecessor never reads successor State.
    let identity: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((agent, status, payload)) = identity else {
        return Ok(None);
    };
    let identity = super::repositories::decode_agent_identity_payload(&payload)?;
    ensure!(
        agent == scope.agent_id
            && identity.agent_id == agent
            && identity.incarnation > 0
            && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
        "Task identity projection mismatch"
    );
    if identity.incarnation != scope.identity_incarnation
        || !(identity.status == AgentRegistryStatus::Active
            || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    {
        return Ok(None);
    }
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    ensure!(
        facts.identity.as_ref() == Some(&identity),
        "Task identity read-view mismatch"
    );
    if facts.state.is_none() {
        return Ok(None);
    }
    Ok(Some(facts))
}
fn binding(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<Option<String>> {
    let (mode,seed):(String,Option<String>)=tx.query_row("SELECT mode,seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        (mode == "fresh_empty" && seed.is_none()) || (mode == "legacy_adopted" && seed.is_some()),
        "invalid Task continuity binding"
    );
    if let Some(seed) = &seed {
        let sealed:bool=tx.query_row("SELECT NOT import_required OR EXISTS(SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id=s.seed_id) FROM episode_continuity_seeds s WHERE seed_id=?1",[seed],|r|r.get(0))?;
        if !sealed {
            return Err(TaskSnapshotError::LegacyImportPending.into());
        }
    }
    Ok(seed)
}
fn alias(tx: &Transaction<'_>, scope: &InputEvidenceScope, key: &TaskKey) -> Result<Option<Alias>> {
    let row:Option<(u64,String,Option<String>,Option<String>)>=tx.query_row("SELECT revision,availability,content_digest,projection_digest FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    row.map(|(revision, availability, digest, projection_digest)| {
        Ok(Alias {
            revision,
            availability: parse_availability(&availability)?,
            digest,
            projection_digest,
        })
    })
    .transpose()
}
fn upsert_alias(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &TaskKey,
    value: &Alias,
) -> Result<()> {
    tx.execute("INSERT INTO task_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id,value.revision,availability_name(value.availability),value.digest,value.projection_digest])?;
    Ok(())
}
fn stored_on(connection: &Connection, agent: &str, key: &TaskKey) -> Result<Option<StoredTask>> {
    let (table, id, owner) = family_table(key.family);
    let material: Option<usize> = connection.query_row(
        &format!("SELECT length(CAST(payload_json AS BLOB))+length(CAST({} AS BLOB)) FROM {table} WHERE {id}=?1", projection_sql(connection, key.family)?),
        [&key.id], |row| row.get(0),
    ).optional()?;
    bound(1, material.unwrap_or_default(), None)?;
    let raw: Option<(String, String)> = connection
        .query_row(
            &format!("SELECT {owner},payload_json FROM {table} WHERE {id}=?1"),
            [&key.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((stored_agent, payload)) = raw else {
        return Ok(None);
    };
    ensure!(
        stored_agent == agent,
        TaskSnapshotError::ForeignRecord(key.clone())
    );
    let (fact, sql, expected) = match key.family {
        TaskFamily::Task => {
            let x = repositories::decode_task_payload(&payload)?;
            let child = repositories::task_detail_string(&x.detail, "child_agent_id");
            let expected = serde_json::json!([
                x.id,
                x.agent_id,
                child.as_ref().map(|_| x.agent_id.clone()),
                child,
                x.kind.as_str(),
                serde_json::to_value(&x.status)?,
                x.summary,
                repositories::task_detail_string(&x.detail, "input_target"),
                serde_json::to_value(x.wait_policy())?,
                repositories::task_detail_string(&x.detail, "output_path"),
                repositories::task_detail_string(&x.detail, "output_summary"),
                repositories::task_detail_i64(&x.detail, "exit_status"),
                i64::from(x.terminal_reentry()),
                repositories::task_revision(&x),
                repositories::timestamp(x.created_at),
                repositories::timestamp(x.updated_at),
                repositories::is_terminal_task_status(&x.status)
                    .then(|| repositories::timestamp(x.updated_at)),
                x.parent_message_id,
            ]);
            (
                CanonicalTaskFact::Task(x),
                projection_sql(connection, TaskFamily::Task)?,
                expected,
            )
        }
        TaskFamily::ResultSettlement => {
            let x: super::task_result_settlement::TaskResultSettlementRecord =
                serde_json::from_str(&payload)?;
            let mut expected = serde_json::json!([
                x.result_identity,
                x.agent_id,
                x.task_id,
                x.message_id,
                x.work_item_id,
                x.rejoin_generation,
                x.source.parent_turn_id().unwrap_or(""),
                serde_json::to_value(x.state)?,
                x.activation_id,
                serde_json::to_value(x.disposition)?,
                repositories::timestamp(x.created_at),
                repositories::timestamp(x.updated_at),
                x.admitted_at.map(repositories::timestamp),
                x.settled_at.map(repositories::timestamp),
                x.deferred_reason,
                x.deferred_at.map(repositories::timestamp),
                x.next_recheck_at.map(repositories::timestamp)
            ]);
            if let Some(id) = x.source.command_creation_id() {
                expected
                    .as_array_mut()
                    .context("Result projection is not an array")?
                    .push(serde_json::json!(id));
            }
            (
                CanonicalTaskFact::ResultSettlement(x),
                projection_sql(connection, TaskFamily::ResultSettlement)?,
                expected,
            )
        }
    };
    ensure!(
        fact.agent_id() == agent && fact.key() == *key,
        "Task payload scope/key mismatch"
    );
    let actual: String = connection.query_row(
        &format!("SELECT {sql} FROM {table} WHERE {id}=?1"),
        [&key.id],
        |r| r.get(0),
    )?;
    ensure!(
        normalized_projection(&actual, key.family)? == expected,
        "Task normalized/payload projection mismatch: {:?}",
        key
    );
    Ok(Some(StoredTask {
        fact,
        payload,
        projection_digest: digest(actual.as_bytes()),
    }))
}
fn projection_sql(connection: &Connection, family: TaskFamily) -> Result<&'static str> {
    if family == TaskFamily::ResultSettlement
        && !super::command_task_creation::has_result_source_column(connection)?
    {
        // Migration 96 captures ParentTurn projections before migration 100.
        // CommandCreation payloads still fail the expected projection here.
        return Ok("json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at)");
    }
    Ok(match family {
        TaskFamily::Task => "json_array(task_id,owner_agent_id,parent_agent_id,child_agent_id,kind,status,summary,input_target,wait_policy,output_path,result_summary,exit_status,terminal_reentry,revision,created_at,updated_at,completed_at,last_message_id,last_turn_id,causation_id,correlation_id)",
        TaskFamily::ResultSettlement => "CASE WHEN command_creation_id IS NULL THEN json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at) ELSE json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at,command_creation_id) END",
    })
}
fn normalized_projection(raw: &str, family: TaskFamily) -> Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(raw)?;
    let positions: &[usize] = match family {
        TaskFamily::Task => &[14, 15, 16],
        TaskFamily::ResultSettlement => &[10, 11, 12, 13, 15, 16],
    };
    let fields = value
        .as_array_mut()
        .context("Task projection is not an array")?;
    for &position in positions {
        if let Some(text) = fields.get(position).and_then(serde_json::Value::as_str) {
            let time = chrono::DateTime::parse_from_rfc3339(text)?;
            fields[position] = serde_json::Value::String(repositories::timestamp(
                time.with_timezone(&chrono::Utc),
            ));
        }
    }
    if family == TaskFamily::Task {
        // These normalized columns retain the original full detail strings;
        // the canonical payload deliberately stores their slim representation.
        for position in [3, 7, 9, 10] {
            if let Some(text) = fields.get(position).and_then(serde_json::Value::as_str) {
                fields[position] =
                    serde_json::Value::String(repositories::truncate_task_payload_string(text));
            }
        }
        // The original writer preserves these three opaque SQL-only columns.
        // They are included in the exact projection digest, not invented in TaskRecord.
        fields.truncate(18);
    }
    Ok(value)
}
pub(in crate::runtime_db) fn checkpoint_material_size_on(
    connection: &Connection,
    key: &TaskKey,
) -> Result<Option<usize>> {
    let (table, id, _) = family_table(key.family);
    connection.query_row(
        &format!("SELECT length(CAST(payload_json AS BLOB))+length(CAST({} AS BLOB)) FROM {table} WHERE {id}=?1", projection_sql(connection, key.family)?),
        [&key.id], |row| row.get(0),
    ).optional().map_err(Into::into)
}
fn lengths(
    connection: &Connection,
    keys: &BTreeSet<TaskKey>,
    request: Option<&TaskDomainLoadRequest>,
) -> Result<()> {
    let mut bytes = 0usize;
    for key in keys {
        let (table, id, _) = family_table(key.family);
        let n: Option<usize> = connection
            .query_row(
                &format!("SELECT length(CAST(payload_json AS BLOB))+length(CAST({} AS BLOB)) FROM {table} WHERE {id}=?1", projection_sql(connection, key.family)?),
                [&key.id],
                |r| r.get(0),
            )
            .optional()?;
        bytes = bytes
            .checked_add(n.unwrap_or_default())
            .and_then(|n| n.checked_add(key.id.len() + 128))
            .context("Task read material overflow")?;
        bound(keys.len(), bytes, request)?;
    }
    Ok(())
}
pub(in crate::runtime_db) fn checkpoint_fact_on(
    connection: &Connection,
    agent: &str,
    key: &TaskKey,
) -> Result<Option<(CanonicalTaskFact, String, String)>> {
    Ok(stored_on(connection, agent, key)?.map(|x| {
        let digest = x.digest();
        (x.fact, digest, x.projection_digest)
    }))
}

#[cfg(test)]
#[path = "task_snapshot/query_fixed_tests.rs"]
mod query_fixed_tests;
