use super::{scheduler, *};
use crate::types::{BriefKind, ExecutionAdmissionProvenance};
use sha2::{Digest, Sha256};

const TASK_TRANSITION_MAX_ATTEMPTS: usize = 3;

#[derive(Debug)]
pub(super) struct TaskTransitionRetryExhausted {
    source: anyhow::Error,
}

impl std::fmt::Display for TaskTransitionRetryExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "task transition retry budget exhausted: {}",
            self.source
        )
    }
}

impl std::error::Error for TaskTransitionRetryExhausted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

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
    pub(super) message_evidence: Option<&'a MessageEnvelope>,
    pub(super) admit_result_message: bool,
}

impl<'a> TaskTransition<'a> {
    pub(super) fn new(task: &'a TaskRecord, event_kind: &'static str) -> Self {
        Self {
            task,
            event_kind,
            message_evidence: None,
            admit_result_message: false,
        }
    }

    #[cfg(test)]
    pub(super) fn with_message_evidence(mut self, message: &'a MessageEnvelope) -> Self {
        self.message_evidence = Some(message);
        self
    }

    pub(super) fn with_terminal_result(mut self, message: &'a MessageEnvelope) -> Self {
        self.message_evidence = Some(message);
        self.admit_result_message = true;
        self
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
        for attempt in 0..TASK_TRANSITION_MAX_ATTEMPTS {
            match self
                .apply_task_transition_attempt(&transition, emit_event)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error)
                    if task_transition_error_is_retryable_conflict(&error)
                        && attempt + 1 < TASK_TRANSITION_MAX_ATTEMPTS =>
                {
                    if transition.admit_result_message {
                        // The attempt future has returned, so its local agent
                        // guard is dropped before this refresh re-locks it.
                        let agent_id = transition.task.agent_id.as_str();
                        if !self.refresh_enqueue_agent_state_baseline(agent_id).await? {
                            return Err(error);
                        }
                    }
                    continue;
                }
                Err(error) if task_transition_error_is_retryable_conflict(&error) => {
                    return Err(TaskTransitionRetryExhausted { source: error }.into());
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("task transition attempts are non-empty")
    }

    async fn apply_task_transition_attempt(
        &self,
        transition: &TaskTransition<'_>,
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
        let mut agent_guard = if transition.admit_result_message {
            Some(self.inner.agent.lock().await)
        } else {
            None
        };
        let (mut state, expected_state) = if let Some(guard) = agent_guard.as_ref() {
            (guard.state.clone(), guard.last_persisted_state.clone())
        } else {
            let state = self.agent_state().await?;
            let expected_state = state.clone();
            (state, expected_state)
        };
        if !matches!(state.status, AgentStatus::Stopped) && state.current_run_id.is_none() {
            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
        }
        let mut expected_wait_conditions = Vec::new();
        let mut wait_conditions = Vec::new();
        let work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        if task_will_change {
            index_changes.extend(self.inner.storage.index_changes_for_task(task)?);
        }
        if emit_event {
            let payload = TaskLifecycleAuditEvent::from_task(&persisted_task);
            let mut skip_event = false;
            let mut event =
                if let Some(kind) = RuntimeEventKind::from_wire_name(transition.event_kind) {
                    AuditEvent::typed(kind, &payload)?
                } else {
                    AuditEvent::legacy(transition.event_kind, to_json_value(&payload))
                };
            if is_terminal_task_status(&persisted_task.status) {
                event.id = stable_terminal_task_event_id(transition.event_kind, &persisted_task);
                event.created_at = persisted_task.updated_at;
                // When a terminal transition is repeated (e.g. duplicate
                // task_result delivery after a concurrent writer modified the
                // stored task), the stable event id collides with the original
                // emission but the payload may differ.  Skip re-emission when
                // the event already exists to avoid a content conflict.
                skip_event = repeated_terminal
                    && self
                        .inner
                        .runtime_db
                        .audit_events()
                        .has_event_by_id(&event.id)?;
            }
            if !skip_event {
                audit_events.push(event);
            }
        }
        if is_terminal_task_status(&task.status) {
            if let Some(message) = transition.message_evidence {
                if let Some(wait_trigger) = self.wait_trigger_transition_for_message(message)? {
                    expected_wait_conditions.push(wait_trigger.expected);
                    wait_conditions.push(wait_trigger.record.clone());
                    audit_events.push(AuditEvent::legacy(
                        "wait_condition_triggered",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "wait_condition_id": wait_trigger.record.id,
                            "trigger_message_id": message.id,
                            "work_item_id": wait_trigger.record.work_item_id,
                        }),
                    ));
                }
            }
        }
        #[cfg(test)]
        self.inject_task_transition_conflict_if_armed().await?;
        #[cfg(test)]
        self.inject_terminal_task_transition_conflict_if_armed()?;
        let existing_queue_entry = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .map(|message| self.inner.runtime_db.queue_entries().latest(&message.id))
            .transpose()?
            .flatten();
        let result_message_is_new = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .map(|message| self.inner.storage.read_message_by_id(&message.id))
            .transpose()?
            .flatten()
            .is_none();
        let queue_entry = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .filter(|_| {
                existing_queue_entry.as_ref().is_none_or(|entry| {
                    matches!(
                        entry.status,
                        QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                    )
                })
            })
            .map(|message| QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: existing_queue_entry
                    .as_ref()
                    .map_or(message.created_at, |entry| entry.created_at),
                updated_at: Utc::now(),
            });
        if let Some(message) = transition
            .message_evidence
            .filter(|_| queue_entry.is_some() && result_message_is_new)
        {
            audit_events.extend([
                AuditEvent::legacy(
                    "message_admitted",
                    serde_json::json!({
                        "message_id": message.id,
                        "agent_id": message.agent_id,
                        "kind": message.kind,
                        "origin": message.origin,
                        "authority_class": message.authority_class,
                        "delivery_surface": message.delivery_surface,
                        "admission_context": message.admission_context,
                        "trigger_kind": message.trigger_kind,
                        "work_item_id": message.work_item_id,
                        "task_id": message.task_id,
                        "source_refs": message.source_refs,
                        "correlation_id": message.correlation_id,
                        "causation_id": message.causation_id,
                    }),
                ),
                AuditEvent::typed(
                    RuntimeEventKind::MessageEnqueued,
                    &MessageLifecycleAuditEvent::from_message(message),
                )?,
            ]);
        }
        let queue_needs_push = queue_entry.as_ref().is_some_and(|entry| {
            agent_guard.as_ref().is_some_and(|guard| {
                guard
                    .queue
                    .peek_next_matching(|message| message.id == entry.message_id)
                    .is_none()
            })
        });
        if queue_needs_push {
            state.pending = agent_guard
                .as_ref()
                .map_or(state.pending.saturating_add(1), |guard| {
                    guard.queue.len().saturating_add(1)
                });
            state.last_wake_reason = Some("TaskResult".into());
            state.total_message_count = self
                .inner
                .storage
                .count_messages()?
                .saturating_add(usize::from(result_message_is_new));
            scheduler::apply_message_wake_projection(&mut state);
        }
        let agent_state =
            (state != expected_state).then(|| crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(expected_state)),
                record: Box::new(state.clone()),
            });
        let message_evidence = transition
            .message_evidence
            .map(|message| {
                let mut message = message.clone();
                message.normalize_admission_fields();
                message
            })
            .into_iter()
            .collect();
        let commit =
            self.commit_task_transition(&crate::runtime_db::transitions::TaskTransitionCommand {
                agent_id,
                task: persisted_task,
                queue_entry,
                work_items,
                expected_wait_conditions,
                wait_conditions,
                agent_state,
                message_evidence,
                audit_events,
                index_changes,
                notify_scheduler: queue_needs_push,
                commit_on_idempotent: emit_event
                    && !task_will_change
                    && is_terminal_task_status(&task.status),
                fault: self.take_transition_fault(),
            })?;
        let mut commit = commit;
        if let (Some(guard), Some(message)) = (
            agent_guard.as_mut(),
            transition.message_evidence.filter(|_| queue_needs_push),
        ) {
            guard.queue.push(message.clone());
            guard.state = state.clone();
            guard.last_persisted_state = state;
            commit.effects.agent_state = None;
        }
        drop(agent_guard);
        self.apply_transition_commit(commit).await;
        Ok(())
    }

    #[cfg(test)]
    async fn inject_task_transition_conflict_if_armed(&self) -> Result<()> {
        let remaining = match self.inner.task_transition_conflicts_remaining.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |remaining| remaining.checked_sub(1),
        ) {
            Ok(remaining) => remaining,
            Err(_) => return Ok(()),
        };
        let mut guard = self.inner.agent.lock().await;
        guard.state.pending_wake_hint = Some(crate::types::PendingWakeHint {
            reason: "test_conflict".into(),
            description: Some(format!(
                "concurrent task transition test attempt {remaining}"
            )),
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        guard.persist_state(&self.inner.storage)
    }

    #[cfg(test)]
    pub(crate) fn inject_task_transition_conflicts(&self, count: usize) {
        self.inner
            .task_transition_conflicts_remaining
            .store(count, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn inject_terminal_task_transition_conflict_if_armed(&self) -> Result<()> {
        let remaining = match self
            .inner
            .terminal_task_transition_conflicts_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            }) {
            Ok(remaining) => remaining,
            Err(_) => return Ok(()),
        };
        let mut state = self
            .inner
            .runtime_db
            .agent_states()
            .latest(&self.inner.default_agent_id)?
            .expect("test runtime agent state");
        state.pending_wake_hint = Some(crate::types::PendingWakeHint {
            reason: "test_terminal_conflict".into(),
            description: Some(format!(
                "concurrent terminal task transition test attempt {remaining}"
            )),
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        self.inner.storage.write_agent(&state)
    }

    #[cfg(test)]
    fn inject_terminal_task_transition_conflicts(&self, count: usize) {
        self.inner
            .terminal_task_transition_conflicts_remaining
            .store(count, Ordering::SeqCst);
    }

    pub(super) async fn persist_task_transition(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.apply_task_transition(TaskTransition::new(task, event_kind))
            .await
    }

    #[cfg(test)]
    pub(super) async fn persist_task_transition_with_message(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
    ) -> Result<()> {
        self.apply_task_transition(
            TaskTransition::new(task, event_kind).with_message_evidence(message),
        )
        .await
    }

    pub(super) async fn commit_terminal_task_result(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
    ) -> Result<()> {
        let mut message = message.clone();
        message.normalize_admission_fields();
        self.apply_task_transition(
            TaskTransition::new(task, event_kind).with_terminal_result(&message),
        )
        .await
    }

    pub(super) async fn reduce_task_status_message(&self, task: TaskRecord) -> Result<()> {
        self.persist_task_transition(&task, "task_status_updated")
            .await
    }

    #[cfg(test)]
    pub(super) async fn reduce_task_result_message(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
    ) -> Result<()> {
        let execution_admission_provenance = self.legacy_execution_admission_provenance(
            message,
            continuation_resolution,
            Some(&task),
        )?;
        if let Some(transition) = self
            .reduce_task_result_message_deferred(
                message,
                task,
                model_reentry,
                continuation_resolution,
                execution_admission_provenance,
            )
            .await?
        {
            self.persist_terminal_transition(&transition).await?;
        }
        Ok(())
    }

    pub(super) async fn reduce_task_result_message_deferred(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
        execution_admission_provenance: ExecutionAdmissionProvenance,
    ) -> Result<Option<turn::TurnTerminalTransition>> {
        if should_ignore_task_update(self.inner.runtime_db.tasks().latest(&task.id)?, &task) {
            return Ok(None);
        }
        self.persist_task_transition(&task, "task_result_received")
            .await?;
        let parent_turn_already_delivered =
            task_result_parent_turn_already_delivered(&self.inner.storage, &task)?;

        let task_status_label = match task.status {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Running => "running",
            TaskStatus::Queued => "queued",
        };
        let emit_result_brief =
            should_emit_task_result_brief(&task) && !parent_turn_already_delivered;
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
        if model_reentry && !parent_turn_already_delivered {
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
            let transition = self
                .process_interactive_message_deferred_with_cleanup(
                    message,
                    continuation_resolution,
                    execution_admission_provenance,
                    LoopControlOptions {
                        max_tool_rounds: None,
                    },
                )
                .await?;
            return Ok(Some(transition));
        } else if !model_reentry {
            if emit_result_brief {
                let brief = brief::make_result(&message.agent_id, message, result_text);
                self.persist_brief(&brief).await?;
            }
        } else {
            self.inner.storage.append_event(&AuditEvent::legacy(
                "stale_task_result_rejoin_suppressed",
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "task_id": task.id,
                    "parent_turn_id": task_parent_turn_id(&task),
                    "reason": "parent_turn_already_delivered",
                }),
            ))?;
        }
        Ok(None)
    }
}

fn task_transition_error_is_retryable_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
            .is_some_and(crate::runtime_db::RuntimeStateTransitionConflict::retryable)
    })
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

fn task_parent_turn_id(task: &TaskRecord) -> Option<&str> {
    task.detail
        .as_ref()
        .and_then(|detail| detail.get("parent_turn_id"))
        .and_then(serde_json::Value::as_str)
}

fn task_result_parent_turn_already_delivered(
    storage: &AppStorage,
    task: &TaskRecord,
) -> Result<bool> {
    let Some(parent_turn_id) = task_parent_turn_id(task) else {
        return Ok(false);
    };
    let Some(parent_turn) = storage.read_turn_by_id(parent_turn_id)? else {
        return Ok(false);
    };
    Ok(storage
        .read_briefs_by_ids(&parent_turn.produced_brief_ids)?
        .iter()
        .any(|brief| brief.kind == BriefKind::Result))
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
        let mut message = MessageEnvelope::new(
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
        );
        message.task_id = Some(task_id.into());
        message
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
    async fn task_transition_without_agent_state_change_ignores_concurrent_agent_write() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeIdle;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(1);

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .unwrap();

        assert!(runtime.task_record("task-1").await.unwrap().is_some());
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(
            state
                .pending_wake_hint
                .as_ref()
                .map(|hint| hint.reason.as_str()),
            Some("test_conflict")
        );
    }

    #[tokio::test]
    async fn task_transition_recomputes_from_latest_agent_state_after_conflict() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::Booting;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(1);

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .unwrap();

        assert!(runtime.task_record("task-1").await.unwrap().is_some());
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(
            state
                .pending_wake_hint
                .as_ref()
                .and_then(|hint| hint.description.as_deref()),
            Some("concurrent task transition test attempt 1")
        );
    }

    #[tokio::test]
    async fn terminal_task_result_retry_releases_agent_lock_before_refresh() {
        let runtime = runtime();
        runtime.inject_terminal_task_transition_conflicts(1);
        let message = task_result_message("task-1");

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.commit_terminal_task_result(
                &task("task-1", TaskStatus::Completed, true),
                "task_completed",
                &message,
            ),
        )
        .await
        .expect("terminal task transition retry should not deadlock")
        .unwrap();

        assert_eq!(
            runtime
                .agent_state()
                .await
                .unwrap()
                .pending_wake_hint
                .as_ref()
                .map(|hint| hint.reason.as_str()),
            Some("test_terminal_conflict")
        );
        assert_eq!(
            runtime
                .storage()
                .read_message_by_id(&message.id)
                .unwrap()
                .map(|stored| stored.id),
            Some(message.id)
        );
    }

    #[tokio::test]
    async fn exhausted_task_transition_conflicts_only_requeue_task_results() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::Booting;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(TASK_TRANSITION_MAX_ATTEMPTS);

        let error = runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .expect_err("retry budget should be exhausted");
        assert!(error.chain().any(|source| source
            .downcast_ref::<TaskTransitionRetryExhausted>()
            .is_some()));

        assert_eq!(
            runtime_error_queue_settlement(&MessageKind::TaskResult, &error),
            (
                QueueEntryStatus::Interrupted,
                "task_transition_retry_exhausted"
            )
        );
        assert_eq!(
            runtime_error_queue_settlement(&MessageKind::OperatorPrompt, &error),
            (QueueEntryStatus::Aborted, "runtime_error")
        );
        assert!(runtime.task_record("task-1").await.unwrap().is_none());
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
    async fn delivered_parent_turn_suppresses_stale_task_result_reentry_and_brief() {
        let runtime = runtime();
        let mut delivered = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Original operator answer.",
            Some("operator-message".into()),
            None,
        );
        delivered.turn_id = Some("turn-parent".into());
        runtime.storage().append_brief(&delivered).unwrap();
        let mut parent_turn = TurnRecord::new("default", "turn-parent", 1);
        parent_turn.produced_brief_ids = vec![delivered.id.clone()];
        runtime.storage().append_turn(&parent_turn).unwrap();

        let mut stale = task("task-1", TaskStatus::Completed, false);
        stale.detail = Some(json!({"parent_turn_id": "turn-parent"}));
        runtime
            .reduce_task_result_message(&task_result_message("task-1"), stale, true, None)
            .await
            .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert_eq!(briefs, vec![delivered]);
        assert!(runtime
            .recent_events(20)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "stale_task_result_rejoin_suppressed"));
        assert!(runtime
            .agent_state()
            .await
            .unwrap()
            .last_turn_terminal
            .is_none());
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
    async fn repeated_terminal_after_concurrent_update_does_not_emit_conflicting_event() {
        let runtime = runtime();
        let mut original = task("task-1", TaskStatus::Completed, false);
        original.parent_message_id = Some("parent-1".into());
        original.detail = Some(json!({"version": 1}));
        runtime
            .reduce_task_result_message(
                &task_result_message("task-1"),
                original.clone(),
                false,
                None,
            )
            .await
            .unwrap();

        // Simulate a concurrent writer updating the task in the runtime_db
        // with different content while keeping the same terminal status.
        // Use raw SQL to bypass the transition validation that would reject
        // a completed -> completed update with a different payload.
        runtime
            .runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET payload_json = json_set(payload_json, '$.detail', json('{\"version\": 2}')) WHERE task_id = 'task-1'",
                [],
            )
            .unwrap();

        // Re-dispatch the same task result.  Before the fix this emitted an
        // audit event with the same stable ID but content derived from the
        // concurrently-updated DB record, causing a conflict error.
        runtime
            .reduce_task_result_message(
                &task_result_message("task-1"),
                original.clone(),
                false,
                None,
            )
            .await
            .unwrap();

        let events = runtime.recent_events(20).await.unwrap();
        let result_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "task_result_received")
            .collect();
        // Only the original emission should exist.
        assert_eq!(result_events.len(), 1);
        let payload =
            serde_json::from_value::<TaskLifecycleAuditEvent>(result_events[0].data.clone())
                .unwrap();
        assert_eq!(payload, TaskLifecycleAuditEvent::from_task(&original));
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
