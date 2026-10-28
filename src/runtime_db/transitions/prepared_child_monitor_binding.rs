//! An authentic monitor may persist its exact recovery parameters before Running.
use super::*;
use crate::runtime::{
    input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger},
    OriginalChildTaskProducer,
};
use crate::runtime_db::{command_task_terminal as owner, task_snapshot as tasks};
use std::sync::{Arc, Weak};

#[derive(Debug, serde::Serialize)]
pub(crate) struct OriginalChildMonitorBindingIntent {
    pub(crate) original: crate::runtime_db::child_task_terminal::OriginalChildTaskAddress,
    before: owner::OriginalCommandTaskBefore,
    task_digest: String,
    #[serde(skip)]
    material: tasks::TaskMaintenanceMaterial,
}
impl OriginalChildMonitorBindingIntent {
    pub(crate) fn material_estimate(&self) -> tasks::TaskMaintenanceMaterial {
        tasks::TaskMaintenanceMaterial {
            bytes: self.material.bytes,
            records: self.material.records,
        }
    }
}
#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedOriginalChildMonitorBinding {
    intent: OriginalChildMonitorBindingIntent,
    #[serde(skip)]
    source: Weak<OriginalChildTaskProducer>,
    #[serde(skip)]
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl PreparedInputTransition {
    pub(crate) fn original_child_monitor_binding(
        &self,
    ) -> Option<&OriginalChildMonitorBindingIntent> {
        let InputTransitionCommand::Task(task) = self.command.as_ref() else {
            return None;
        };
        task.original_child_binding
            .as_ref()
            .map(|binding| &binding.intent)
    }
    pub(crate) fn prepare_original_child_monitor_binding_tx(
        tx: &Transaction<'_>,
        source: &Arc<OriginalChildTaskProducer>,
        at: chrono::DateTime<chrono::Utc>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<Self>> {
        source.validate_material_ledger(ledger)?;
        let scope = &source.address().scope;
        let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
        let observed = phase.owner_tx(tx, scope, ledger)?;
        if observed.mode != owner::CommandTaskTerminalInstallMode::Current {
            return Ok(None);
        }
        let mut guards = observed.into_material();
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
            "binding changed actual target incarnation/association"
        );
        let observed = fresh.binding_material_estimate();
        phase.include(observed.bytes, observed.records)?;
        guards.extend(fresh.binding_material_guards());

        let (source_bytes, source_records) = source.monitor_binding_task_material()?;
        guards.push(owner::charge_material(
            scope,
            source_bytes,
            source_records,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(source_bytes, source_records)?;
        let selector = source
            .address()
            .admitted
            .key
            .id
            .len()
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("binding selector overflow")?;
        guards.push(owner::charge_material(
            scope,
            selector,
            16,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(selector, 16)?;
        let key = &source.address().admitted.key;
        let keys = std::collections::BTreeSet::from([key.clone()]);
        let raw = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
        guards.push(owner::charge_material(
            scope,
            raw.bytes,
            raw.records,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(raw.bytes, raw.records)?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
            [&key.id],
            |row| row.get(0),
        )?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(8))
            .context("binding Task nodes overflow")?;
        let bytes = records
            .checked_mul(256)
            .context("binding Task nodes bytes overflow")?;
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
        if !source.validate_monitor_binding_current(current)? {
            return Ok(None);
        }
        let current_raw =
            tasks::checkpoint_material_size_on(tx, key)?.context("binding Task disappeared")?;
        let actual =
            tasks::prepare_task_sql_readback_tx(tx, scope, &key.id, &phase.remaining()?, ledger)?;
        phase.include(
            current_raw
                .checked_add(key.id.len() + 256)
                .and_then(|n| n.checked_mul(8))
                .context("binding readback overflow")?,
            8,
        )?;
        anyhow::ensure!(
            &actual.task == current,
            "binding qualified Task readback differs"
        );
        guards.push(actual.budget);
        let task = source.project_original_monitor_binding(current, at)?;
        let bytes = usize::try_from(serialized_bytes(&task)?)?
            .checked_mul(8)
            .context("binding task copies overflow")?;
        // Both the desired Task and digest/descriptor temporary serialize copies
        // are admitted before constructing this owned command.
        guards.push(owner::charge_material(
            scope,
            bytes,
            records,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(bytes, records)?;
        let command = TaskTransitionCommand {
            agent_id: scope.agent_id.clone(),
            expected_owner_incarnation: scope.identity_incarnation,
            task,
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: Vec::new(),
            expected_wait_conditions: Vec::new(),
            wait_conditions: Vec::new(),
            agent_state: None,
            message_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: false,
            commit_on_idempotent: true,
            fault: None,
        };
        let task_digest = canonical_command_digest(&command.task)?;
        let before = owner::OriginalCommandTaskBefore {
            content_digest: actual.content_digest,
            projection_digest: actual.projection_digest,
        };
        let mut prepared_task =
            PreparedTaskTransition::new(command, Default::default(), false, None, None)?;
        let usage = phase.remaining()?;
        let (record_limit, byte_limit) = tasks::TaskDomainLoadRequest::bounded_material_limits(
            request.record_limit,
            request.byte_limit,
        );
        prepared_task.original_child_binding = Some(PreparedOriginalChildMonitorBinding {
            intent: OriginalChildMonitorBindingIntent {
                original: source.address().clone(),
                before,
                task_digest,
                material: tasks::TaskMaintenanceMaterial {
                    bytes: byte_limit
                        .checked_sub(usage.byte_limit)
                        .context("binding used bytes underflow")?,
                    records: record_limit
                        .checked_sub(usage.record_limit)
                        .context("binding used records underflow")?,
                },
            },
            source: Arc::downgrade(source),
            _guards: guards,
        });
        Ok(Some(Self::task(
            scope.clone(),
            format!("child-binding:{}", source.address().lease_id),
            at,
            prepared_task,
        )?))
    }
}
fn binding(prepared: &PreparedInputTransition) -> Result<&PreparedOriginalChildMonitorBinding> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        anyhow::bail!("binding omitted Task facade")
    };
    anyhow::ensure!(
        task.stop_intent.is_none()
            && task.original_child_terminal.is_none()
            && task.original_command_terminal.is_none()
            && task.child.is_none()
            && !task.synchronize_execution_protocol,
        "binding combined a lifecycle or execution facet"
    );
    let command = &task.command;
    anyhow::ensure!(
        command.agent_deletion.is_none()
            && command.task_result_settlement.is_none()
            && command.queue_entry.is_none()
            && command.work_items.is_empty()
            && command.expected_wait_conditions.is_empty()
            && command.wait_conditions.is_empty()
            && command.agent_state.is_none()
            && command.message_evidence.is_empty()
            && command.audit_events.is_empty()
            && command.index_changes.is_empty()
            && !command.notify_scheduler,
        "binding combined another business write"
    );
    task.original_child_binding
        .as_ref()
        .context("original monitor binding facet absent")
}
pub(super) fn validate_task_proof(
    prepared: &PreparedInputTransition,
    proof: &tasks::TaskWriteProof,
) -> Result<()> {
    let binding = binding(prepared)?;
    let source = binding
        .source
        .upgrade()
        .context("original binding source unavailable")?;
    anyhow::ensure!(
        source.address() == &binding.intent.original,
        "binding changed original source"
    );
    proof.validate_original_producer_before(
        &binding.intent.original.scope,
        &binding.intent.original.admitted.key.id,
        &binding.intent.before.content_digest,
        &binding.intent.before.projection_digest,
    )?;
    let before = proof.task_before(&binding.intent.original.admitted.key.id)?;
    anyhow::ensure!(
        source.validate_monitor_binding_current(before)?,
        "binding current Task/target unavailable"
    );
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    anyhow::ensure!(
        canonical_command_digest(&task.command.task)? == binding.intent.task_digest,
        "binding desired Task body changed"
    );
    source
        .source_view()?
        .validate_generation(&task.command.task)?;
    Ok(())
}
pub(super) fn preflight_current_entry_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<super::child_terminal::ChildCurrentEntryMaterial> {
    let binding = binding(prepared)?;
    let source = binding
        .source
        .upgrade()
        .context("original binding source unavailable")?;
    source.validate_material_ledger(ledger)?;
    anyhow::ensure!(
        source.address() == &binding.intent.original,
        "binding Current source differs"
    );
    let scope = &binding.intent.original.scope;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let admitted = binding.intent.material_estimate();
    phase.include(admitted.bytes, admitted.records)?;
    let owner_bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|row|row.get(0))?;
    let owner_bytes = owner_bytes
        .checked_add(scope.agent_id.len() + 256)
        .and_then(|n| n.checked_mul(8))
        .context("binding owner bytes overflow")?;
    let owner = owner::observe_original_owner_tx(tx, scope, &phase.remaining()?, ledger)?;
    phase.include(owner_bytes, 8)?;
    anyhow::ensure!(
        owner.mode == owner::CommandTaskTerminalInstallMode::Current,
        "original monitor binding owner is unavailable"
    );
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
    anyhow::ensure!(
        target.same_target(&fresh),
        "binding Current target incarnation/association changed"
    );
    let target_usage = fresh.binding_material_estimate();
    phase.include(target_usage.bytes, target_usage.records)?;

    let (record_limit, byte_limit) = tasks::TaskDomainLoadRequest::bounded_material_limits(
        request.record_limit,
        request.byte_limit,
    );
    let remaining = phase.remaining()?;
    let mut material = tasks::read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
        .with_admitted_material(tasks::TaskMaintenanceMaterial {
            bytes: byte_limit
                .checked_sub(remaining.byte_limit)
                .context("binding owner byte underflow")?,
            records: record_limit
                .checked_sub(remaining.record_limit)
                .context("binding owner count underflow")?,
        })?;
    let bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1),0)",[&scope.agent_id],|row|row.get(0))?;
    material.charge_ids(
        bytes
            .checked_add(256)
            .and_then(|n| n.checked_mul(2))
            .context("binding State material overflow")?,
        2,
    )?;
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    material.charge_ids(
        nodes
            .checked_mul(2 * 256)
            .context("binding State nodes overflow")?,
        nodes
            .checked_mul(2)
            .context("binding State count overflow")?,
    )?;
    let key = &binding.intent.original.admitted.key;
    material.charge_record(
        key,
        tasks::checkpoint_material_size_on(tx, key)?.context("binding original Task missing")?,
    )?;
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
        [&key.id],
        |row| row.get(0),
    )?;
    material.charge_ids(
        nodes
            .checked_mul(2 * 256)
            .context("binding Task nodes overflow")?,
        nodes
            .checked_mul(2)
            .context("binding Task count overflow")?,
    )?;
    let total = material.material_estimate();
    let admitted = binding.intent.material_estimate();
    let mut guards = material.into_guards();
    guards.extend(owner.into_material());
    guards.extend(fresh.binding_material_guards());
    Ok(super::child_terminal::ChildCurrentEntryMaterial {
        usage: tasks::TaskMaintenanceMaterial {
            bytes: total
                .bytes
                .checked_sub(admitted.bytes)
                .context("binding entry bytes underflow")?,
            records: total
                .records
                .checked_sub(admitted.records)
                .context("binding entry count underflow")?,
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
    let binding = binding(prepared)?;
    let source = binding
        .source
        .upgrade()
        .context("original binding source unavailable")?;
    source.validate_material_ledger(ledger)?;
    anyhow::ensure!(
        &binding.intent.original == source.address(),
        "binding replay changed original source"
    );
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let usage = binding.intent.material_estimate();
    phase.include(usage.bytes, usage.records)?;
    let (guards, _, _) = crate::runtime_db::child_task_terminal::child_receipt_material_tx(
        tx,
        prepared.descriptor(),
        &binding.intent.original.scope,
        &phase.remaining()?,
        ledger,
    )?;
    let Some(receipt) = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())? else {
        return Ok(None);
    };
    anyhow::ensure!(
        matches!(&receipt.outcome.result,InputMutationResult::Task{task_id,deletion:None,stop:None}
        if task_id==&binding.intent.original.admitted.key.id),
        "binding replay returned another outcome"
    );
    Ok(Some(InputMutationExecution::Replayed {
        receipt,
        _material_budget_views: guards,
    }))
}

/// Metadata for an already accepted physical monitor. This proof neither
/// inserts a Task nor admits Message/Queue work across a cleanup fence.
pub(in crate::runtime_db) struct ExistingOriginalChildBindingWriteProof<'a, 'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    before: &'a crate::types::TaskRecord,
    after: &'a crate::types::TaskRecord,
}
impl ExistingOriginalChildBindingWriteProof<'_, '_, '_> {
    pub(in crate::runtime_db) fn validate_transaction(&self, tx: &Transaction<'_>) -> Result<()> {
        anyhow::ensure!(
            std::ptr::eq(self.tx, tx),
            "child binding write crossed actual transaction"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn validate_existing(
        &self,
        tx: &Transaction<'_>,
        before: &crate::types::TaskRecord,
        after: &crate::types::TaskRecord,
    ) -> Result<()> {
        self.validate_transaction(tx)?;
        anyhow::ensure!(
            self.before == before && self.after == after,
            "child binding write changed exact before/after"
        );
        Ok(())
    }
}
pub(super) fn existing_write_proof_tx<'a, 'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    prepared: &'a PreparedInputTransition,
    proof: &'a tasks::TaskWriteProof,
) -> Result<ExistingOriginalChildBindingWriteProof<'a, 'tx, 'conn>> {
    validate_task_proof(prepared, proof)?;
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    let before = proof.task_before(&task.command.task.id)?;
    let after = &task.command.task;
    anyhow::ensure!(
        before.id == after.id
            && before.agent_id == after.agent_id
            && before.kind == after.kind
            && before.status == after.status
            && before.created_at == after.created_at
            && before.parent_message_id == after.parent_message_id
            && before.work_item_id == after.work_item_id
            && before.summary == after.summary
            && before.recovery == after.recovery
            && after.updated_at >= before.updated_at,
        "child binding changed a non-metadata Task field"
    );
    for name in [
        "rejoin_obligation_id",
        "rejoin_generation",
        "parent_turn_id",
        "work_item_id",
        "cancel_requested",
        "force_stop_requested",
    ] {
        anyhow::ensure!(
            before.detail.as_ref().and_then(|v| v.get(name))
                == after.detail.as_ref().and_then(|v| v.get(name)),
            "child binding changed protected Task detail"
        );
    }
    Ok(ExistingOriginalChildBindingWriteProof { tx, before, after })
}

pub(super) fn existing_input_projection_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
) -> Result<crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof> {
    let prepared_binding = binding(prepared)?;
    let source = prepared_binding
        .source
        .upgrade()
        .context("original binding source unavailable")?;
    anyhow::ensure!(
        &prepared_binding.intent.original.scope == scope
            && source.address() == &prepared_binding.intent.original,
        "metadata-only binding crossed original input scope"
    );
    // There is no Message, Turn, Queue, pending ticket or Input write in this
    // facet. This empty D3 projection cannot grant active Task write authority.
    crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
        tx,
        scope,
        BTreeSet::new(),
        BTreeSet::new(),
    )
}
