//! Original Control metadata source + selected Task heads share one whole receipt.
use super::*;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger,
    resident_task::{TaskFamily, TaskKey},
};
use crate::runtime_db::{task_snapshot as tasks, task_status_metadata as status};
use std::sync::Arc;

#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedTaskStatusMetadataCommand {
    #[serde(serialize_with = "serialize_frozen")]
    frozen: Arc<status::FrozenControlWorktreeCompletion>,
    #[serde(skip)]
    selected: Option<status::TaskStatusMetadataSelection>,
    #[serde(skip)]
    fault: Option<TransitionFaultPoint>,
}
fn serialize_frozen<S: serde::Serializer>(
    value: &Arc<status::FrozenControlWorktreeCompletion>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serde::Serialize::serialize(value.as_ref(), serializer)
}
impl PreparedTaskStatusMetadataCommand {
    pub(super) fn scope(&self) -> &InputEvidenceScope {
        self.frozen.scope()
    }
}
impl PreparedInputTransition {
    pub(crate) fn control_worktree_metadata(
        frozen: Arc<status::FrozenControlWorktreeCompletion>,
        nonce: String,
        selected: Option<status::TaskStatusMetadataSelection>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let scope = frozen.scope().clone();
        let at = frozen.at;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::TaskStatusMetadata(PreparedTaskStatusMetadataCommand {
                frozen,
                selected,
                fault,
            }),
        )
    }
    pub(crate) fn task_status_metadata_scope(&self) -> Option<&InputEvidenceScope> {
        let InputTransitionCommand::TaskStatusMetadata(command) = self.command.as_ref() else {
            return None;
        };
        Some(command.scope())
    }
}

pub(super) fn commit_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    command: &PreparedTaskStatusMetadataCommand,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = command.scope();
    anyhow::ensure!(
        command.frozen.completion.matches_ledger(ledger),
        "Control completion material belongs to another actual ledger"
    );
    anyhow::ensure!(
        prepared.primary_scope == *scope
            && prepared.descriptor.scopes() == std::slice::from_ref(scope),
        "Control metadata requires only its original scope"
    );
    let mut material = status::PhaseMaterial::new(request);
    material.include(command.frozen.bytes, command.frozen.records)?;
    if let Some(selected) = &command.selected {
        material.include(selected.bytes, selected.records_count)?;
    }
    let (receipt_guard, receipt_bytes, receipt_records) =
        super::super::prepared_task_result::receipt_budget_with_material_tx(
            tx,
            prepared.descriptor(),
            scope,
            &material.remaining()?,
            ledger,
        )?;
    material.include(receipt_bytes, receipt_records)?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())? {
        return Ok(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: vec![receipt_guard],
        });
    }
    let mut guards = command.frozen.guards.clone();
    guards.push(receipt_guard);
    let owner = material.owner_tx(tx, scope, ledger)?;
    let mode: status::TaskStatusMetadataInstallMode = owner.mode.into();
    guards.extend(owner.into_material());
    let (pool, pool_bytes, pool_records) = status::control_completion_material_tx(
        tx,
        scope,
        &command.frozen.key,
        &material.remaining()?,
        ledger,
    )?;
    material.include(pool_bytes, pool_records)?;
    guards.extend(pool);
    let changed = if command.frozen.completed {
        crate::runtime_db::control_transition::complete_control_obligation_tx(
            tx,
            &command.frozen.key,
            &command.frozen.target,
            command.frozen.at,
            &command.frozen.evidence,
        )?
    } else {
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4 AND target_json=?5)",rusqlite::params![scope.agent_id,scope.identity_incarnation,command.frozen.key.command_id,command.frozen.target.obligation_id,serde_json::to_string(&command.frozen.target)?],|r|r.get(0))?;
        anyhow::ensure!(
            exact,
            "Control failed cleanup audit differs from original target"
        );
        false
    };
    guards.extend(status::audit_material_tx(
        tx,
        scope,
        &command.frozen.audits,
        &mut material,
        ledger,
    )?);
    let mut audit_inserted = false;
    for event in &command.frozen.audits {
        let (_, inserted) =
            crate::runtime_db::evidence::append_audit_event_tx(tx, Some(&scope.agent_id), event)?;
        audit_inserted |= inserted;
    }
    let mut effects = PostCommitEffects::default();
    if command.frozen.completed && mode == status::TaskStatusMetadataInstallMode::Current {
        if let Some(selected) = &command.selected {
            anyhow::ensure!(
                selected
                    .records
                    .iter()
                    .all(|record| record.locator.key.id == command.frozen.task_id()),
                "Control metadata selection differs from its original Task"
            );
            if let Some((delta, views)) = tasks::refresh_task_status_metadata_tx(
                tx,
                scope,
                selected.revision,
                &selected.records,
                &material.remaining()?,
                ledger,
                None,
            )? {
                guards.extend(views);
                effects.task_delta = Some(delta);
                effects.task_delta_requires_prepared_domain = true;
            }
            effects.task_observation_keys.extend(
                selected
                    .records
                    .iter()
                    .map(|record| TaskKey::new(TaskFamily::Task, &record.locator.key.id)),
            );
        }
    }
    let ack = status::TaskStatusMetadataAck {
        key: command.frozen.key.clone(),
        obligation_id: command.frozen.target.obligation_id.clone(),
        target_digest: canonical_command_digest(&command.frozen.target)?,
        source_changed: changed,
        install_mode: mode,
        task_revision: effects.task_delta.as_ref().map(|delta| delta.revision),
    };
    effects.task_material_budget_views.extend(guards);
    effects.fault = command.fault;
    let applied = changed || audit_inserted || effects.task_delta.is_some();
    let install_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            scope, &effects,
        )?;
    let outcome = InputMutationOutcome {
        result: InputMutationResult::TaskStatusMetadata { ack: Box::new(ack) },
        applied,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope.clone(),
            install_keys,
            audit_events: vec![],
            notify_memory_index: false,
            notify_scheduler: false,
        }],
    };
    let receipt =
        insert_input_mutation_receipt_tx(tx, prepared.descriptor(), &outcome, command.frozen.at)?;
    inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
    Ok(InputMutationExecution::Committed {
        value: TransitionCommit {
            applied,
            effects: Box::new(effects),
            ..Default::default()
        },
        receipt,
    })
}
