//! Explicit proof that a live-input writer can retain its original Task Arc.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_task::{ResidentTaskDomain, TaskKey},
};
use crate::runtime_db::task_snapshot::{self as tasks, unchanged};
use std::sync::Arc;

#[cfg(test)]
thread_local! {
    static FULL_TASK_ORACLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run the existing finalizer as a differential oracle, on this thread only.
#[cfg(test)]
pub(super) fn with_full_task_oracle<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FULL_TASK_ORACLE.set(self.0);
        }
    }
    let _restore = Restore(FULL_TASK_ORACLE.replace(true));
    run()
}

pub(super) struct UnchangedLiveTask {
    _resident: Arc<ResidentTaskDomain>,
    allowed: BTreeSet<TaskKey>,
    guards: Vec<Arc<BudgetViewGuard>>,
}

impl UnchangedLiveTask {
    pub(super) fn prepare_tx(
        tx: &Transaction<'_>,
        prepared: &PreparedInputTransition,
        scope: &InputEvidenceScope,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        bases: Option<&PreparedTaskBaselines>,
    ) -> Result<Option<Self>> {
        #[cfg(test)]
        if FULL_TASK_ORACLE.get() {
            return Ok(None);
        }
        let (messages, queues, attempt) = match prepared.command.as_ref() {
            InputTransitionCommand::LiveInputAttachment(command) => (
                command.messages.as_slice(),
                command.queues.as_slice(),
                &command.expected_attempt,
            ),
            InputTransitionCommand::LiveInputRecovery(command) => {
                (&[][..], &[][..], &command.expected_attempt)
            }
            InputTransitionCommand::ProviderResultHandoff(command) => {
                (&[][..], &[][..], &command.expected_attempt)
            }
            _ => return Ok(None),
        };
        let _check = attribution::TASK_LIVE_DEPENDENCY_CHECK.start();
        let Some(resident) = bases
            .and_then(|bases| bases.resident(scope))
            .or_else(|| prepared.task_base(scope).map(|base| &base.resident))
        else {
            return Ok(None);
        };
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
                .with_memory_budget(memory);
        // Scalar probes return no owned SQL bodies. Charge their finite address
        // copies and the retained proof before constructing the Result selector.
        let addresses = match prepared.command.as_ref() {
            InputTransitionCommand::LiveInputAttachment(command) => {
                crate::runtime::input_evidence_budget::serialized_bytes(
                    &command.batch.result_identities,
                )? as usize
            }
            _ => attempt.attempt_id.len(),
        };
        material.charge_ids(
            addresses
                .checked_add(scope.agent_id.len() + 1024)
                .context("live Task observation address overflow")?,
            messages
                .len()
                .checked_add(queues.len() + 4)
                .context("live Task observation count overflow")?,
        )?;
        if !unchanged::resident_is_current_tx(tx, scope, resident, request)?
            || live_input::has_result_writes_tx(tx, prepared)?
        {
            return Ok(None);
        }
        for message in messages {
            if !unchanged::message_is_unrelated_tx(tx, scope, message)? {
                return Ok(None);
            }
        }
        for queue in queues {
            if !unchanged::queue_stays_live_tx(tx, scope, queue)? {
                return Ok(None);
            }
        }
        let _unchanged = attribution::TASK_LIVE_UNCHANGED.start();
        Ok(Some(Self {
            _resident: Arc::clone(resident),
            allowed: BTreeSet::new(),
            guards: material.into_guards(),
        }))
    }

    pub(super) fn allowed_keys(&self) -> &BTreeSet<TaskKey> {
        &self.allowed
    }

    pub(super) fn finish(self, value: &mut TransitionCommit) -> Result<()> {
        let effects = &mut value.effects;
        anyhow::ensure!(
            effects.task_write_keys.is_empty()
                && effects.task_observation_keys.is_empty()
                && effects.task_delta.is_none()
                && effects.tasks.is_empty()
                && effects.execution_protocol.is_none()
                && effects.input_evidence_delta.removals.is_empty()
                && effects
                    .input_evidence_delta
                    .output_recall_invalidation
                    .is_none(),
            "independent live input changed a Task dependency"
        );
        effects.task_material_budget_views.extend(self.guards);
        Ok(())
    }
}
