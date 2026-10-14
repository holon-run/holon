//! Typed Stop ACKs retain their original result before notification awaits.
use super::super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
};
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationFamily, InputMutationResult, TaskStopReceiptBinding},
    task_stop::{TaskStopPhaseAck, TaskStopPhaseInstallMode},
    transitions::PreparedInputTransition,
};

pub(in crate::runtime) struct TaskStopInitialRead {
    pub(in crate::runtime) binding: TaskStopReceiptBinding,
    _material: Arc<BudgetViewGuard>,
}
pub(in crate::runtime) struct TaskStopPhaseRead {
    pub(in crate::runtime) ack: Box<TaskStopPhaseAck>,
    _material: Arc<BudgetViewGuard>,
}
impl RuntimeMutation {
    /// The initial command cannot signal, admit its Result, or delete a child.
    pub(in crate::runtime) async fn commit_initial_task_stop(
        &mut self,
        prepared: PreparedInputTransition,
    ) -> Result<TaskStopInitialRead> {
        let original = prepared
            .initial_task_stop_intent()
            .context("initial TaskStop adapter requires its frozen intent")?;
        anyhow::ensure!(
            prepared.descriptor().family() == InputMutationFamily::Task
                && original.task.scope == self.input_evidence_scope(),
            "initial TaskStop adapter namespace differs"
        );
        // The descriptor is held by the original command budget across SQL.
        let task_id = original.task.key.id.as_str();
        let bytes = serialized_bytes(&task_id)?;
        let address = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: bytes,
                },
            )?;
        let task_id = task_id.to_owned();
        let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
        let InputMutationResult::Task {
            task_id: actual,
            stop: Some(binding),
            deletion: None,
        } = result
        else {
            bail!("initial TaskStop receipt has another result family");
        };
        anyhow::ensure!(
            actual == task_id && binding.scope == self.input_evidence_scope(),
            "initial TaskStop ACK differs from the original Task"
        );
        let guard = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &binding.scope,
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: serialized_bytes(&binding)?,
                },
            )?;
        let result = TaskStopInitialRead {
            binding,
            _material: guard,
        };
        drop(address);
        self.runtime.apply_transition_commit(commit).await;
        Ok(result)
    }

    /// Sparse or retired phases must use the original owner's metadata actor.
    pub(in crate::runtime) async fn commit_current_task_stop_phase(
        &mut self,
        prepared: PreparedInputTransition,
    ) -> Result<TaskStopPhaseRead> {
        let prepared = prepared.for_ready_runtime_task_stop_phase()?;
        let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
        let InputMutationResult::TaskStopPhase { ack } = result else {
            bail!("TaskStop progress receipt has another result family");
        };
        anyhow::ensure!(
            ack.install_mode == TaskStopPhaseInstallMode::Current,
            "Ready TaskStop adapter received a sparse ACK"
        );
        // SQL/replay guards remain in commit until the returned ACK is observed.
        let guard = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: serialized_bytes(ack.as_ref())?,
                },
            )?;
        let result = TaskStopPhaseRead {
            ack,
            _material: guard,
        };
        self.runtime.apply_transition_commit(commit).await;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::StubProvider,
        runtime::{
            command_task::{CommandTaskHandle, ManagedTaskHandle},
            control_cancellation::{ControlCancellationRegistry, SharedControlSignal},
            resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
            task_stop_intent::FrozenTaskStopIntent,
        },
        runtime_db::transitions::{PreparedTaskTransition, TaskTransitionCommand},
        types::{TaskKind, TaskRecord, TaskStatus},
    };
    use tempfile::TempDir;
    use tokio::sync::{mpsc, oneshot};

    async fn fixture() -> Result<(TempDir, RuntimeHandle, TaskRecord)> {
        let home = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            crate::context::ContextConfig::default(),
        )?;
        let at = runtime.now();
        let task = TaskRecord {
            id: "task:runtime-stop".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"cancel_requested": false})),
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "runtime_stop_fixture")
            .await?;
        Ok((home, runtime, task))
    }
    fn command(task: TaskRecord) -> TaskTransitionCommand {
        TaskTransitionCommand {
            agent_id: task.agent_id.clone(),
            task,
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![],
            index_changes: vec![],
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault: None,
        }
    }
    #[tokio::test]
    async fn initial_stop_adapter_publishes_original_ack_before_any_signal_and_phase_replays_once(
    ) -> Result<()> {
        let (_home, runtime, task) = fixture().await?;
        let ledger = &runtime.inner.input_evidence_budget;
        let (cancel, mut cancel_rx) = oneshot::channel();
        let (force, mut force_rx) = oneshot::channel();
        let cancel = SharedControlSignal::new(cancel);
        let force = SharedControlSignal::new(force);
        let lease =
            ControlCancellationRegistry::default().command(&task, cancel.clone(), force.clone());
        let (input_tx, _) = mpsc::channel(1);
        let handle = ManagedTaskHandle::Command(CommandTaskHandle {
            cancel_tx: Some(cancel),
            force_stop_tx: Some(force),
            input_tx,
            control_lease: Some(lease),
        });
        let mut mutation = runtime.begin_mutation().await?;
        let mut request = TaskCaptureRequest::current();
        request
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, &task.id));
        mutation.prepare_owner_task_request(&request).await?;
        let captured = mutation.capture_tasks(&request)?;
        let qualified = captured
            .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
            .unwrap();
        let frozen = FrozenTaskStopIntent::prepare(
            qualified,
            Some(&handle),
            runtime.inner.mutation_owner.owner_epoch(),
            None,
            runtime.now(),
            ledger,
        )?;
        let prepare = || {
            PreparedInputTransition::task(
                frozen.intent.task.scope.clone(),
                "runtime-stop-original".into(),
                frozen.task.updated_at,
                PreparedTaskTransition::new(
                    command(frozen.task.clone()),
                    Default::default(),
                    false,
                    None,
                    None,
                )?
                .with_stop_intent(frozen.intent.clone())?,
            )
        };
        let original = mutation.commit_initial_task_stop(prepare()?).await?;
        let capture = mutation.capture_tasks(&request)?;
        let record = capture.stop_head(&task.id)?.unwrap();
        assert_eq!(record.receipt_binding(), original.binding);
        assert_eq!(record.ack_task.status, TaskStatus::Cancelling);
        assert!(matches!(
            cancel_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            force_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let (load, _load_guard) = mutation.owner_task_load_request()?;
        let phase = || {
            PreparedInputTransition::task_stop_safe_progress(
                record,
                "runtime-stop-attention".into(),
                runtime.now(),
                crate::runtime_db::task_stop::TaskStopProgressAction::PhysicalAttention {
                    reason: "fixture has no attached monitor".into(),
                },
                &load,
                ledger,
            )
        };
        // Freeze once: retries must not obtain a new clock or intent digest.
        let first = phase()?;
        let repeated = PreparedInputTransition::task_stop_safe_progress(
            record,
            "runtime-stop-attention".into(),
            first.descriptor().prepared_at(),
            crate::runtime_db::task_stop::TaskStopProgressAction::PhysicalAttention {
                reason: "fixture has no attached monitor".into(),
            },
            &load,
            ledger,
        )?;
        let first = mutation.commit_current_task_stop_phase(first).await?;
        let revision = mutation.capture_tasks(&request)?.coverage.revision;
        let repeated = mutation.commit_current_task_stop_phase(repeated).await?;
        assert_eq!(first.ack, repeated.ack);
        assert_eq!(
            mutation.capture_tasks(&request)?.coverage.revision,
            revision
        );
        assert!(matches!(
            cancel_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            force_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        Ok(())
    }
}
