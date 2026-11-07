//! Original unloaded deletion jobs may maintain existing Task qualification only.
use super::*;
use crate::{
    runtime::input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    runtime_db::RuntimeDb,
    types::{
        AgentDeletionJob, AgentDeletionMode, AgentDeletionPhase, AgentDeletionStatus,
        AgentIdentityRecord, TaskStatus,
    },
};
use chrono::{DateTime, Utc};

const BATCH: usize = 128;
const ACTIVE: &str = "status IN ('queued','running','cancelling')";
const JOB_MATERIAL: &str = "length(CAST(payload_json AS BLOB))+length(CAST(deletion_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(phase AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(completed_at AS BLOB)),0)";

/// Only IDs; no body, resident root, or permission to create a Task.
pub(crate) struct DeletionTaskBatch {
    pub(crate) ids: Vec<String>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
/// Retained through the original metadata ACK/publication, then immediately dropped.
pub(crate) struct DeletionTaskAck {
    pub(crate) changed: bool,
    pub(crate) guards: Vec<Arc<BudgetViewGuard>>,
}
fn charge(
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    material: TaskMaintenanceMaterial,
) -> Result<Arc<BudgetViewGuard>> {
    let limits = ledger.limits();
    let request = TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        byte_limit: usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("deletion Task limits overflow")?,
        )?
        .min(MAX_BYTES),
        record_limit: usize::try_from(limits.record_limit)?.min(MAX_RECORDS),
    };
    bound(material.records, material.bytes, Some(&request))?;
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(material.bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?)
}
fn job_material(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    job: &AgentDeletionJob,
) -> Result<TaskMaintenanceMaterial> {
    let bytes:usize=tx.query_row(&format!("SELECT COALESCE((SELECT {JOB_MATERIAL} FROM agent_deletion_jobs WHERE deletion_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?2),0)"),
        params![job.deletion_id,scope.agent_id],|r|r.get(0))?;
    Ok(TaskMaintenanceMaterial {
        bytes: bytes
            .checked_add(scope.agent_id.len() + job.deletion_id.len() + 1024)
            .and_then(|n| n.checked_mul(8))
            .context("deletion Task job material overflow")?,
        records: 16,
    })
}
fn validate_job(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    expected: &AgentDeletionJob,
) -> Result<AgentIdentityRecord> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid deletion Task scope"
    );
    let (id,agent,status,phase,raw):(String,String,String,String,String)=tx.query_row(
        "SELECT deletion_id,agent_id,status,phase,payload_json FROM agent_deletion_jobs WHERE deletion_id=?1",
        [&expected.deletion_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let actual: AgentDeletionJob = serde_json::from_str(&raw)?;
    ensure!(
        actual == *expected
            && id == actual.deletion_id
            && agent == actual.agent_id
            && agent == scope.agent_id
            && status == "running"
            && actual.status == AgentDeletionStatus::Running
            && phase == "quiesce"
            && actual.phase == AgentDeletionPhase::Quiesce
            && actual.mode != AgentDeletionMode::ParentCleanup,
        "original deletion Task job changed or is not Quiesce"
    );
    let (agent, status, raw): (String, String, String) = tx.query_row(
        "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let identity = repositories::decode_agent_identity_payload(&raw)?;
    ensure!(
        agent == scope.agent_id
            && identity.agent_id == agent
            && identity.incarnation == scope.identity_incarnation
            && identity.incarnation > 0
            && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
        "deletion Task identity/incarnation differs"
    );
    ensure!(
        matches!(
            (&identity.status, &actual.mode),
            (AgentRegistryStatus::Deleting, AgentDeletionMode::Delete)
                | (
                    AgentRegistryStatus::Deleted,
                    AgentDeletionMode::CleanupRepair
                )
        ),
        "deletion Task job has no retired cleanup authority"
    );
    Ok(identity)
}
impl RuntimeDb {
    /// Scope discovery is metadata only. The guard also covers the original
    /// metadata ACK's later identity/State read, before that decoder is entered.
    pub(crate) fn deletion_task_scope_with_budget(
        &self,
        agent_id: &str,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(InputEvidenceScope, Arc<BudgetViewGuard>)> {
        self.transaction(|tx| {
            let raw_bytes: usize=tx.query_row(
                "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",
                [agent_id], |row|row.get(0))?;
            let bytes=raw_bytes.checked_add(agent_id.len()+512).and_then(|n|n.checked_mul(8)).context("deletion Task scope material overflow")?;
            let limits=ledger.limits();
            let byte_limit=usize::try_from(limits.history_per_agent_bytes.checked_add(limits.pending_per_agent_bytes).context("deletion Task scope limits overflow")?)?.min(MAX_BYTES);
            ensure!(bytes<=byte_limit && 16<=limits.record_limit, TaskSnapshotError::ResourceExhausted("deletion Task scope header"));
            // Bound first: even malformed JSON must not beat the resource error.
            // Only a scalar is returned; no identity/State body is decoded here.
            let incarnation: Option<u64>=tx.query_row(
                "SELECT CASE WHEN json_valid(payload_json) THEN CASE WHEN json_extract(payload_json,'$.agent_id')=agent_id AND json_extract(payload_json,'$.status')=status THEN CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END END END FROM agent_identities WHERE agent_id=?1", [agent_id], |row|row.get(0))?;
            let scope=InputEvidenceScope { agent_id:agent_id.into(), identity_incarnation:incarnation.context("deletion Task scope header is invalid")? };
            let guard=charge(&scope,ledger,TaskMaintenanceMaterial { bytes,records:16 })?;
            Ok((scope,guard))
        })
    }
    pub(crate) fn deletion_task_batch(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<DeletionTaskBatch> {
        self.transaction(|tx| {
            let guard=charge(scope,ledger,job_material(tx,scope,job)?)?;
            validate_job(tx,scope,job)?;
            let (bytes,count):(usize,usize)=tx.query_row(&format!("SELECT COALESCE(SUM(length(CAST(task_id AS BLOB))+128),0),COUNT(*) FROM (SELECT task_id FROM tasks WHERE owner_agent_id=?1 AND {ACTIVE} ORDER BY task_id LIMIT {BATCH})"),
                [&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let ids_guard=charge(scope,ledger,TaskMaintenanceMaterial {
                bytes:bytes.checked_mul(4).context("deletion Task IDs overflow")?,
                records:count.checked_mul(4).context("deletion Task count overflow")?,
            })?;
            let mut query=tx.prepare(&format!("SELECT task_id FROM tasks WHERE owner_agent_id=?1 AND {ACTIVE} ORDER BY task_id LIMIT {BATCH}"))?;
            let ids=query.query_map([&scope.agent_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(DeletionTaskBatch { ids, _guards:vec![guard,ids_guard] })
        })
    }
    /// The original raw cancellation remains one transaction per Task. There is
    /// no State creation, Ready snapshot, new Result, or new qualification.
    pub(crate) fn cancel_task_for_deletion(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        task_id: &str,
        now: DateTime<Utc>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(DeletionTaskAck, AgentIdentityRecord)> {
        self.transaction(|tx| {
            let mut guards = vec![charge(scope, ledger, job_material(tx, scope, job)?)?];
            guards.push(charge(
                scope,
                ledger,
                TaskMaintenanceMaterial {
                    bytes: task_id
                        .len()
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(4))
                        .context("deletion Task address overflow")?,
                    records: 4,
                },
            )?);
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, task_id)]);
            guards.push(charge(
                scope,
                ledger,
                task_maintenance_material_tx(tx, scope, &keys)?,
            )?);
            let identity = validate_job(tx, scope, job)?;
            let proof = if identity.status == AgentRegistryStatus::Deleting {
                Some(prepare_existing_task_maintenance_tx(tx, scope, &keys)?)
            } else {
                None
            };
            let before = match &proof {
                Some(proof) => proof.before[&TaskKey::new(TaskFamily::Task, task_id)]
                    .before
                    .clone(),
                None => stored_on(
                    tx,
                    &scope.agent_id,
                    &TaskKey::new(TaskFamily::Task, task_id),
                )?,
            };
            let mut changed = false;
            if let Some(before) = before {
                let CanonicalTaskFact::Task(mut task) = before.fact else {
                    unreachable!()
                };
                if repositories::is_active_task_status(&task.status) {
                    task.status = TaskStatus::Cancelled;
                    task.updated_at = now;
                    changed = repositories::upsert_task_tx(tx, &task)?;
                    ensure!(
                        changed,
                        "original deletion Task cancellation did not advance its active row"
                    );
                }
            }
            if let Some(proof) = proof {
                let actual = if changed { keys } else { BTreeSet::new() };
                register_existing_task_maintenance_tx(tx, proof, &actual)?;
            }
            // Deleted CleanupRepair deliberately leaves the retired aliases and
            // namespace untouched. It has no path to successor qualification.
            Ok((DeletionTaskAck { changed, guards }, identity))
        })
    }
}
