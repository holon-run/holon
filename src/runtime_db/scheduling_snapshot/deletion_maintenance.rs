//! Original unloaded deletion jobs maintain existing Scheduling rows only.
use super::*;
use crate::{
    runtime_db::RuntimeDb,
    types::{
        AgentDeletionJob, AgentDeletionMode, AgentDeletionPhase, AgentDeletionStatus,
        AgentIdentityRecord, ExternalTriggerStatus, TimerStatus,
    },
};
use chrono::{DateTime, Utc};
const BATCH: usize = 128;
const JOB_BYTES: &str = "length(CAST(payload_json AS BLOB))+length(CAST(deletion_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(phase AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(completed_at AS BLOB)),0)";
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionSchedulingSelection {
    Timers,
    ExternalTriggers,
}
pub(crate) struct DeletionSchedulingBatch {
    pub ids: Vec<String>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
pub(crate) struct DeletionSchedulingAck {
    pub changed: usize,
    pub qualified_actual_keys: BTreeSet<SchedulingKey>,
    pub invalidation: Option<SchedulingScopeInvalidation>,
    pub guards: Vec<Arc<BudgetViewGuard>>,
}
#[derive(Clone, Copy)]
enum Mutation<'a> {
    Timer(&'a str),
    ParentTimers,
    Trigger(&'a str),
}
fn request(ledger: &InputEvidenceBudgetLedger) -> Result<SchedulingDomainLoadRequest> {
    let limits = ledger.limits();
    Ok(SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: usize::try_from(limits.record_limit)?.min(MAX_RECORDS),
        byte_limit: usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("deletion Scheduling limits overflow")?,
        )?
        .min(MAX_BYTES),
    })
}
fn job_material(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    job: &AgentDeletionJob,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    let size:usize=tx.query_row(&format!("SELECT COALESCE((SELECT {JOB_BYTES} FROM agent_deletion_jobs WHERE deletion_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?2),0)"),params![job.deletion_id,scope.agent_id],|r|r.get(0))?;
    material.charge(
        size.checked_add(usize::try_from(serialized_bytes(job)?)?)
            .and_then(|n| n.checked_add(512))
            .context("deletion Scheduling job bytes overflow")?,
        3,
    )
}
fn validate_job(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    job: &AgentDeletionJob,
    phase: AgentDeletionPhase,
) -> Result<AgentIdentityRecord> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid deletion Scheduling scope"
    );
    let (id,agent,status,at,raw):(String,String,String,String,String)=tx.query_row("SELECT deletion_id,agent_id,status,phase,payload_json FROM agent_deletion_jobs WHERE deletion_id=?1",[&job.deletion_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let actual: AgentDeletionJob = serde_json::from_str(&raw)?;
    ensure!(
        actual == *job
            && id == job.deletion_id
            && agent == scope.agent_id
            && actual.agent_id == agent
            && actual.status == AgentDeletionStatus::Running
            && status == "running"
            && actual.phase == phase
            && serde_json::to_value(phase)?.as_str() == Some(at.as_str()),
        "original deletion Scheduling job/phase changed"
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
            && serde_json::to_value(identity.status)?.as_str() == Some(status.as_str()),
        "deletion Scheduling original incarnation changed"
    );
    ensure!(
        matches!(
            (identity.status, job.mode),
            (
                AgentRegistryStatus::Deleting,
                AgentDeletionMode::Delete | AgentDeletionMode::ParentCleanup
            ) | (
                AgentRegistryStatus::Deleted,
                AgentDeletionMode::CleanupRepair
            )
        ),
        "deletion Scheduling has no original cleanup authority"
    );
    Ok(identity)
}
fn source(selection: DeletionSchedulingSelection) -> (&'static str, &'static str, &'static str) {
    match selection {
        DeletionSchedulingSelection::Timers => ("timers", "timer_id", "agent_id"),
        DeletionSchedulingSelection::ExternalTriggers => (
            "external_triggers",
            "external_trigger_id",
            "target_agent_id",
        ),
    }
}
fn charge_target(
    tx: &Transaction<'_>,
    key: &SchedulingKey,
    material: &mut Material<'_, '_>,
) -> Result<bool> {
    let (from, id, owner, _, sort) = stored::source(key.family);
    let projection = stored::projection(key.family);
    let bytes:Option<usize>=tx.query_row(&format!("SELECT length(CAST(c.payload_json AS BLOB))+length(CAST({projection} AS BLOB))+length(CAST({owner} AS BLOB))+length(CAST({id} AS BLOB))+length(CAST({sort} AS BLOB))+length(CAST(c.created_at AS BLOB))+128 FROM {from} WHERE {id}=?1"),[&key.id],|r|r.get(0)).optional()?;
    if let Some(bytes) = bytes {
        material.charge(bytes, 1)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
fn remaining(
    load: &SchedulingDomainLoadRequest,
    material: &Material<'_, '_>,
) -> Result<SchedulingDomainLoadRequest> {
    let next = SchedulingDomainLoadRequest {
        capture: load.capture.clone(),
        byte_limit: load
            .byte_limit
            .checked_sub(material.bytes)
            .context("deletion Scheduling bytes exhausted")?,
        record_limit: load
            .record_limit
            .checked_sub(material.records)
            .context("deletion Scheduling records exhausted")?,
    };
    bound(0, 0, &next)?;
    Ok(next)
}
impl RuntimeDb {
    /// IDs only. The original job/identity and the actual ledger precede decode.
    pub(crate) fn deletion_scheduling_batch(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        selection: DeletionSchedulingSelection,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<DeletionSchedulingBatch> {
        let load = request(ledger)?;
        self.transaction(|tx| {
            let mut material=Material::new(scope,&load,ledger);
            job_material(tx,scope,job,&mut material)?;
            validate_job(tx,scope,job,if selection==DeletionSchedulingSelection::Timers {AgentDeletionPhase::Quiesce} else {AgentDeletionPhase::Ingress})?;
            let (table,id,owner)=source(selection);
            let (columns,order)=match selection {
                DeletionSchedulingSelection::Timers=>(format!("{id},updated_at,created_at"),format!("updated_at ASC,created_at ASC,{id} DESC")),
                DeletionSchedulingSelection::ExternalTriggers=>(format!("{id},created_at"),format!("created_at DESC,{id} ASC")),
            };
            // The original per-row Quiesce decoded the legacy scheduled alias
            // as Active. ParentCleanup's separate bulk SQL remains active only.
            let live=match selection {
                DeletionSchedulingSelection::Timers=>"status IN ('active','scheduled')",
                DeletionSchedulingSelection::ExternalTriggers=>"status='active'",
            };
            let from=format!("(SELECT {columns} FROM {table} WHERE {owner}=?1 AND {live} ORDER BY {order} LIMIT {BATCH})");
            let (bytes,count):(usize,usize)=tx.query_row(&format!("SELECT COALESCE(SUM(length(CAST({id} AS BLOB))+128),0),COUNT(*) FROM {from}"),[&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            material.charge(bytes,count)?;
            let mut q=tx.prepare(&format!("SELECT {id} FROM {from} ORDER BY {order}"))?;
            let ids=q.query_map([&scope.agent_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(DeletionSchedulingBatch {ids,_guards:material.guards})
        })
    }
    pub(crate) fn cancel_timer_for_deletion(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        id: &str,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(DeletionSchedulingAck, AgentIdentityRecord)> {
        self.mutate_deletion_scheduling(scope, job, Mutation::Timer(id), Utc::now(), ledger)
    }
    pub(crate) fn cancel_parent_cleanup_timers(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        now: DateTime<Utc>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(DeletionSchedulingAck, AgentIdentityRecord)> {
        self.mutate_deletion_scheduling(scope, job, Mutation::ParentTimers, now, ledger)
    }
    pub(crate) fn revoke_trigger_for_deletion(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        id: &str,
        now: DateTime<Utc>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(DeletionSchedulingAck, AgentIdentityRecord)> {
        self.mutate_deletion_scheduling(scope, job, Mutation::Trigger(id), now, ledger)
    }
    fn mutate_deletion_scheduling(
        &self,
        scope: &InputEvidenceScope,
        job: &AgentDeletionJob,
        mutation: Mutation<'_>,
        now: DateTime<Utc>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(DeletionSchedulingAck, AgentIdentityRecord)> {
        let load = request(ledger)?;
        self.transaction(|tx| {
            let mut material=Material::new(scope,&load,ledger);
            job_material(tx,scope,job,&mut material)?;
            let identity=validate_job(tx,scope,job,if matches!(mutation,Mutation::Trigger(_)) {AgentDeletionPhase::Ingress} else {AgentDeletionPhase::Quiesce})?;
            ensure!(!matches!(mutation,Mutation::ParentTimers) || job.mode==AgentDeletionMode::ParentCleanup,"bulk timer cleanup requires original ParentCleanup job");
            ensure!(!matches!(mutation,Mutation::Timer(_)) || job.mode!=AgentDeletionMode::ParentCleanup,"ParentCleanup must preserve its original bulk Wake/Queue transaction");
            if matches!(mutation,Mutation::ParentTimers) {
                let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
                material.charge(bytes,1)?;
                if let Some(blocker)=super::super::reclamation::parent_cleanup_blocker_tx(tx,&scope.agent_id)? {anyhow::bail!("agent_cleanup_blocked: {blocker}");}
            }
            let mut keys=BTreeSet::new();
            match mutation {
                Mutation::ParentTimers=>{
                    // Bound the original whole bulk decode, all pending Wake
                    // addresses and Queue payloads before collecting any IDs.
                    let (bytes,count):(usize,usize)=tx.query_row("SELECT COALESCE((SELECT SUM(length(CAST(payload_json AS BLOB))+length(CAST(timer_id AS BLOB))+512) FROM timers WHERE agent_id=?1 AND status='active'),0)+COALESCE((SELECT SUM(length(CAST(w.timer_id AS BLOB))+length(CAST(w.message_id AS BLOB))+length(CAST(w.created_at AS BLOB))+length(CAST(w.updated_at AS BLOB))+256+COALESCE(length(CAST(q.payload_json AS BLOB)),0)) FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id LEFT JOIN queue_entries q ON q.message_id=w.message_id AND q.status IN ('queued','interrupted') WHERE t.agent_id=?1 AND t.status='active' AND w.status='pending'),0),(SELECT COUNT(*) FROM timers WHERE agent_id=?1 AND status='active')+(SELECT COUNT(*) FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE t.agent_id=?1 AND t.status='active' AND w.status='pending')",[&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
                    material.charge(bytes,count.checked_mul(3).context("deletion timer bulk count overflow")?)?;
                    let mut q=tx.prepare("SELECT timer_id FROM timers WHERE agent_id=?1 AND status='active'")?;
                    for id in q.query_map([&scope.agent_id],|r|r.get::<_,String>(0))? {keys.insert(SchedulingKey::new(SchedulingFamily::Timer,id?));}
                    let mut q=tx.prepare("SELECT w.timer_id,w.message_id FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE t.agent_id=?1 AND t.status='active' AND w.status='pending'")?;
                    for pair in q.query_map([&scope.agent_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))? {let (timer,message)=pair?;keys.insert(SchedulingKey::wake(&timer,&message));}
                }
                Mutation::Timer(id)|Mutation::Trigger(id)=>{
                    let family=if matches!(mutation,Mutation::Timer(_)) {SchedulingFamily::Timer} else {SchedulingFamily::ExternalTrigger};
                    material.charge(id.len().checked_add(128).context("deletion Scheduling key overflow")?,1)?;
                    let key=SchedulingKey::new(family,id);
                    if charge_target(tx,&key,&mut material)? {keys.insert(key);}
                }
            }
            let available=remaining(&load,&material)?;
            let proof=if identity.status==AgentRegistryStatus::Deleting && !keys.is_empty() {Some(prepare_existing_scheduling_maintenance_tx(tx,scope,&keys,&available,ledger)?)} else {None};
            let mut actual=BTreeSet::new();
            let changed=match mutation {
                Mutation::ParentTimers=>{
                    let ack=repositories::cancel_active_timers_with_writes_tx(tx,&scope.agent_id,now)?;
                    for id in ack.timer_ids {actual.insert(SchedulingKey::new(SchedulingFamily::Timer,id));}
                    for (timer,message) in ack.wake_pairs {actual.insert(SchedulingKey::wake(&timer,&message));}
                    // Queue writes are part of this original canonical TX;
                    // they do not grant fresh Input or dispatch authority.
                    drop(ack.queue_entries);
                    ack.cancelled_timers
                }
                Mutation::Timer(id)|Mutation::Trigger(id)=>{
                    let family=if matches!(mutation,Mutation::Timer(_)) {SchedulingFamily::Timer} else {SchedulingFamily::ExternalTrigger};
                    let key=SchedulingKey::new(family,id);
                    match stored::read_stored(tx,&key,&scope.agent_id,None)? {
                        Some(before)=>match before.fact {
                            CanonicalSchedulingFact::Timer(mut timer) if timer.status==TimerStatus::Active=>{
                                timer.status=TimerStatus::Cancelled;
                                let changed=repositories::upsert_timer_with_write_tx(tx,&timer)?;
                                if changed {actual.insert(key);} usize::from(changed)
                            }
                            CanonicalSchedulingFact::ExternalTrigger(mut trigger) if trigger.status==ExternalTriggerStatus::Active=>{
                                trigger.status=ExternalTriggerStatus::Revoked;trigger.revoked_at=Some(now);
                                let changed=repositories::upsert_external_trigger_with_write_tx(tx,&trigger)?;
                                if changed {actual.insert(key);} usize::from(changed)
                            }
                            _=>0,
                        },None=>0,
                    }
                }
            };
            let (qualified_actual_keys,invalidation)=if let Some(proof)=proof {
                let mut ack=register_existing_scheduling_maintenance_tx(tx,proof,&actual,&available)?;
                material.guards.extend(ack.take_budget_views());
                (ack.qualified_actual_keys,ack.invalidation)
            } else {(BTreeSet::new(),None)};
            // Deleted CleanupRepair deliberately has no qualification update.
            Ok((DeletionSchedulingAck {changed,qualified_actual_keys,invalidation,guards:material.guards},identity))
        })
    }
}
#[cfg(test)]
#[path = "deletion_maintenance_tests.rs"]
mod tests;
