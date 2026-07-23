use super::{scheduler, *};
use crate::types::{
    WaitConditionKind, WaitConditionStatus, WakeSource, WorkItemRecord, WorkItemState,
};
use sha2::{Digest, Sha256};

pub(super) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    scheduler::is_terminal_task_status(status)
}

pub(super) fn should_ignore_task_update(latest: Option<TaskRecord>, task: &TaskRecord) -> bool {
    let Some(latest) = latest else {
        return false;
    };

    if is_terminal_task_status(&latest.status)
        && is_terminal_task_status(&task.status)
        && latest.status != task.status
    {
        return true;
    }

    task_status_phase(&latest.status) > task_status_phase(&task.status)
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
        self.apply_task_transition_inner(transition, true).await
    }

    pub(super) async fn apply_task_transition_silent(
        &self,
        transition: TaskTransition<'_>,
    ) -> Result<()> {
        self.apply_task_transition_inner(transition, false).await
    }

    async fn apply_task_transition_inner(
        &self,
        transition: TaskTransition<'_>,
        emit_event: bool,
    ) -> Result<()> {
        let task = transition.task;
        let latest_task = self.inner.runtime_db.tasks().latest(&task.id)?;
        if should_ignore_task_update(latest_task.clone(), task) {
            return Ok(());
        }
        let repeated_terminal = latest_task.as_ref().is_some_and(|latest| {
            is_terminal_task_status(&latest.status)
                && is_terminal_task_status(&task.status)
                && latest.status == task.status
        });
        let persisted_task = if repeated_terminal {
            latest_task.clone().expect("repeated terminal task exists")
        } else {
            task.clone()
        };
        let task_will_change = if repeated_terminal {
            false
        } else {
            latest_task
                .as_ref()
                .map(|latest| {
                    crate::runtime_db::repositories::task_transition(latest, task).map(|outcome| {
                        outcome == crate::runtime_db::repositories::StateTransitionOutcome::Applied
                    })
                })
                .transpose()?
                .unwrap_or(true)
        };

        let agent_id = self.agent_id().await?;
        let mut state = self.agent_state().await?;
        let expected_state = state.clone();
        if !matches!(state.status, AgentStatus::Stopped) && state.current_run_id.is_none() {
            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
        }
        let mut wait_conditions = Vec::new();
        let mut work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        if task_will_change {
            index_changes.extend(self.inner.storage.index_changes_for_task(task)?);
        }
        if emit_event {
            let payload = TaskLifecycleAuditEvent::from_task(&persisted_task);
            let mut event =
                if let Some(kind) = RuntimeEventKind::from_wire_name(transition.event_kind) {
                    AuditEvent::typed(kind, &payload)?
                } else {
                    AuditEvent::legacy(transition.event_kind, to_json_value(&payload))
                };
            if is_terminal_task_status(&persisted_task.status) {
                event.id = stable_terminal_task_event_id(transition.event_kind, &persisted_task);
                event.created_at = persisted_task.updated_at;
            }
            audit_events.push(event);
        }
        if is_terminal_task_status(&task.status) {
            let matching = self
                .inner
                .storage
                .active_wait_conditions_for_agent(&agent_id)?
                .into_iter()
                .filter(|condition| {
                    condition.wake_sources.iter().any(|source| {
                        matches!(source, WakeSource::TaskResult { task_id } if task_id == &task.id)
                    })
                })
                .collect::<Vec<_>>();
            let now = Utc::now();
            let mut resolved_ids = Vec::with_capacity(matching.len());
            let mut updated_work_item_ids = std::collections::BTreeSet::new();
            for condition in matching {
                let mut resolved = condition.clone();
                resolved.status = WaitConditionStatus::Resolved;
                resolved.updated_at = now;
                resolved.resolved_at = Some(now);
                wait_conditions.push(resolved.clone());
                resolved_ids.push(condition.id);
                if resolved.kind == WaitConditionKind::Task {
                    if let Some(work_item_id) = resolved.work_item_id.as_deref() {
                        if updated_work_item_ids.insert(work_item_id.to_string()) {
                            if let Some(existing) =
                                self.inner.runtime_db.work_items().latest(work_item_id)?
                            {
                                if existing.state == WorkItemState::Open
                                    && existing.blocked_by.as_deref()
                                        == Some(resolved.waiting_for.as_str())
                                {
                                    let mut record = WorkItemRecord {
                                        revision: existing.revision + 1,
                                        blocked_by: None,
                                        recheck_at: None,
                                        recheck_consumed_at: None,
                                        updated_at: now,
                                        ..existing.clone()
                                    };
                                    let plan_artifact_changed =
                                        crate::work_item_plan::refresh_plan_artifact_metadata(
                                            self.agent_home().as_path(),
                                            &mut record,
                                        )?;
                                    if plan_artifact_changed {
                                        if let Some(event) =
                                            self.work_item_plan_artifact_refreshed_event(&record)
                                        {
                                            audit_events.push(event);
                                        }
                                    }
                                    audit_events.push(self.work_item_written_event(
                                        "wait_for_task_resolved",
                                        &record,
                                        serde_json::json!({
                                            "wait_condition_id": resolved.id,
                                        }),
                                    ));
                                    index_changes.extend(
                                        self.inner.storage.index_changes_for_work_item(&record)?,
                                    );
                                    work_items.push(
                                        crate::runtime_db::transitions::WorkItemMutation::Update {
                                            record,
                                            expected_revision: existing.revision,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if !resolved_ids.is_empty() {
                audit_events.push(AuditEvent::legacy(
                    "wait_conditions_resolved",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "task_id": task.id,
                        "reason": "task_result",
                        "wait_condition_ids": resolved_ids,
                    }),
                ));
            }
        }
        let commit = self.inner.runtime_db.transitions().commit_task(
            &crate::runtime_db::transitions::TaskTransitionCommand {
                agent_id,
                task: persisted_task,
                work_items,
                wait_conditions,
                agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(expected_state)),
                    record: Box::new(state),
                }),
                audit_events,
                index_changes,
                notify_scheduler: false,
                commit_on_idempotent: emit_event
                    && !task_will_change
                    && is_terminal_task_status(&task.status),
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
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
        if should_ignore_task_update(self.inner.runtime_db.tasks().latest(&task.id)?, &task) {
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
                guard.persist_state(&self.inner.storage)?;
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

fn stable_terminal_task_event_id(event_kind: &str, task: &TaskRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event_kind.as_bytes());
    hasher.update([0]);
    hasher.update(task.id.as_bytes());
    hasher.update([0]);
    if let Some(message_id) = task.parent_message_id.as_deref() {
        hasher.update(message_id.as_bytes());
    }
    hasher.update([0]);
    let status = match task.status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    };
    hasher.update(status.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("event_{}", &digest[..15])
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
        types::{AuthorityClass, MessageKind, MessageOrigin, Priority},
    };
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    fn task_with_kind(id: &str, status: TaskStatus, blocking: bool, kind: TaskKind) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind,
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

    fn task(id: &str, status: TaskStatus, blocking: bool) -> TaskRecord {
        task_with_kind(id, status, blocking, TaskKind::ChildAgentTask)
    }

    fn scheduler_blocking_task(id: &str, status: TaskStatus) -> TaskRecord {
        task_with_kind(id, status, true, TaskKind::SleepJob)
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
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "task finished".into(),
            },
        )
    }

    #[test]
    fn stale_non_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(latest, &stale));
    }

    #[test]
    fn conflicting_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let late_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(should_ignore_task_update(latest, &late_terminal));
    }

    #[test]
    fn repeated_same_terminal_updates_are_preserved_for_result_events() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Failed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let repeated_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(!should_ignore_task_update(latest, &repeated_terminal));
    }

    #[test]
    fn stale_running_update_is_ignored_after_cancelling() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Cancelling, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(latest, &stale));
    }

    #[test]
    fn active_tasks_do_not_block_from_legacy_wait_policy_payloads() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&scheduler_blocking_task("blocking", TaskStatus::Running))
            .unwrap();
        storage
            .append_task(&task("background", TaskStatus::Running, false))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", usize::MAX)
            .unwrap();
        assert!(active.iter().any(|task| task.id == "blocking"));
        assert!(active.iter().any(|task| task.id == "background"));
        assert!(!active.iter().any(TaskRecord::is_blocking));
    }

    #[test]
    fn active_task_projection_ignores_terminal_latest_records() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    async fn non_terminal_task_updates_are_visible_without_scheduler_wait() {
        let runtime = runtime();

        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-1", TaskStatus::Running))
            .await
            .unwrap();

        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(active_tasks.iter().any(|task| task.id == "task-1"));
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[tokio::test]
    async fn task_transition_preserves_active_run_id_during_turn() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeRunning;
            guard.state.current_run_id = Some("run-1".into());
            guard.persist_state(&runtime.inner.storage).unwrap();
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
    async fn terminal_result_keeps_scheduler_idle_with_other_running_tasks() {
        let runtime = runtime();
        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-1", TaskStatus::Running))
            .await
            .unwrap();
        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-2", TaskStatus::Running))
            .await
            .unwrap();

        runtime
            .reduce_task_result_message(
                &task_result_message("task-1"),
                scheduler_blocking_task("task-1", TaskStatus::Completed),
                false,
                None,
            )
            .await
            .unwrap();
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(!active_tasks.iter().any(|task| task.id == "task-1"));
        assert!(active_tasks.iter().any(|task| task.id == "task-2"));

        runtime
            .reduce_task_result_message(
                &task_result_message("task-2"),
                scheduler_blocking_task("task-2", TaskStatus::Completed),
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
    async fn repeated_terminal_task_result_skips_transition_but_processes_message() {
        let runtime = runtime();
        let mut recorded = task("task-1", TaskStatus::Completed, false);
        recorded.parent_message_id = Some("original-parent".into());
        recorded.detail = Some(json!({"source": "completion"}));
        runtime
            .reduce_task_status_message(recorded.clone())
            .await
            .unwrap();

        let mut repeated = task("task-1", TaskStatus::Completed, false);
        repeated.parent_message_id = Some("task-result-message".into());
        repeated.detail = Some(json!({
            "source": "message",
            "parent_turn_id": "turn-1",
        }));
        let mut redispatched = repeated.clone();
        redispatched.created_at += chrono::Duration::seconds(1);
        redispatched.updated_at += chrono::Duration::seconds(1);
        runtime
            .reduce_task_result_message(&task_result_message("task-1"), repeated, false, None)
            .await
            .unwrap();
        runtime
            .reduce_task_result_message(&task_result_message("task-1"), redispatched, false, None)
            .await
            .unwrap();

        let latest = runtime.task_record("task-1").await.unwrap().unwrap();
        assert_eq!(latest.parent_message_id.as_deref(), Some("original-parent"));
        assert_eq!(latest.detail, recorded.detail);
        let events = runtime.recent_events(20).await.unwrap();
        let result_events = events
            .iter()
            .filter(|event| event.kind == "task_result_received")
            .collect::<Vec<_>>();
        assert_eq!(result_events.len(), 1);
        assert_eq!(result_events[0].created_at, recorded.updated_at);
        let payload =
            serde_json::from_value::<TaskLifecycleAuditEvent>(result_events[0].data.clone())
                .unwrap();
        assert_eq!(payload, TaskLifecycleAuditEvent::from_task(&recorded));
        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| {
            brief.kind == crate::types::BriefKind::Result
                && brief.text.contains("Task task-1 completed")
        }));
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
