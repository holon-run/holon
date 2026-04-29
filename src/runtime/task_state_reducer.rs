use super::*;

pub(super) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(super) fn should_ignore_task_update(storage: &AppStorage, task: &TaskRecord) -> Result<bool> {
    let Some(latest) = storage.latest_task_record(&task.id)? else {
        return Ok(false);
    };

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

pub(super) fn has_blocking_active_tasks(
    storage: &AppStorage,
    active_task_ids: &[String],
) -> Result<bool> {
    if active_task_ids.is_empty() {
        return Ok(false);
    }
    let tasks = storage.latest_task_records()?;
    Ok(active_task_ids.iter().any(|task_id| {
        tasks
            .iter()
            .find(|task| &task.id == task_id)
            .is_some_and(TaskRecord::is_blocking)
    }))
}

impl RuntimeHandle {
    pub(super) async fn reduce_task_status_message(&self, task: TaskRecord) -> Result<()> {
        if should_ignore_task_update(&self.inner.storage, &task)? {
            return Ok(());
        }

        self.inner.storage.append_task(&task)?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !guard.state.active_task_ids.contains(&task.id) {
                guard.state.active_task_ids.push(task.id.clone());
            }
            if task.is_blocking()
                && !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                )
            {
                guard.state.status = AgentStatus::AwaitingTask;
            }
            guard.state.current_run_id = None;
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "task_status_updated",
            to_json_value(&task),
        ))?;
        Ok(())
    }

    pub(super) async fn reduce_task_result_message(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_visible: bool,
        continuation_resolution: Option<&ContinuationResolution>,
    ) -> Result<()> {
        if should_ignore_task_update(&self.inner.storage, &task)? {
            return Ok(());
        }

        self.inner.storage.append_task(&task)?;
        {
            let mut guard = self.inner.agent.lock().await;
            if is_terminal_task_status(&task.status) {
                guard.state.active_task_ids.retain(|id| id != &task.id);
                if !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                ) {
                    guard.state.status = if has_blocking_active_tasks(
                        &self.inner.storage,
                        &guard.state.active_task_ids,
                    )? {
                        AgentStatus::AwaitingTask
                    } else {
                        AgentStatus::AwakeIdle
                    };
                }
            } else {
                if !guard.state.active_task_ids.contains(&task.id) {
                    guard.state.active_task_ids.push(task.id.clone());
                }
                if task.is_blocking()
                    && !matches!(
                        guard.state.status,
                        AgentStatus::Paused | AgentStatus::Stopped
                    )
                {
                    guard.state.status = AgentStatus::AwaitingTask;
                }
            }
            guard.state.current_run_id = None;
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "task_result_received",
            to_json_value(&task),
        ))?;

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
        if model_visible {
            if emit_result_brief {
                let brief = brief::make_task_result(&message.agent_id, &task.id, &result_text);
                self.persist_brief(&brief).await?;
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

        let active_task_ids = vec!["blocking".to_string()];
        assert!(has_blocking_active_tasks(&storage, &active_task_ids).unwrap());
        let no_blocking = vec!["background".to_string()];
        assert!(!has_blocking_active_tasks(&storage, &no_blocking).unwrap());
    }

    #[tokio::test]
    async fn non_terminal_task_updates_add_missing_active_task_ids_and_blocking_state() {
        let runtime = runtime();

        runtime
            .reduce_task_status_message(task("task-1", TaskStatus::Running, true))
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        assert!(state.active_task_ids.contains(&"task-1".to_string()));
        assert_eq!(state.status, AgentStatus::AwaitingTask);
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
        assert!(!state.active_task_ids.contains(&"task-1".to_string()));
        assert!(state.active_task_ids.contains(&"task-2".to_string()));

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
        assert!(!final_state.active_task_ids.contains(&"task-2".to_string()));
    }

    #[tokio::test]
    async fn non_model_visible_task_results_emit_a_result_brief_without_reopening_turn() {
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
