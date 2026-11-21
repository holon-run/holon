//! Terminal Stop status and its original Result publication are separate facts.
use super::*;
use crate::runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
use crate::runtime_db::task_stop::TaskStopResultState;

pub(super) enum TerminalTaskResultSource {
    Message(String),
    PendingOriginalStop,
}

impl RuntimeHandle {
    pub(super) async fn terminal_task_result_source(
        &self,
        mutation: &mut super::super::state_mutation::RuntimeMutation,
        task: &TaskRecord,
    ) -> Result<TerminalTaskResultSource> {
        if let Some(id) = &task.parent_message_id {
            return Ok(TerminalTaskResultSource::Message(id.clone()));
        }
        let request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: std::collections::BTreeSet::from([TaskKey::new(
                TaskFamily::Task,
                &task.id,
            )]),
            recovery_source_ids: Default::default(),
        };
        mutation.prepare_owner_task_request(&request).await?;
        let read = mutation.capture_task_read(&request)?;
        if let Some(original) = read.tasks.stop_head(&task.id)? {
            original.intent.validate_original(task)?;
            anyhow::ensure!(
                task.status == original.ack_task.status,
                "terminal Task differs from its original Stop ACK status"
            );
            if let Some(plan) = &original.intent.result {
                match original.progress.result {
                    TaskStopResultState::Pending => {
                        return Ok(TerminalTaskResultSource::PendingOriginalStop)
                    }
                    TaskStopResultState::Completed
                        if original.result_nonce.as_deref() == Some(plan.phase_nonce.as_str()) =>
                    {
                        return Ok(TerminalTaskResultSource::Message(plan.message.id.clone()));
                    }
                    _ => {}
                }
            }
        }
        Err(RuntimeError::validation(
            "task_result_evidence_missing",
            format!(
                "terminal task has no exact result message identity: {}",
                task.id
            ),
        )
        .with_safe_context("task_id", &task.id)
        .into())
    }
}
