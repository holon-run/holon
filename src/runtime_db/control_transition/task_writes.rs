//! Task/result facts join the original Control transaction and original receipt.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
    resident_task::{CommittedTaskDelta, TaskFamily, TaskKey},
};
use crate::runtime_db::{task_result_settlement as result, task_snapshot as tasks};
use std::collections::BTreeMap;

struct Material<'a> {
    scope: &'a InputEvidenceScope,
    request: &'a tasks::TaskDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl Material<'_> {
    fn charge(&mut self, bytes: usize, records: usize, copies: usize) -> Result<()> {
        let bytes = bytes
            .checked_mul(copies)
            .context("Control Task material overflow")?;
        let records = records
            .checked_mul(copies)
            .context("Control Task count overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Control Task material overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Control Task count overflow")?;
        ensure!(
            self.bytes <= self.request.byte_limit.min(64 * 1024 * 1024)
                && self.records <= self.request.record_limit.min(65_536),
            tasks::TaskSnapshotError::ResourceExhausted("Control Task material")
        );
        self.guards.push(self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(())
    }
    fn result(&mut self, estimate: result::TaskResultMaterialEstimate) -> Result<()> {
        self.charge(
            estimate
                .payload_bytes
                .checked_add(estimate.metadata_estimate_bytes)
                .context("Control Result material overflow")?,
            estimate
                .records
                .checked_mul(12)
                .context("Control Result count overflow")?,
            1,
        )
    }
}

pub(super) struct ControlTaskWrites<'tx, 'connection> {
    proof: tasks::TaskWriteProof,
    allowed: BTreeSet<TaskKey>,
    observations: BTreeSet<TaskKey>,
    actual: BTreeSet<TaskKey>,
    pending: BTreeMap<String, result::PreparedTaskResultMutation<'tx, 'connection>>,
    request: tasks::TaskDomainLoadRequest,
    before_material: tasks::TaskMaintenanceMaterial,
    material_total: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
}
pub(super) struct ControlTaskFacts {
    pub(super) delta: CommittedTaskDelta,
    pub(super) actual: BTreeSet<TaskKey>,
    pub(super) observations: BTreeSet<TaskKey>,
    pub(super) guards: Vec<Arc<BudgetViewGuard>>,
}

pub(super) fn prepare_control_task_writes_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    command: &PreparedControl,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ControlTaskWrites<'tx, 'connection>> {
    let scope = InputEvidenceScope {
        agent_id: command.key.agent_id.clone(),
        identity_incarnation: command.key.identity_incarnation,
    };
    let mut material = Material {
        scope: &scope,
        request,
        ledger,
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    // Reserve request/command addresses and future frozen pending bodies before
    // constructing any owned selector, Result record, or closed operation.
    material.charge(
        usize::try_from(serialized_bytes(&(
            &command.task_results,
            &command.execution_commands,
            &(
                request.capture.include_active,
                request.capture.include_unsettled,
                request.capture.terminal_limit,
                &request.capture.required_keys,
            ),
            &command.key,
        ))?)?,
        command.task_results.len()
            + command.execution_commands.len()
            + request.capture.required_keys.len()
            + 1,
        8,
    )?;
    material.charge(
        usize::try_from(serialized_bytes(&request.query)?)?,
        request.query.selector_record_count()?,
        8,
    )?;
    // load_snapshot_in performs the original complete active-Task OCC read. An
    // extra concurrent target must be bounded before that read, even though OCC
    // will reject it instead of granting qualification.
    let (pool_count, pool_bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM tasks WHERE owner_agent_id=?1 AND status IN ('queued','running','cancelling')",
        [&scope.agent_id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    material.charge(pool_bytes, pool_count, 4)?;
    let checkpoint: (usize, usize) = tx.query_row(
        "SELECT COALESCE(SUM(length(CAST(s.roots_json AS BLOB))+COALESCE(length(CAST(i.roots_json AS BLOB)),0)),0),COUNT(*) FROM agent_episode_continuity b JOIN episode_continuity_seeds s ON s.seed_id=b.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=s.seed_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1",
        params![scope.agent_id, scope.identity_incarnation], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    material.charge(checkpoint.0, checkpoint.1, 4)?;
    let limits = result::TaskResultWriteLimits {
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    };
    let mut pending = BTreeMap::new();
    let mut candidates = BTreeSet::new();
    for planned in &command.task_results {
        candidates.insert(TaskKey::new(TaskFamily::Task, &planned.task.id));
        if let Some(record) = TaskResultSettlementRecord::pending(
            &planned.task,
            &planned.message,
            command.prepared_at,
        )? {
            material.result(result::upsert_pending_material_estimate_tx(
                tx,
                &planned.task,
                &record,
            )?)?;
            let operation = result::prepare_upsert_pending_tx(tx, &planned.task, &record, limits)?;
            candidates.extend(
                operation
                    .candidate_result_ids()
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
            );
            ensure!(
                pending.insert(planned.task.id.clone(), operation).is_none(),
                "duplicate Control Task target"
            );
        }
    }
    // A pure Interrupt/Settle can change activation metadata with no Result SQL
    // write. Observe only those exact affected original activation addresses.
    for command in &command.execution_commands {
        let attempt = match command {
            ExecutionProtocolCommand::Admit(c) => Some(c.attempt.attempt_id.as_str()),
            ExecutionProtocolCommand::Settle(c) => Some(c.outcome.attempt_id.as_str()),
            ExecutionProtocolCommand::Interrupt(c) => Some(c.attempt_id.as_str()),
            ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(c) => {
                Some(c.attempt_id.as_str())
            }
            ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(c) => {
                Some(c.attempt_id.as_str())
            }
            _ => None,
        };
        if let Some(attempt) = attempt {
            let (count, bytes): (usize, usize) = tx.query_row(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(result_identity AS BLOB))+128),0) FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2",
                params![scope.agent_id, attempt], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            material.charge(bytes, count, 4)?;
            let mut query = tx.prepare("SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2 ORDER BY result_identity")?;
            for id in query.query_map(params![scope.agent_id, attempt], |row| {
                row.get::<_, String>(0)
            })? {
                candidates.insert(TaskKey::new(TaskFamily::ResultSettlement, id?));
            }
        }
    }
    material.charge(
        usize::try_from(serialized_bytes(&candidates)?)?,
        candidates.len(),
        4,
    )?;
    let before_material = tasks::task_maintenance_material_tx(tx, &scope, &candidates)?;
    material.charge(before_material.bytes, before_material.records, 1)?;
    let mut allowed = tasks::qualified_task_recovery_selectors_tx(tx, &scope, &candidates)?;
    let mut observations = BTreeSet::new();
    for key in &candidates {
        if tasks::checkpoint_material_size_on(tx, key)?.is_some() {
            observations.insert(key.clone());
        }
    }
    // Control business admission remains Active-only. Only the authenticated
    // original terminal command for an existing qualified Task can grant its
    // new Result. Existing raw neighbors keep their original cleanup semantics.
    for operation in pending.values() {
        let original = operation.upsert_pending_view()?;
        let key = TaskKey::new(
            TaskFamily::ResultSettlement,
            &original.record().result_identity,
        );
        if tasks::checkpoint_material_size_on(tx, &key)?.is_none()
            && allowed.contains(&TaskKey::new(TaskFamily::Task, &original.task().id))
        {
            allowed.insert(key);
        }
    }
    material.charge(
        usize::try_from(serialized_bytes(&(&allowed, &observations))?)?,
        allowed.len() + observations.len(),
        4,
    )?;
    let proof = tasks::prepare_task_write_tx(tx, &scope, &allowed)?;
    tasks::preflight_task_write_snapshot_with_budget_tx(tx, &proof, request, ledger)?
        .require_current()?;
    Ok(ControlTaskWrites {
        proof,
        allowed,
        observations,
        actual: BTreeSet::new(),
        pending,
        request: request.clone(),
        before_material,
        material_total: (material.bytes, material.records),
        guards: material.guards,
    })
}

impl ControlTaskWrites<'_, '_> {
    pub(super) fn task_written(&mut self, task_id: &str) {
        self.actual.insert(TaskKey::new(TaskFamily::Task, task_id));
    }
    pub(super) fn execute_pending(&mut self, task_id: &str) -> Result<()> {
        if let Some(operation) = self.pending.remove(task_id) {
            let ack = operation.execute()?;
            ensure!(
                matches!(ack.outcome, result::TaskResultWriteOutcome::Changed(_)),
                "Control pending outcome differs"
            );
            self.actual.extend(
                ack.actual_result_ids
                    .into_iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
            );
        }
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        tx: &Transaction<'_>,
        final_state: &AgentState,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ControlTaskFacts> {
        ensure!(
            self.pending.is_empty(),
            "Control omitted its closed Result operation"
        );
        let selected: BTreeSet<_> = self.actual.intersection(&self.allowed).cloned().collect();
        let after = tasks::task_maintenance_material_tx(tx, self.proof.scope(), &self.allowed)?;
        let mut material = Material {
            scope: self.proof.scope(),
            request: &self.request,
            ledger,
            bytes: self.material_total.0,
            records: self.material_total.1,
            guards: Vec::new(),
        };
        material.charge(
            after.bytes.saturating_sub(self.before_material.bytes),
            after.records.saturating_sub(self.before_material.records),
            1,
        )?;
        self.guards.extend(material.guards);
        let facts = tasks::register_task_writes_with_budget_tx(
            tx,
            &self.proof,
            &selected,
            final_state,
            &self.request,
            ledger,
        )?;
        self.guards.push(ledger.register_task_stop_view(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            facts.delta.stop_records.iter(),
        )?);
        self.guards.push(ledger.register_task_view(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            facts.delta.records.iter(),
        )?);
        self.guards.push(ledger.register_owned_bytes(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(&facts.delta.coverage)?,
            },
        )?);
        Ok(ControlTaskFacts {
            delta: facts.delta,
            actual: self.actual,
            observations: self.observations,
            guards: self.guards,
        })
    }
}
