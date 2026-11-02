//! The original monitor's Running Task and Input are one independently qualified ACK.
use super::*;
use crate::runtime::{
    input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger},
    FrozenChildTaskRunning, OriginalChildTaskProducer,
};
use crate::runtime_db::{command_task_terminal as owner, task_snapshot as tasks};
use crate::types::TaskStatus;
use std::sync::{Arc, Weak};

#[derive(Debug, serde::Serialize)]
pub(crate) struct OriginalChildRunningIntent {
    pub(crate) original: crate::runtime_db::child_task_terminal::OriginalChildTaskAddress,
    before: owner::OriginalCommandTaskBefore,
    task_digest: String,
    message_digest: String,
    #[serde(skip)]
    material: tasks::TaskMaintenanceMaterial,
}
impl OriginalChildRunningIntent {
    pub(crate) fn material_estimate(&self) -> tasks::TaskMaintenanceMaterial {
        tasks::TaskMaintenanceMaterial {
            bytes: self.material.bytes,
            records: self.material.records,
        }
    }
}
#[derive(serde::Serialize)]
pub(super) struct PreparedOriginalChildRunning {
    intent: OriginalChildRunningIntent,
    #[serde(skip)]
    source: Weak<OriginalChildTaskProducer>,
    #[serde(skip)]
    frozen: Arc<FrozenChildTaskRunning>,
    #[serde(skip)]
    pub(super) _guards: Vec<Arc<BudgetViewGuard>>,
}
impl std::fmt::Debug for PreparedOriginalChildRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedOriginalChildRunning")
            .field("intent", &self.intent)
            .finish()
    }
}
impl PreparedInputTransition {
    pub(crate) fn original_child_running(&self) -> Option<&OriginalChildRunningIntent> {
        let InputTransitionCommand::Task(task) = self.command.as_ref() else {
            return None;
        };
        task.original_child_running
            .as_ref()
            .map(|running| &running.intent)
    }
    pub(crate) fn prepare_original_child_running_tx(
        tx: &Transaction<'_>,
        source: &Arc<OriginalChildTaskProducer>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        source.validate_material_ledger(ledger)?;
        source.validate_original_running_source()?;
        let frozen = source
            .frozen_running()?
            .context("original Running body absent")?;
        source.validate_frozen_running(&frozen)?;
        let scope = &source.address().scope;
        let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
        phase.include(frozen.material_bytes, frozen.material_records)?;
        let observation = phase.owner_tx(tx, scope, ledger)?;
        anyhow::ensure!(
            observation.mode == owner::CommandTaskTerminalInstallMode::Current,
            "Running owner has no current State"
        );
        let mut guards = observation.into_material();
        let target = source.original_monitor_binding_target()?;
        let fresh = crate::runtime_db::child_task_terminal::bind_original_child_target_tx(
            tx,
            source,
            target.target_id(),
            target.binding_delivery_id(),
            target.binding_turn_baseline(),
            &phase.remaining()?,
            ledger,
        )?;
        anyhow::ensure!(
            target.same_target(&fresh),
            "Running target incarnation/association changed"
        );
        let usage = fresh.binding_material_estimate();
        phase.include(usage.bytes, usage.records)?;
        guards.extend(fresh.binding_material_guards());
        let key = &source.address().admitted.key;
        let keys = BTreeSet::from([key.clone()]);
        let raw = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
            [&key.id],
            |row| row.get(0),
        )?;
        let records = raw
            .records
            .checked_add(nodes)
            .and_then(|n| n.checked_add(128))
            .and_then(|n| n.checked_mul(8))
            .context("Running Task nodes overflow")?;
        let bytes = raw
            .bytes
            .checked_mul(8)
            .and_then(|n| records.checked_mul(256).and_then(|b| n.checked_add(b)))
            .context("Running Task material overflow")?;
        guards.push(owner::charge_material(
            scope,
            bytes,
            records,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(bytes, records)?;
        let proof = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
        let current = proof.qualified_task_before(&key.id)?;
        anyhow::ensure!(
            current == &frozen.before,
            "Running exact Task baseline changed"
        );
        let actual =
            tasks::prepare_task_sql_readback_tx(tx, scope, &key.id, &phase.remaining()?, ledger)?;
        phase.include(
            tasks::checkpoint_material_size_on(tx, key)?
                .context("Running Task disappeared")?
                .checked_add(key.id.len() + 256)
                .and_then(|n| n.checked_mul(8))
                .context("Running readback material overflow")?,
            8,
        )?;
        anyhow::ensure!(&actual.task == current, "Running Task readback changed");
        let before = owner::OriginalCommandTaskBefore {
            content_digest: actual.content_digest,
            projection_digest: actual.projection_digest,
        };
        guards.push(actual.budget);
        let changed = repositories::task_transition(current, &frozen.task)?
            == repositories::StateTransitionOutcome::Applied;
        let (command, requested) =
            super::task_terminal_business::project_original_child_business_using_phase_tx(
                tx,
                scope,
                frozen.task.clone(),
                Some(&frozen.message),
                frozen.at,
                None,
                owner::CommandTaskTerminalInstallMode::Current,
                false,
                changed,
                "task_status_updated",
                request,
                ledger,
                &mut guards,
                &mut phase,
            )?;
        #[cfg(test)]
        let command = TaskTransitionCommand {
            fault: source.take_running_fault_for_test(),
            ..command
        };
        anyhow::ensure!(
            command.task_result_settlement.is_none()
                && command.queue_entry.is_some()
                && command.agent_state.is_some()
                && requested.as_ref().map(|request| &request.1)
                    == Some(&canonical_command_digest(&frozen.message)?),
            "Running projection changed frozen Message or created a terminal settlement"
        );
        let bytes = usize::try_from(serialized_bytes(&command)?)?
            .checked_mul(8)
            .context("Running prepared copies overflow")?;
        guards.push(owner::charge_material(
            scope,
            bytes,
            64,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(bytes, 64)?;
        let remaining = phase.remaining()?;
        let (count, bytes) = tasks::TaskDomainLoadRequest::bounded_material_limits(
            request.record_limit,
            request.byte_limit,
        );
        let intent = OriginalChildRunningIntent {
            original: source.address().clone(),
            before,
            task_digest: canonical_command_digest(&command.task)?,
            message_digest: canonical_command_digest(&frozen.message)?,
            material: tasks::TaskMaintenanceMaterial {
                bytes: bytes
                    .checked_sub(remaining.byte_limit)
                    .context("Running bytes underflow")?,
                records: count
                    .checked_sub(remaining.record_limit)
                    .context("Running count underflow")?,
            },
        };
        let mut task = PreparedTaskTransition::new(command, Default::default(), false, None, None)?;
        task.original_child_running = Some(PreparedOriginalChildRunning {
            intent,
            source: Arc::downgrade(source),
            frozen: frozen.clone(),
            _guards: guards,
        });
        Self::task(scope.clone(), frozen.nonce.clone(), frozen.at, task)
    }
    pub(crate) fn validate_original_child_running(
        &self,
        source: &Arc<OriginalChildTaskProducer>,
    ) -> Result<()> {
        let running = running(self)?;
        let original = running
            .source
            .upgrade()
            .context("Running source unavailable")?;
        anyhow::ensure!(
            Arc::ptr_eq(&original, source)
                && &running.intent.original == source.address()
                && self.descriptor().scopes() == std::slice::from_ref(&source.address().scope)
                && self.descriptor().command_nonce() == running.frozen.nonce
                && self.descriptor().prepared_at() == running.frozen.at,
            "Running witness changed original scope/source/nonce"
        );
        source.validate_frozen_running(&running.frozen)
    }
}
fn running(prepared: &PreparedInputTransition) -> Result<&PreparedOriginalChildRunning> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        anyhow::bail!("Running omitted Task facade")
    };
    anyhow::ensure!(
        task.original_child_binding.is_none()
            && task.original_child_terminal.is_none()
            && task.original_command_terminal.is_none()
            && task.original_command_creation.is_none()
            && task.stop_intent.is_none()
            && task.child.is_none()
            && !task.synchronize_execution_protocol
            && task.command.agent_deletion.is_none()
            && task.command.task_result_settlement.is_none()
            && task.command.work_items.is_empty()
            && task.command.wait_conditions.is_empty()
            && task.command.expected_wait_conditions.is_empty(),
        "Running combined another lifecycle facet"
    );
    task.original_child_running
        .as_ref()
        .context("original Running facet absent")
}
pub(super) fn validate_task_proof(
    prepared: &PreparedInputTransition,
    proof: &tasks::TaskWriteProof,
) -> Result<()> {
    let running = running(prepared)?;
    let source = running
        .source
        .upgrade()
        .context("Running source unavailable")?;
    prepared.validate_original_child_running(&source)?;
    source.validate_original_running_source()?;
    proof.validate_original_producer_before(
        &running.intent.original.scope,
        &running.intent.original.admitted.key.id,
        &running.intent.before.content_digest,
        &running.intent.before.projection_digest,
    )?;
    let before = proof.task_before(&running.intent.original.admitted.key.id)?;
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    anyhow::ensure!(
        before == &running.frozen.before
            && task.command.task == running.frozen.task
            && canonical_command_digest(&task.command.task)? == running.intent.task_digest
            && task.command.message_evidence.as_slice()
                == std::slice::from_ref(&running.frozen.message)
            && canonical_command_digest(&running.frozen.message)? == running.intent.message_digest,
        "Running exact Task/Message proof changed"
    );
    Ok(())
}
/// Unlike Binding, this proof authorizes exactly one existing Task's Running
/// update plus its sealed Message, and is tied to this actual SQL transaction.
pub(in crate::runtime_db) struct ExistingOriginalChildRunningWriteProof<'a, 'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    before: &'a TaskRecord,
    task: &'a TaskRecord,
    message: &'a MessageEnvelope,
}
impl ExistingOriginalChildRunningWriteProof<'_, '_, '_> {
    pub(in crate::runtime_db) fn validate_transaction(&self, tx: &Transaction<'_>) -> Result<()> {
        anyhow::ensure!(
            std::ptr::eq(self.tx, tx),
            "Running proof crossed transaction"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn validate_existing(
        &self,
        tx: &Transaction<'_>,
        before: &TaskRecord,
        after: &TaskRecord,
    ) -> Result<()> {
        self.validate_transaction(tx)?;
        anyhow::ensure!(
            before == self.before && after == self.task,
            "Running exact update changed"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn task(&self) -> &TaskRecord {
        self.task
    }
    pub(in crate::runtime_db) fn message(&self) -> &MessageEnvelope {
        self.message
    }
}
pub(super) fn existing_write_proof_tx<'a, 'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    prepared: &'a PreparedInputTransition,
    proof: &'a tasks::TaskWriteProof,
) -> Result<ExistingOriginalChildRunningWriteProof<'a, 'tx, 'conn>> {
    validate_task_proof(prepared, proof)?;
    let frozen = &running(prepared)?.frozen;
    anyhow::ensure!(
        matches!(
            frozen.before.status,
            TaskStatus::Queued | TaskStatus::Running
        ) && frozen.task.status == TaskStatus::Running
            && frozen.task.detail == frozen.before.detail
            && frozen.task.recovery == frozen.before.recovery
            && frozen.task.work_item_id == frozen.before.work_item_id
            && frozen.task.updated_at >= frozen.before.updated_at,
        "Running cannot replace protected original Task fields"
    );
    Ok(ExistingOriginalChildRunningWriteProof {
        tx,
        before: &frozen.before,
        task: &frozen.task,
        message: &frozen.message,
    })
}
pub(super) fn existing_input_projection_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
    proof: &tasks::TaskWriteProof,
) -> Result<crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof> {
    let running = running(prepared)?;
    anyhow::ensure!(
        &running.intent.original.scope == scope,
        "Running input scope changed"
    );
    let exact = existing_write_proof_tx(tx, prepared, proof)?;
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    let command = &task.command;
    let queue = command
        .queue_entry
        .as_ref()
        .context("Running lost original Queue responsibility")?;
    let state = command
        .agent_state
        .as_ref()
        .context("Running lost original State accounting")?;
    validate_agent_state_mutation_tx(tx, Some(state))?;
    let old = state
        .expected
        .as_deref()
        .context("Running State omitted exact baseline")?;
    let mut expected = old.clone();
    if expected.current_run_id.is_none()
        && !matches!(
            expected.status,
            crate::types::AgentStatus::Asleep | crate::types::AgentStatus::Stopped
        )
    {
        expected.status = crate::types::AgentStatus::AwakeIdle;
    }
    let (pending, count, queued, present): (usize, usize, bool, bool) = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')),(SELECT COUNT(*) FROM messages WHERE agent_id=?1),EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2 AND status IN ('queued','dequeued','interrupted')),EXISTS(SELECT 1 FROM messages WHERE agent_id=?1 AND evidence_id=?2)",
        params![scope.agent_id, exact.message().id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    )?;
    expected.pending = pending
        .checked_add(usize::from(!queued))
        .context("Running pending overflow")?;
    expected.total_message_count = count
        .checked_add(usize::from(!present))
        .context("Running count overflow")?;
    expected.last_wake_reason = Some("TaskStatus".into());
    crate::runtime::original_task_message_wake_projection(&mut expected);
    anyhow::ensure!(
        queue.message_id == exact.message().id
            && queue.agent_id == scope.agent_id
            && queue.priority == exact.message().priority
            && queue.status == QueueEntryStatus::Queued
            && state.record.as_ref() == &expected
            && task.execution_protocol.bootstrap.is_none()
            && task.execution_protocol.commands.is_empty(),
        "Running Input changed its exact Queue/State footprint"
    );
    let input =
        crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
            tx,
            scope,
            BTreeSet::new(),
            BTreeSet::new(),
        )?;
    crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::
        prepare_from_original_child_running_tx(tx, input, &exact)
}
pub(super) fn preflight_current_entry_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<super::child_terminal::ChildCurrentEntryMaterial> {
    let running = running(prepared)?;
    let source = running
        .source
        .upgrade()
        .context("Running source unavailable")?;
    source.validate_material_ledger(ledger)?;
    source.validate_original_running_source()?;
    let scope = &running.intent.original.scope;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    phase.include(
        running.intent.material.bytes,
        running.intent.material.records,
    )?;
    let owner = phase.owner_tx(tx, scope, ledger)?;
    anyhow::ensure!(
        owner.mode == owner::CommandTaskTerminalInstallMode::Current,
        "Running Current owner/State unavailable"
    );
    let mut guards = owner.into_material();
    let target = source.original_monitor_binding_target()?;
    let fresh = crate::runtime_db::child_task_terminal::bind_original_child_target_tx(
        tx,
        &source,
        target.target_id(),
        target.binding_delivery_id(),
        target.binding_turn_baseline(),
        &phase.remaining()?,
        ledger,
    )?;
    anyhow::ensure!(target.same_target(&fresh), "Running Current target changed");
    let usage = fresh.binding_material_estimate();
    phase.include(usage.bytes, usage.records)?;
    guards.extend(fresh.binding_material_guards());
    // Admit the subsequent Current ScopeFacts and exact Task decodes before
    // either body is materialized, just as the metadata-only Binding path does.
    let (raw, nodes): (usize, usize) = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB)) FROM agent_states WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM tasks WHERE task_id=?2),0),(SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1)+(SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?2)",
        params![scope.agent_id,running.intent.original.admitted.key.id], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    let records = nodes
        .checked_mul(2)
        .and_then(|n| n.checked_add(8))
        .context("Running entry nodes overflow")?;
    let bytes = raw
        .checked_add(512)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| records.checked_mul(256).and_then(|b| n.checked_add(b)))
        .context("Running entry body bytes overflow")?;
    guards.push(owner::charge_material(
        scope,
        bytes,
        records,
        &phase.remaining()?,
        ledger,
    )?);
    phase.include(bytes, records)?;
    let remaining = phase.remaining()?;
    let (count, bytes) = tasks::TaskDomainLoadRequest::bounded_material_limits(
        request.record_limit,
        request.byte_limit,
    );
    Ok(super::child_terminal::ChildCurrentEntryMaterial {
        usage: tasks::TaskMaintenanceMaterial {
            bytes: bytes
                .checked_sub(remaining.byte_limit)
                .and_then(|n| n.checked_sub(running.intent.material.bytes))
                .context("Running entry bytes underflow")?,
            records: count
                .checked_sub(remaining.record_limit)
                .and_then(|n| n.checked_sub(running.intent.material.records))
                .context("Running entry count underflow")?,
        },
        guards,
    })
}
pub(super) fn replay_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<InputMutationExecution<TransitionCommit>>> {
    let running = running(prepared)?;
    let source = running
        .source
        .upgrade()
        .context("Running replay source unavailable")?;
    source.validate_material_ledger(ledger)?;
    prepared.validate_original_child_running(&source)?;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    phase.include(
        running.intent.material.bytes,
        running.intent.material.records,
    )?;
    let (guards, _, _) = crate::runtime_db::child_task_terminal::child_receipt_material_tx(
        tx,
        prepared.descriptor(),
        &running.intent.original.scope,
        &phase.remaining()?,
        ledger,
    )?;
    let Some(receipt) = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())? else {
        return Ok(None);
    };
    anyhow::ensure!(
        matches!(&receipt.outcome.result, InputMutationResult::Task {task_id,deletion:None,stop:None}
            if task_id == &running.intent.original.admitted.key.id),
        "Running replay returned another whole outcome"
    );
    Ok(Some(InputMutationExecution::Replayed {
        receipt,
        _material_budget_views: guards,
    }))
}
