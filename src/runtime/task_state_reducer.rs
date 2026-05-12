use super::{scheduler, *};

pub(super) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    scheduler::is_terminal_task_status(status)
}

pub(super) fn should_ignore_task_update(storage: &AppStorage, task: &TaskRecord) -> Result<bool> {
    let Some(latest) = storage.latest_task_record(&task.id)? else {
        return Ok(false);
    };

    if is_terminal_task_status(&latest.status)
        && is_terminal_task_status(&task.status)
        && latest.status != task.status
    {
        return Ok(true);
    }

    Ok(task_status_phase(&latest.status) > task_status_phase(&task.status))
}

fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

pub(super) struct TaskTransition<'a> {
    pub(super) task: &'a TaskRecord,
    pub(super) event_kind: &'static str,
}

impl<'a> TaskTransition<'a> {
    pub(super) fn new(task: &'a TaskRecord, event_kind: &'static str) -> Self {
        Self { task, event_kind }
    }
}

impl RuntimeHandle {
    pub(super) async fn apply_task_transition(&self, transition: TaskTransition<'_>) -> Result<()> {
        let task = transition.task;
        if should_ignore_task_update(&self.inner.storage, task)? {
            return Ok(());
        }

        self.inner.storage.append_task(task)?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !matches!(
                guard.state.status,
                AgentStatus::Paused | AgentStatus::Stopped
            ) {
                if guard.state.current_run_id.is_none() {
                    scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?;
                } else if task.is_blocking() && !is_terminal_task_status(&task.status) {
                    scheduler::apply_awaiting_task_projection(&mut guard.state);
                }
            }
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner
            .storage
            .append_event(&AuditEvent::new(transition.event_kind, to_json_value(task)))?;
        Ok(())
    }

    pub(super) async fn persist_task_transition(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.apply_task_transition(TaskTransition::new(task, event_kind))
            .await
    }

    pub(super) async fn reduce_task_status_message(&self, task: TaskRecord) -> Result<()> {
        self.persist_task_transition(&task, "task_status_updated")
            .await
    }

    pub(super) async fn reduce_task_result_message(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
    ) -> Result<()> {
        if should_ignore_task_update(&self.inner.storage, &task)? {
            return Ok(());
        }
        self.persist_task_transition(&task, "task_result_received")
            .await?;

        let task_status_label = match task.status {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Running => "running",
            TaskStatus::Queued => "queued",
        };
        let emit_result_brief = should_emit_task_result_brief(&task);
        let result_text = match &message.body {
            MessageBody::Text { text } => {
                format!("Task {} {}: {}", task.id, task_status_label, text)
            }
            MessageBody::Json { value } => {
                format!("Task {} {}: {}", task.id, task_status_label, value)
            }
            MessageBody::Brief { text, .. } => {
                format!("Task {} {}: {}", task.id, task_status_label, text)
            }
        };
        if model_reentry {
            if emit_result_brief {
                let brief = brief::make_task_result(&message.agent_id, &task.id, &result_text);
                self.persist_brief(&brief).await?;
            }
            if let Some(work_item_id) = message
                .work_item_id
                .clone()
                .or_else(|| task.effective_work_item_id().map(ToString::to_string))
            {
                let mut guard = self.inner.agent.lock().await;
                guard.state.current_turn_work_item_id = Some(work_item_id);
                self.inner.storage.write_agent(&guard.state)?;
            }
            self.process_interactive_message(
                message,
                continuation_resolution,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await?;
        } else {
            if emit_result_brief {
                let brief = brief::make_result(&message.agent_id, message, result_text);
                self.persist_brief(&brief).await?;
            }
        }
        Ok(())
    }
}

fn should_emit_task_result_brief(task: &TaskRecord) -> bool {
    task.kind != TaskKind::CommandTask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::ContextConfig,
        provider::StubProvider,
        types::{MessageKind, MessageOrigin, Priority, TrustLevel},
    };
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    fn task(id: &str, status: TaskStatus, blocking: bool) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::ChildAgentTask,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(format!("task {id}")),
            detail: blocking.then(|| json!({ "wait_policy": "blocking" })),
            recovery: None,
        }
    }

    struct RuntimeFixture {
        runtime: RuntimeHandle,
        _dir: TempDir,
        _workspace: TempDir,
    }

    impl std::ops::Deref for RuntimeFixture {
        type Target = RuntimeHandle;

        fn deref(&self) -> &Self::Target {
            &self.runtime
        }
    }

    fn runtime() -> RuntimeFixture {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        RuntimeFixture {
            runtime,
            _dir: dir,
            _workspace: workspace,
        }
    }

    fn task_result_message(task_id: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.into(),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "task finished".into(),
            },
        )
    }

    #[test]
    fn stale_non_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(&storage, &stale).unwrap());
    }

    #[test]
    fn conflicting_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let late_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(should_ignore_task_update(&storage, &late_terminal).unwrap());
    }

    #[test]
    fn repeated_same_terminal_updates_are_preserved_for_result_events() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Failed, true))
            .unwrap();

        let repeated_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(!should_ignore_task_update(&storage, &repeated_terminal).unwrap());
    }

    #[test]
    fn stale_running_update_is_ignored_after_cancelling() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Cancelling, true))
            .unwrap();

        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(&storage, &stale).unwrap());
    }

    #[test]
    fn has_blocking_active_tasks_uses_storage_backed_task_records() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("blocking", TaskStatus::Running, true))
            .unwrap();
        storage
            .append_task(&task("background", TaskStatus::Running, false))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", usize::MAX)
            .unwrap();
        assert!(active.iter().any(|task| task.id == "blocking"));
        assert!(active.iter().any(|task| task.id == "background"));
        assert!(active.iter().any(TaskRecord::is_blocking));
    }

    #[test]
    fn active_task_projection_ignores_terminal_latest_records() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&task("stale", TaskStatus::Running, true))
            .unwrap();
        storage
            .append_task(&task("stale", TaskStatus::Completed, true))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", usize::MAX)
            .unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn task_record_work_item_id_falls_back_to_detail_for_old_records() {
        let mut record = task("task-1", TaskStatus::Running, true);
        record.detail = Some(serde_json::json!({
            "wait_policy": "blocking",
            "work_item_id": "work-old",
        }));

        assert_eq!(record.effective_work_item_id(), Some("work-old"));

        record.work_item_id = Some("work-new".into());
        assert_eq!(record.effective_work_item_id(), Some("work-new"));
    }

    #[tokio::test]
    async fn non_terminal_task_updates_are_visible_in_ledger_projection_and_blocking_state() {
        let runtime = runtime();

        runtime
            .reduce_task_status_message(task("task-1", TaskStatus::Running, true))
            .await
            .unwrap();

        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(active_tasks.iter().any(|task| task.id == "task-1"));
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwaitingTask);
    }

    #[tokio::test]
    async fn task_transition_preserves_active_run_id_during_turn() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeRunning;
            guard.state.current_run_id = Some("run-1".into());
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, true),
                "task_status_updated",
            ))
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.current_run_id.as_deref(), Some("run-1"));
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(active_tasks.iter().any(|task| task.id == "task-1"));
    }

    #[tokio::test]
    async fn terminal_result_falls_back_to_awake_idle_only_when_no_blocking_tasks_remain() {
        let runtime = runtime();
        runtime
            .reduce_task_status_message(task("task-1", TaskStatus::Running, true))
            .await
            .unwrap();
        runtime
            .reduce_task_status_message(task("task-2", TaskStatus::Running, true))
            .await
            .unwrap();

        runtime
            .reduce_task_result_message(
                &task_result_message("task-1"),
                task("task-1", TaskStatus::Completed, true),
                false,
                None,
            )
            .await
            .unwrap();
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwaitingTask);
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(!active_tasks.iter().any(|task| task.id == "task-1"));
        assert!(active_tasks.iter().any(|task| task.id == "task-2"));

        runtime
            .reduce_task_result_message(
                &task_result_message("task-2"),
                task("task-2", TaskStatus::Completed, true),
                false,
                None,
            )
            .await
            .unwrap();
        let final_state = runtime.agent_state().await.unwrap();
        assert_eq!(final_state.status, AgentStatus::AwakeIdle);
        let final_active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(!final_active_tasks.iter().any(|task| task.id == "task-2"));
    }

    #[tokio::test]
    async fn non_model_reentry_task_results_emit_a_result_brief_without_reopening_turn() {
        let runtime = runtime();
        runtime
            .reduce_task_status_message(task("task-1", TaskStatus::Running, false))
            .await
            .unwrap();

        runtime
            .reduce_task_result_message(
                &task_result_message("task-1"),
                task("task-1", TaskStatus::Completed, false),
                false,
                None,
            )
            .await
            .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| {
            brief.kind == crate::types::BriefKind::Result
                && brief.related_task_id.is_none()
                && brief.text.contains("Task task-1 completed")
        }));
        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert!(transcript.is_empty());
    }

    #[tokio::test]
    async fn model_reentry_task_result_binds_turn_to_work_item() {
        let runtime = runtime();
        let mut task = task("task-1", TaskStatus::Completed, false);
        task.work_item_id = Some("work-1".into());
        let mut message = task_result_message("task-1");
        message.work_item_id = Some("work-1".into());

        runtime
            .reduce_task_result_message(&message, task, true, None)
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.current_turn_work_item_id.as_deref(), Some("work-1"));
        assert!(state.last_turn_terminal.is_some());
    }

    #[tokio::test]
    async fn command_task_results_do_not_emit_result_briefs() {
        let runtime = runtime();
        let mut command_task = task("task-1", TaskStatus::Running, false);
        command_task.kind = TaskKind::CommandTask;
        runtime
            .reduce_task_status_message(command_task.clone())
            .await
            .unwrap();

        command_task.status = TaskStatus::Completed;
        command_task.updated_at = Utc::now();
        runtime
            .reduce_task_result_message(&task_result_message("task-1"), command_task, false, None)
            .await
            .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.is_empty());
    }
}
