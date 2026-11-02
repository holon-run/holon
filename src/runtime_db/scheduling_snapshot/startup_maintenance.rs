//! Original exclusive startup transaction; no ordinary read or new admission.
use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery,
    transitions::StartupInputRecoveryScope,
};
use crate::types::{AgentState, AgentStatus};

// Same original business predicates, restricted to the live Wait references.
// Foreign bodies cannot influence the old same-agent match and are not decoded.
pub(in crate::runtime_db) const WAIT_TASK_POOL: &str = "SELECT t.payload_json FROM tasks t WHERE EXISTS(SELECT 1 FROM wait_conditions w,json_each(w.wake_sources_json) s WHERE w.status IN ('active','triggered') AND w.agent_id=t.owner_agent_id AND json_extract(s.value,'$.kind')='task_result' AND json_extract(s.value,'$.task_id')=t.task_id)";
pub(in crate::runtime_db) const WAIT_TRIGGER_POOL: &str = "SELECT t.payload_json FROM external_triggers t WHERE EXISTS(SELECT 1 FROM wait_conditions w,json_each(w.wake_sources_json) s WHERE w.status IN ('active','triggered') AND w.agent_id=t.target_agent_id AND json_extract(s.value,'$.kind')='external_ingress' AND json_extract(s.value,'$.external_trigger_id')=t.external_trigger_id)";
pub(in crate::runtime_db) const WAIT_TIMER_POOL: &str = "SELECT t.payload_json FROM timers t WHERE EXISTS(SELECT 1 FROM wait_conditions w WHERE w.status IN ('active','triggered') AND w.agent_id=t.agent_id AND w.kind='timer' AND w.subject_ref=t.timer_id)";
struct ScopeMaterial {
    accounting: InputEvidenceScope,
    identity: Option<Arc<crate::types::AgentIdentityRecord>>,
    remaining: SchedulingDomainLoadRequest,
    guards: Vec<Arc<BudgetViewGuard>>,
}
pub(crate) struct StartupSchedulingMaterial {
    scopes: BTreeMap<String, ScopeMaterial>,
}
pub(crate) struct StartupSchedulingAck {
    pub identities: BTreeMap<String, Option<Arc<crate::types::AgentIdentityRecord>>>,
    // Original ACK provenance; durable invalidation is already committed, not a Ready install.
    pub _invalidations: BTreeMap<String, SchedulingScopeInvalidation>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
enum Cleanup<'tx, 'conn> {
    Bound(ExistingSchedulingMaintenanceProof<'tx, 'conn>),
    Unbound(seed_maintenance::UnboundSchedulingCleanup<'tx, 'conn>),
    Retired,
}
pub(in crate::runtime_db) struct StartupSchedulingCleanup<'tx, 'conn> {
    proofs: BTreeMap<String, Cleanup<'tx, 'conn>>,
    material: StartupSchedulingMaterial,
}
fn remaining(
    request: &SchedulingDomainLoadRequest,
    material: &Material<'_, '_>,
) -> Result<SchedulingDomainLoadRequest> {
    let next = SchedulingDomainLoadRequest {
        capture: request.capture.clone(),
        record_limit: request
            .record_limit
            .checked_sub(material.records)
            .context("startup Scheduling record allowance exhausted")?,
        byte_limit: request
            .byte_limit
            .checked_sub(material.bytes)
            .context("startup Scheduling byte allowance exhausted")?,
    };
    bound(0, 0, &next)?;
    Ok(next)
}
impl StartupSchedulingMaterial {
    /// Called before the original actor decodes State/Wait/dependency bodies.
    /// Accounting scopes are held reservation addresses, never unbound authority.
    pub(crate) fn prepare_tx(
        tx: &Transaction<'_>,
        scopes: &[StartupInputRecoveryScope],
        accounting: &[InputEvidenceScope],
        request: &SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        admission: &ExclusiveStartupInputRecovery<'_>,
    ) -> Result<Self> {
        admission.validate_transaction(tx)?;
        ensure!(
            scopes.len() == accounting.len(),
            "startup Scheduling reservation group differs"
        );
        let mut out = BTreeMap::new();
        let mut covered_states = 0usize;
        let mut covered_waits = 0usize;
        for authority in scopes {
            let agent = authority.agent_id();
            let account = accounting
                .iter()
                .find(|s| s.agent_id == agent)
                .context("startup Scheduling missing accounting reservation")?;
            ensure!(
                account.identity_incarnation > 0 && !out.contains_key(agent),
                "duplicate/invalid startup Scheduling reservation"
            );
            let mut material = Material::new(account, request, ledger);
            let identity = match authority {
                StartupInputRecoveryScope::Current(scope) => {
                    ensure!(
                        scope == account,
                        "startup Scheduling current reservation differs"
                    );
                    Some(Arc::new(
                        loader::scope_identity(tx, scope, &mut material, false)?
                            .context(SchedulingSnapshotError::RetiredScope)?,
                    ))
                }
                StartupInputRecoveryScope::Unbound(seed) => {
                    seed_maintenance::validate_unbound_scope_tx(tx, seed, &mut material)?;
                    None
                }
                StartupInputRecoveryScope::Retired {
                    identity_incarnation,
                    status,
                    ..
                } => {
                    ensure!(
                        *identity_incarnation == account.identity_incarnation
                            && *status != AgentRegistryStatus::Active,
                        "startup Scheduling retired reservation differs"
                    );
                    let bytes:usize=tx.query_row("SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1",[agent],|r|r.get(0))?;
                    material.charge(bytes, 1)?;
                    let (id,normalized,raw):(String,String,String)=tx.query_row("SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",[agent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                    let identity = repositories::decode_agent_identity_payload(&raw)?;
                    ensure!(
                        id == agent
                            && identity.agent_id == agent
                            && identity.incarnation == *identity_incarnation
                            && identity.status == *status
                            && serde_json::to_value(status)?.as_str() == Some(normalized.as_str()),
                        "startup Scheduling retired identity changed"
                    );
                    Some(Arc::new(identity))
                }
            };
            // Raw JSON lengths first: the finite native selector below may parse
            // wake_sources_json only after this actual owner guard is alive.
            let (waits,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(wake_sources_json AS BLOB))+COALESCE(length(CAST(continuation_json AS BLOB)),0)+length(CAST(wait_condition_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(subject_ref AS BLOB)),0)+COALESCE(length(CAST(waiting_for AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(trigger_message_id AS BLOB)),0)+length(CAST(status AS BLOB))+length(CAST(kind AS BLOB))+COALESCE(length(CAST(source AS BLOB)),0)+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(expires_at AS BLOB)),0)+COALESCE(length(CAST(resolved_at AS BLOB)),0)+COALESCE(length(CAST(cancelled_at AS BLOB)),0)+COALESCE(length(CAST(triggered_at AS BLOB)),0)+length(CAST(payload_json AS BLOB))+512),0) FROM wait_conditions WHERE agent_id=?1 AND status IN ('active','triggered')",[agent],|r|Ok((r.get(0)?,r.get(1)?)))?;
            material.charge(bytes, waits)?;
            // The original row decoder owns both normalized JSON collections.
            // Raw bytes are admitted before native traversal or typed allocation.
            let wait_nodes:usize=tx.query_row("SELECT COALESCE(SUM((SELECT COUNT(*) FROM json_tree(w.wake_sources_json))+(SELECT COUNT(*) FROM json_tree(w.continuation_json))),0) FROM wait_conditions w WHERE w.agent_id=?1 AND w.status IN ('active','triggered')",[agent],|r|r.get(0))?;
            material.charge(
                wait_nodes
                    .checked_mul(std::mem::size_of::<serde_json::Value>())
                    .context("startup Wait node bytes overflow")?,
                wait_nodes,
            )?;
            covered_waits += waits;
            let (states,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+512),0) FROM agent_states WHERE agent_id=?1",[agent],|r|Ok((r.get(0)?,r.get(1)?)))?;
            material.charge(bytes, states)?;
            let state_nodes: usize = tx.query_row(
                "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
                [agent],
                |r| r.get(0),
            )?;
            material.charge(
                state_nodes
                    .checked_mul(std::mem::size_of::<serde_json::Value>())
                    .context("startup State node bytes overflow")?,
                state_nodes,
            )?;
            covered_states += states;
            for sql in [
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+512),0) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')",
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(t.payload_json AS BLOB))+length(CAST(t.task_id AS BLOB))+length(CAST(t.owner_agent_id AS BLOB))+512),0) FROM tasks t WHERE t.owner_agent_id=?1 AND EXISTS(SELECT 1 FROM wait_conditions w,json_each(w.wake_sources_json) s WHERE w.status IN ('active','triggered') AND w.agent_id=t.owner_agent_id AND json_extract(s.value,'$.kind')='task_result' AND json_extract(s.value,'$.task_id')=t.task_id)",
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(t.payload_json AS BLOB))+length(CAST(t.external_trigger_id AS BLOB))+length(CAST(t.target_agent_id AS BLOB))+512),0) FROM external_triggers t WHERE t.target_agent_id=?1 AND EXISTS(SELECT 1 FROM wait_conditions w,json_each(w.wake_sources_json) s WHERE w.status IN ('active','triggered') AND w.agent_id=t.target_agent_id AND json_extract(s.value,'$.kind')='external_ingress' AND json_extract(s.value,'$.external_trigger_id')=t.external_trigger_id)",
            ] {
                let (n,bytes):(usize,usize)=tx.query_row(sql,[agent],|r|Ok((r.get(0)?,r.get(1)?)))?;material.charge(bytes,n)?;
            }
            // Exactly the same finite same-owner pool the original actor decodes;
            // no unrelated historical/foreign Task body enters this traversal.
            let sql=format!("SELECT COUNT(*) FROM ({WAIT_TASK_POOL} AND t.owner_agent_id=?1) selected,json_tree(selected.payload_json)");
            let task_nodes: usize = tx.query_row(&sql, [agent], |r| r.get(0))?;
            material.charge(
                task_nodes
                    .checked_mul(std::mem::size_of::<serde_json::Value>())
                    .context("startup Task node bytes overflow")?,
                task_nodes,
            )?;
            // Active-only is the original bulk cancellation predicate. scheduled
            // aliases still participate in the distinct original Wait predicate.
            let projection = stored::projection(SchedulingFamily::Timer);
            let sql=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(c.payload_json AS BLOB))+length(CAST({projection} AS BLOB))+512),0) FROM timers c WHERE c.agent_id=?1 AND (c.status='active' OR EXISTS(SELECT 1 FROM wait_conditions w WHERE w.agent_id=c.agent_id AND w.status IN ('active','triggered') AND w.kind='timer' AND w.subject_ref=c.timer_id))");
            let (n, bytes): (usize, usize) =
                tx.query_row(&sql, [agent], |r| Ok((r.get(0)?, r.get(1)?)))?;
            material.charge(bytes, n)?;
            let projection = stored::projection(SchedulingFamily::TimerWake);
            let sql=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({projection} AS BLOB))+512),0) FROM timer_wakes c JOIN timers t ON t.timer_id=c.timer_id WHERE t.agent_id=?1 AND t.status='active' AND c.status='pending'");
            let (n, bytes): (usize, usize) =
                tx.query_row(&sql, [agent], |r| Ok((r.get(0)?, r.get(1)?)))?;
            material.charge(bytes, n)?;
            out.insert(
                agent.to_owned(),
                ScopeMaterial {
                    accounting: account.clone(),
                    identity,
                    remaining: remaining(request, &material)?,
                    guards: material.guards,
                },
            );
        }
        let (states,waits):(usize,usize)=tx.query_row("SELECT (SELECT COUNT(*) FROM agent_states),(SELECT COUNT(*) FROM wait_conditions WHERE status IN ('active','triggered'))",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            states == covered_states && waits == covered_waits,
            "startup Scheduling has an unreserved State/Wait scope"
        );
        Ok(Self { scopes: out })
    }
    pub(in crate::runtime_db) fn prepare_stopped_tx<'tx, 'conn>(
        self,
        tx: &'tx Transaction<'conn>,
        states: &BTreeMap<String, AgentState>,
        scopes: &[StartupInputRecoveryScope],
        ledger: &InputEvidenceBudgetLedger,
        admission: &ExclusiveStartupInputRecovery<'_>,
    ) -> Result<StartupSchedulingCleanup<'tx, 'conn>> {
        let mut material = self;
        let mut proofs = BTreeMap::new();
        for state in states.values().filter(|s| s.status == AgentStatus::Stopped) {
            let authority = scopes
                .iter()
                .find(|s| s.agent_id() == state.id)
                .context("unreserved stopped Scheduling scope")?;
            let slot = material
                .scopes
                .get_mut(&state.id)
                .context("unbudgeted stopped Scheduling scope")?;
            let mut selected = Material::new(&slot.accounting, &slot.remaining, ledger);
            let (n,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(record_id AS BLOB))+128),0) FROM (SELECT timer_id AS record_id FROM timers WHERE agent_id=?1 AND status='active' UNION ALL SELECT json_array(w.timer_id,w.message_id) FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE t.agent_id=?1 AND t.status='active' AND w.status='pending')",[&state.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            selected.charge(bytes, n)?;
            let mut statement=tx.prepare("SELECT 'timer',timer_id FROM timers WHERE agent_id=?1 AND status='active' UNION ALL SELECT 'timer_wake',json_array(w.timer_id,w.message_id) FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE t.agent_id=?1 AND t.status='active' AND w.status='pending'")?;
            let keys = statement
                .query_map([&state.id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .map(|r| {
                    let (family, id) = r?;
                    Ok(SchedulingKey::new(
                        if family == "timer" {
                            SchedulingFamily::Timer
                        } else {
                            SchedulingFamily::TimerWake
                        },
                        id,
                    ))
                })
                .collect::<Result<BTreeSet<_>>>()?;
            let remaining = remaining(&slot.remaining, &selected)?;
            slot.guards.extend(selected.guards);
            slot.remaining = remaining;
            let proof = match authority {
                StartupInputRecoveryScope::Current(scope) => {
                    Cleanup::Bound(prepare_existing_scheduling_maintenance_tx(
                        tx,
                        scope,
                        &keys,
                        &slot.remaining,
                        ledger,
                    )?)
                }
                StartupInputRecoveryScope::Retired {
                    status: AgentRegistryStatus::Deleting,
                    ..
                } => Cleanup::Bound(prepare_existing_scheduling_maintenance_tx(
                    tx,
                    &slot.accounting,
                    &keys,
                    &slot.remaining,
                    ledger,
                )?),
                StartupInputRecoveryScope::Retired { .. } => Cleanup::Retired,
                StartupInputRecoveryScope::Unbound(seed) => {
                    Cleanup::Unbound(seed_maintenance::prepare_unbound_scheduling_cleanup_tx(
                        tx,
                        seed,
                        &keys,
                        &slot.accounting,
                        &slot.remaining,
                        ledger,
                        admission,
                    )?)
                }
            };
            proofs.insert(state.id.clone(), proof);
        }
        Ok(StartupSchedulingCleanup { proofs, material })
    }
}
impl StartupSchedulingCleanup<'_, '_> {
    pub(in crate::runtime_db) fn finish_tx(
        mut self,
        tx: &Transaction<'_>,
        actual: &BTreeMap<String, repositories::TimerCancellationWrites>,
        ledger: &InputEvidenceBudgetLedger,
        admission: &ExclusiveStartupInputRecovery<'_>,
    ) -> Result<StartupSchedulingAck> {
        admission.validate_transaction(tx)?;
        ensure!(
            actual.keys().all(|agent| self.proofs.contains_key(agent)),
            "startup Scheduling unprepared actual owner"
        );
        let mut invalidations = BTreeMap::new();
        let mut guards = Vec::new();
        for (agent, proof) in self.proofs {
            let slot = self
                .material
                .scopes
                .get_mut(&agent)
                .context("startup Scheduling material missing")?;
            let writes = actual
                .get(&agent)
                .context("startup Scheduling actual ACK missing")?;
            let keys = writes
                .timer_ids
                .iter()
                .map(|id| SchedulingKey::new(SchedulingFamily::Timer, id))
                .chain(
                    writes
                        .wake_pairs
                        .iter()
                        .map(|(timer, message)| SchedulingKey::wake(timer, message)),
                )
                .collect::<BTreeSet<_>>();
            match proof {
                Cleanup::Bound(proof) => {
                    let mut ack = register_existing_scheduling_maintenance_tx(
                        tx,
                        proof,
                        &keys,
                        &slot.remaining,
                    )?;
                    if let Some(value) = ack.invalidation.take() {
                        invalidations.insert(agent, value);
                    }
                    guards.extend(ack.take_budget_views());
                }
                Cleanup::Unbound(proof) => {
                    guards.extend(seed_maintenance::register_unbound_scheduling_cleanup_tx(
                        tx,
                        proof,
                        &keys,
                        &slot.accounting,
                        &slot.remaining,
                        ledger,
                        admission,
                    )?)
                }
                Cleanup::Retired => {}
            }
        }
        let identities = self
            .material
            .scopes
            .iter()
            .map(|(agent, slot)| (agent.clone(), slot.identity.clone()))
            .collect();
        for slot in self.material.scopes.values_mut() {
            guards.append(&mut slot.guards);
        }
        Ok(StartupSchedulingAck {
            identities,
            _invalidations: invalidations,
            _guards: guards,
        })
    }
}

#[cfg(test)]
#[path = "startup_maintenance_tests.rs"]
mod tests;
