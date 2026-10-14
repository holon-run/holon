//! Original accepted Command terminals reuse Task's whole receipt and W1 writer.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger},
        resident_task::{TaskFamily, TaskKey},
        {FrozenCommandTaskTerminal, OriginalCommandTaskProducer},
    },
    runtime_db::{command_task_terminal as producer, task_snapshot as tasks},
    types::TaskStatus,
};
use rusqlite::Transaction;
use std::sync::{Arc, Weak};

#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedOriginalCommandTerminal {
    pub(super) intent: producer::OriginalCommandTaskTerminalIntent,
    #[serde(skip)]
    authorization: Weak<OriginalCommandTaskProducer>,
    #[serde(skip)]
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl PreparedInputTransition {
    pub(crate) fn original_command_terminal(
        &self,
    ) -> Option<&producer::OriginalCommandTaskTerminalIntent> {
        let InputTransitionCommand::Task(task) = self.command.as_ref() else {
            return None;
        };
        task.original_command_terminal
            .as_ref()
            .map(|command| &command.intent)
    }
    pub(crate) fn with_original_command_material(
        mut self,
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        let InputTransitionCommand::Task(task) = self.command.as_mut() else {
            anyhow::bail!("Command material requires original Task facade")
        };
        task.original_command_terminal
            .as_mut()
            .context("Command material requires original producer facet")?
            .guards
            .extend(guards);
        Ok(self)
    }
    /// The source capability comes from the real registered Command lease. The
    /// same owner remains held until this frozen business command is attempted.
    pub(crate) fn prepare_original_command_terminal_tx(
        tx: &Transaction<'_>,
        source: &Arc<OriginalCommandTaskProducer>,
        frozen: &FrozenCommandTaskTerminal,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let view = source.source_view()?;
        view.validate_frozen(frozen)?;
        let original = view.address();
        let scope = &original.scope;
        let owner = producer::observe_original_owner_tx(tx, scope, request, ledger)?;
        let mut guards = vec![owner.material()];
        let mut before = None;
        let actual;
        let mut no_op = false;
        let mut task_will_change = false;
        let task = if owner.mode == producer::CommandTaskTerminalInstallMode::Retired {
            frozen.task.clone()
        } else {
            let key = TaskKey::new(TaskFamily::Task, &original.admitted.key.id);
            let keys = std::collections::BTreeSet::from([key.clone()]);
            let material = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
            guards.push(producer::charge_material(
                scope,
                material.bytes,
                material.records,
                request,
                ledger,
            )?);
            let proof = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
            let row = proof.qualified_task_before(&key.id)?;
            view.validate_original_generation(row)?;
            actual = tasks::prepare_task_sql_readback_tx(tx, scope, &key.id, request, ledger)?;
            anyhow::ensure!(
                &actual.task == row,
                "Command qualified Task readback changed"
            );
            before = Some(producer::OriginalCommandTaskBefore {
                content_digest: actual.content_digest.clone(),
                projection_digest: actual.projection_digest.clone(),
            });
            guards.push(actual.budget.clone());
            no_op = crate::runtime::original_task_update_is_ignored(Some(row), &frozen.task);
            task_will_change = !no_op
                && !crate::runtime::original_task_status_is_terminal(&row.status)
                && crate::runtime_db::repositories::task_transition(row, &frozen.task)?
                    == crate::runtime_db::repositories::StateTransitionOutcome::Applied;
            if no_op || crate::runtime::original_task_status_is_terminal(&row.status) {
                row.clone()
            } else {
                let mut task = frozen.task.clone();
                if task.status == TaskStatus::Cancelled {
                    producer::merge_original_cancel_flags(&mut task, row)?;
                }
                task
            }
        };
        let (command, requested) =
            super::task_terminal_business::project_original_terminal_business_tx(
                tx,
                scope,
                task,
                frozen.message.as_ref(),
                frozen.at,
                frozen.take_fault()?,
                owner.mode,
                no_op,
                task_will_change,
                "command_task_terminal_persisted",
                request,
                ledger,
                &mut guards,
            )?;
        let requested =
            requested.map(
                |(message_id, content_digest)| producer::OriginalCommandResultAddress {
                    message_id,
                    content_digest,
                },
            );
        let intent = producer::OriginalCommandTaskTerminalIntent {
            original: original.clone(),
            before,
            mode: owner.mode,
            terminal_digest: canonical_command_digest(&(&frozen.task, &frozen.message, frozen.at))?,
            requested,
            no_op,
        };
        let extra = serialized_bytes(&(&command, &intent))?
            .checked_mul(8)
            .context("Command prepared material overflow")?;
        guards.push(producer::charge_material(
            scope,
            usize::try_from(extra)?,
            32,
            request,
            ledger,
        )?);
        let mut task =
            PreparedTaskTransition::new(command, Default::default(), !no_op, None, None)?;
        task.original_command_terminal = Some(PreparedOriginalCommandTerminal {
            intent,
            authorization: Arc::downgrade(source),
            guards,
        });
        Self::task(scope.clone(), frozen.nonce.clone(), frozen.at, task)
    }
}

fn authorize_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    prepared: &PreparedInputTransition,
    command: &PreparedOriginalCommandTerminal,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(
    producer::CommandTaskOwnerObservation,
    Option<tasks::ExistingTaskMaintenanceProof<'tx, 'connection>>,
)> {
    let source = command
        .authorization
        .upgrade()
        .context("original Command producer capability unavailable")?;
    source.validate_terminal_digest(&command.intent.terminal_digest)?;
    let view = source.source_view()?;
    anyhow::ensure!(
        view.address() == &command.intent.original,
        "Command terminal capability address differs"
    );
    let mut owner =
        producer::observe_original_owner_tx(tx, &command.intent.original.scope, request, ledger)?;
    anyhow::ensure!(
        owner.mode == command.intent.mode,
        "Command original owner changed before terminal commit"
    );
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    anyhow::ensure!(
        task.stop_intent.is_none() && task.child.is_none() && task.command.agent_deletion.is_none(),
        "Command terminal combined another lifecycle authority"
    );
    let proof = if owner.mode != producer::CommandTaskTerminalInstallMode::Retired {
        let keys = std::collections::BTreeSet::from([TaskKey::new(
            TaskFamily::Task,
            &command.intent.original.admitted.key.id,
        )]);
        let material =
            tasks::task_maintenance_material_tx(tx, &command.intent.original.scope, &keys)?;
        // This guard is attached to the returned owner before any full decode.
        let guard = producer::charge_material(
            &command.intent.original.scope,
            material.bytes,
            material.records,
            request,
            ledger,
        )?;
        let proof =
            tasks::prepare_existing_task_maintenance_tx(tx, &command.intent.original.scope, &keys)?;
        let actual = proof.qualified_task_before(&command.intent.original.admitted.key.id)?;
        view.validate_original_generation(actual)?;
        view.validate_original_generation(&task.command.task)?;
        let read = tasks::prepare_task_sql_readback_tx(
            tx,
            &command.intent.original.scope,
            &actual.id,
            request,
            ledger,
        )?;
        anyhow::ensure!(
            command
                .intent
                .before
                .as_ref()
                .is_some_and(|before| before.content_digest == read.content_digest
                    && before.projection_digest == read.projection_digest)
                && &read.task == actual,
            "Command original before-version changed"
        );
        owner.retain(guard);
        owner.retain(read.budget);
        Some(proof)
    } else {
        anyhow::ensure!(
            command.intent.before.is_none(),
            "retired Command terminal retained current Task authority"
        );
        None
    };
    Ok((owner, proof))
}

pub(super) fn replay_or_sparse_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<InputMutationExecution<TransitionCommit>>> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    let original = task
        .original_command_terminal
        .as_ref()
        .context("Command terminal omitted source facet")?;
    let receipt_guard = super::super::prepared_task_result::receipt_budget_tx(
        tx,
        &prepared.descriptor,
        &prepared.primary_scope,
        request,
        ledger,
    )?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
        anyhow::ensure!(
            matches!(&receipt.outcome.result,InputMutationResult::CommandTaskTerminal{ack} if ack.original==original.intent.original),
            "Command replay returned another producer"
        );
        return Ok(Some(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: vec![receipt_guard],
        }));
    }
    if original.intent.mode == producer::CommandTaskTerminalInstallMode::Current {
        return Ok(None);
    }
    let (owner, proof) = authorize_tx(tx, prepared, original, request, ledger)?;
    anyhow::ensure!(
        prepared.memory.is_none()
            && task.command.queue_entry.is_none()
            && task.command.message_evidence.is_empty()
            && task.command.task_result_settlement.is_none()
            && task.command.agent_state.is_none()
            && task.command.work_items.is_empty()
            && task.command.wait_conditions.is_empty(),
        "sparse Command terminal cannot create ordinary dependencies"
    );
    let mut effects = PostCommitEffects::default();
    let mut applied = false;
    let mut task_changed = false;
    let key = TaskKey::new(TaskFamily::Task, &task.command.task.id);
    if let Some(proof) = proof {
        // Ordinary transition validation is retained; this is not the special
        // Interrupted child Stop path and grants no terminal rewrite exception.
        applied = crate::runtime_db::repositories::upsert_task_tx(tx, &task.command.task)?;
        task_changed = applied;
        let actual = if applied {
            std::collections::BTreeSet::from([key.clone()])
        } else {
            Default::default()
        };
        let ack = tasks::register_existing_task_maintenance_tx(tx, proof, &actual)?;
        effects.task_write_keys.extend(ack.qualified_actual_keys);
        effects.task_observation_keys.insert(key);
        // Existing audit bodies may be larger than this frozen event. Charge
        // before the append helper's duplicate-content readback.
        for event in &task.command.audit_events {
            let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)+COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)",[&event.id],|r|r.get(0))?;
            effects
                .task_material_budget_views
                .push(producer::charge_material(
                    &prepared.primary_scope,
                    bytes
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(8))
                        .context("Command audit material overflow")?,
                    8,
                    request,
                    ledger,
                )?);
        }
        if task_changed {
            crate::runtime_db::evidence::insert_runtime_index_changes_at_tx(
                tx,
                &task.command.index_changes,
                prepared.descriptor.prepared_at(),
            )?;
        }
        for event in &task.command.audit_events {
            let (event, inserted) = crate::runtime_db::evidence::append_audit_event_tx(
                tx,
                Some(&prepared.primary_scope.agent_id),
                event,
            )?;
            applied |= inserted;
            effects.audit_events.push(event);
        }
        effects.notify_memory_index = task_changed;
    }
    let ack = producer::CommandTaskTerminalAck {
        original: original.intent.original.clone(),
        install_mode: owner.mode,
        task_changed,
        result: producer::CommandTaskTerminalResult::Unavailable {
            reason: if owner.mode == producer::CommandTaskTerminalInstallMode::NoState {
                producer::CommandTaskTerminalUnavailable::OwnerNoState
            } else {
                producer::CommandTaskTerminalUnavailable::ScopeRetired
            },
        },
    };
    effects
        .task_material_budget_views
        .extend(original.guards.iter().cloned());
    effects
        .task_material_budget_views
        .extend(owner.into_material());
    effects.fault = task.command.fault;
    let install_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            &prepared.primary_scope,
            &effects,
        )?;
    let outcome = InputMutationOutcome {
        result: InputMutationResult::CommandTaskTerminal { ack: Box::new(ack) },
        applied,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: prepared.primary_scope.clone(),
            install_keys,
            audit_events: effects.audit_events.clone(),
            notify_memory_index: effects.notify_memory_index,
            notify_scheduler: false,
        }],
    };
    let receipt = insert_input_mutation_receipt_tx(tx, &prepared.descriptor, &outcome, Utc::now())?;
    inject_fault(task.command.fault, TransitionFaultPoint::BeforeCommit)?;
    Ok(Some(InputMutationExecution::Committed {
        value: TransitionCommit {
            applied,
            effects: Box::new(effects),
            ..Default::default()
        },
        receipt,
    }))
}
pub(super) fn current_ack(
    original: &PreparedOriginalCommandTerminal,
    value: &TransitionCommit,
) -> Result<InputMutationResult> {
    anyhow::ensure!(
        original.intent.mode == producer::CommandTaskTerminalInstallMode::Current,
        "current Command ACK has sparse mode"
    );
    let result = if original.intent.no_op {
        producer::CommandTaskTerminalResult::NoOp
    } else if let Some(message) = &original.intent.requested {
        producer::CommandTaskTerminalResult::Delivered {
            message_id: message.message_id.clone(),
        }
    } else {
        producer::CommandTaskTerminalResult::NotRequested
    };
    Ok(InputMutationResult::CommandTaskTerminal {
        ack: Box::new(producer::CommandTaskTerminalAck {
            original: original.intent.original.clone(),
            install_mode: original.intent.mode,
            task_changed: value.effects.task_write_keys.contains(&TaskKey::new(
                TaskFamily::Task,
                &original.intent.original.admitted.key.id,
            )),
            result,
        }),
    })
}

pub(super) fn validate_current_task_proof(
    prepared: &PreparedInputTransition,
    proof: &tasks::TaskWriteProof,
) -> Result<()> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    let command = task
        .original_command_terminal
        .as_ref()
        .context("Command original facet missing")?;
    anyhow::ensure!(
        command.intent.mode == producer::CommandTaskTerminalInstallMode::Current
            && task.stop_intent.is_none()
            && task.child.is_none(),
        "Command current authority differs"
    );
    let source = command
        .authorization
        .upgrade()
        .context("original Command producer unavailable")?;
    source.validate_terminal_digest(&command.intent.terminal_digest)?;
    let view = source.source_view()?;
    anyhow::ensure!(
        view.address() == &command.intent.original,
        "Command original capability differs"
    );
    let before = command
        .intent
        .before
        .as_ref()
        .context("current Command original version missing")?;
    proof.validate_original_producer_before(
        &command.intent.original.scope,
        &command.intent.original.admitted.key.id,
        &before.content_digest,
        &before.projection_digest,
    )?;
    view.validate_original_generation(
        proof.task_before(&command.intent.original.admitted.key.id)?,
    )?;
    view.validate_original_generation(&task.command.task)?;
    Ok(())
}

pub(super) fn prepare_input_view_tx<'tx, 'connection, 'command>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    proof: Option<&tasks::TaskWriteProof>,
) -> Result<Option<producer::OriginalCommandTaskInputView<'tx, 'connection, 'command>>> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        return Ok(None);
    };
    let Some(original) = task.original_command_terminal.as_ref() else {
        return Ok(None);
    };
    if task.command.task_result_settlement.is_some() || task.command.message_evidence.is_empty() {
        return Ok(None);
    };
    let [message] = task.command.message_evidence.as_slice() else {
        anyhow::bail!("Command producer has multiple terminal sources")
    };
    let source = original
        .authorization
        .upgrade()
        .context("original Command source unavailable")?;
    producer::OriginalCommandTaskInputView::prepare_tx(
        tx,
        &source,
        proof.context("Command input omitted W1 proof")?,
        &task.command.task,
        message,
        &original.intent,
    )
    .map(Some)
}
