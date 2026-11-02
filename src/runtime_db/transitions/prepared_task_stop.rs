//! TaskStop's first SQL ACK contains intent only; physical actions belong to its worker.
use super::*;
use crate::runtime_db::task_stop::{PreparedTaskStop, TaskStopSignal};

impl PreparedTaskTransition {
    pub(crate) fn with_stop_intent(mut self, intent: PreparedTaskStop) -> Result<Self> {
        anyhow::ensure!(self.stop_intent.is_none(), "TaskStop intent already frozen");
        self.stop_intent = Some(intent);
        validate_command(&self)?;
        Ok(self)
    }
}

pub(super) fn validate_command(task: &PreparedTaskTransition) -> Result<()> {
    let intent = task
        .stop_intent
        .as_ref()
        .context("TaskStop intent missing")?;
    let command = &task.command;
    intent.validate_original(&command.task)?;
    anyhow::ensure!(
        command.agent_id == intent.task.scope.agent_id && command.task.id == intent.task.key.id,
        "TaskStop command differs from original Task scope"
    );
    // Result is a frozen continuation template inside the intent, not admission.
    anyhow::ensure!(
        command.message_evidence.is_empty()
            && command.queue_entry.is_none()
            && command.task_result_settlement.is_none()
            && command.agent_deletion.is_none()
            && task.child.is_none(),
        "TaskStop initial ACK cannot admit Message/Queue/Result or delete identity"
    );
    anyhow::ensure!(
        command.work_items.is_empty()
            && command.wait_conditions.is_empty()
            && command.expected_wait_conditions.is_empty()
            && command.agent_state.is_none()
            && task.execution_protocol.bootstrap.is_none()
            && task.execution_protocol.commands.is_empty()
            && !task.synchronize_execution_protocol,
        "TaskStop initial ACK must contain only the original Task transition"
    );
    let expected = if intent.kind == crate::types::TaskKind::CommandTask
        && intent.signal != TaskStopSignal::NoHandle
    {
        crate::types::TaskStatus::Cancelling
    } else {
        crate::types::TaskStatus::Cancelled
    };
    anyhow::ensure!(
        command.task.status == expected,
        "TaskStop initial Task status differs from frozen signal"
    );
    if intent.kind == crate::types::TaskKind::CommandTask {
        anyhow::ensure!(
            command
                .task
                .detail
                .as_ref()
                .and_then(|detail| detail.get("cancel_requested"))
                .and_then(serde_json::Value::as_bool)
                == Some(true),
            "TaskStop Command cancel request missing"
        );
        if intent.signal == TaskStopSignal::Force {
            anyhow::ensure!(
                command
                    .task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("force_stop_requested"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
                "TaskStop Force request missing"
            );
        }
    }
    Ok(())
}

/// Scalar-only original child witness. It has no State/activation/write authority,
/// including when the same child incarnation is already Deleted at replay.
pub(super) fn validate_child_readonly_tx(
    tx: &Transaction<'_>,
    intent: &PreparedTaskStop,
) -> Result<()> {
    let Some(child) = &intent.child else {
        return Ok(());
    };
    crate::runtime_db::task_stop::validate_task_stop_readonly_child_tx(tx, child)
}
