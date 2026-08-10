use super::*;

use crate::ingress::WakeHint;
use crate::runtime_error::RuntimeError;
use crate::types::{
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WaitConditionSummary, WakeSource,
    WorkItemRecord, WorkItemState,
};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForScope {
    Agent,
    WorkItem,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForWakeKind {
    OperatorInput,
    TaskResult,
    External,
    Timer,
    System,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WaitForRegistration {
    pub(crate) scope: WaitForScope,
    pub(crate) condition: WaitConditionRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item: Option<WorkItemRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cancelled_wait_condition_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(crate) enum WaitForRegistrationOutcome {
    Registered {
        registration: WaitForRegistration,
    },
    TaskResultQueued {
        task_id: String,
        result_message_id: String,
    },
    TaskResultAlreadyConsumed {
        task_id: String,
        result_message_id: String,
    },
}

#[derive(Debug, Clone)]
pub(super) struct WorkItemBlockerClearance {
    pub(super) work_item: WorkItemRecord,
    pub(super) expected_revision: Option<u64>,
    pub(super) wait_conditions: Vec<WaitConditionRecord>,
    pub(super) audit_events: Vec<AuditEvent>,
    pub(super) index_changes: Vec<crate::runtime_db::RuntimeIndexChange>,
    pub(super) blocker_cleared: bool,
    pub(super) cancelled_wait_condition_ids: Vec<String>,
}

impl WorkItemBlockerClearance {
    pub(super) fn unchanged(work_item: WorkItemRecord) -> Self {
        Self {
            work_item,
            expected_revision: None,
            wait_conditions: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            blocker_cleared: false,
            cancelled_wait_condition_ids: Vec::new(),
        }
    }
}

impl RuntimeHandle {
    pub(super) fn wait_trigger_transition_for_message(
        &self,
        message: &MessageEnvelope,
    ) -> Result<Option<crate::runtime_db::transitions::QueueWaitTransition>> {
        let matching = self
            .inner
            .storage
            .raw_active_wait_conditions_for_agent(&message.agent_id)?
            .into_iter()
            .filter(|condition| {
                message
                    .turn_id
                    .as_deref()
                    .zip(condition.turn_id.as_deref())
                    .is_none_or(|(message_turn, wait_turn)| message_turn != wait_turn)
            })
            .filter(|condition| matching_wake_source(message, condition).is_some())
            .collect::<Vec<_>>();
        let correlated_wait_id = message.source_refs.get("wait_id");
        let condition = if let Some(wait_id) = correlated_wait_id {
            matching.iter().find(|condition| condition.id == *wait_id)
        } else {
            matching.iter().max_by_key(|condition| {
                (
                    condition.updated_at,
                    condition.created_at,
                    condition.id.as_str(),
                )
            })
        };
        let Some(condition) = condition else {
            return Ok(None);
        };
        let mut triggered = condition.clone();
        triggered.mark_triggered(&message.id, self.now());
        Ok(Some(crate::runtime_db::transitions::QueueWaitTransition {
            expected: crate::runtime_db::transitions::WaitConditionExpectation {
                id: condition.id.clone(),
                agent_id: condition.agent_id.clone(),
                status: condition.status.clone(),
                updated_at: condition.updated_at,
            },
            record: triggered,
            work_item: None,
            index_changes: Vec::new(),
        }))
    }

    pub(super) fn wait_resolution_transition_for_message(
        &self,
        message: &MessageEnvelope,
    ) -> Result<Option<crate::runtime_db::transitions::QueueWaitTransition>> {
        let Some(condition) = self
            .inner
            .storage
            .raw_unresolved_wait_conditions_for_agent(&message.agent_id)?
            .into_iter()
            .find(|condition| {
                condition.status == WaitConditionStatus::Triggered
                    && condition.trigger_message_id() == Some(message.id.as_str())
            })
        else {
            return Ok(None);
        };
        let now = self.now();
        let mut resolved = condition.clone();
        resolved.status = WaitConditionStatus::Resolved;
        resolved.updated_at = now;
        resolved.resolved_at = Some(now);

        let mut work_item = None;
        let mut index_changes = Vec::new();
        if let Some(work_item_id) = resolved.work_item_id.as_deref() {
            if let Some(existing) = self.inner.runtime_db.work_items().latest(work_item_id)? {
                if existing.state == WorkItemState::Open
                    && existing.blocked_by.as_deref() == Some(resolved.waiting_for.as_str())
                {
                    let mut record = WorkItemRecord {
                        revision: existing.revision + 1,
                        blocked_by: None,
                        recheck_at: None,
                        recheck_consumed_at: None,
                        updated_at: now,
                        ..existing.clone()
                    };
                    crate::work_item_plan::refresh_plan_artifact_metadata(
                        self.agent_home().as_path(),
                        &mut record,
                    )?;
                    index_changes.extend(self.inner.storage.index_changes_for_work_item(&record)?);
                    work_item = Some(crate::runtime_db::transitions::WorkItemMutation::Update {
                        record,
                        expected_revision: existing.revision,
                    });
                }
            }
        }

        Ok(Some(crate::runtime_db::transitions::QueueWaitTransition {
            expected: crate::runtime_db::transitions::WaitConditionExpectation {
                id: condition.id.clone(),
                agent_id: condition.agent_id.clone(),
                status: condition.status.clone(),
                updated_at: condition.updated_at,
            },
            record: resolved,
            work_item,
            index_changes,
        }))
    }

    pub(crate) async fn register_wait_for(
        &self,
        agent_id: &str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
    ) -> Result<WaitForRegistration> {
        match self
            .register_wait_for_outcome(
                agent_id,
                work_item_id,
                wake,
                resource,
                reason,
                recheck_after_ms,
            )
            .await?
        {
            WaitForRegistrationOutcome::Registered { registration } => Ok(registration),
            WaitForRegistrationOutcome::TaskResultQueued {
                task_id,
                result_message_id,
            } => Err(RuntimeError::validation(
                "task_result_already_queued",
                format!(
                    "task {task_id} completed before wait registration; result {result_message_id} was queued"
                ),
            )
            .into()),
            WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
                task_id,
                result_message_id,
            } => Err(RuntimeError::validation(
                "task_result_already_consumed",
                format!(
                    "task {task_id} result was already consumed: {result_message_id}"
                ),
            )
            .into()),
        }
    }

    pub(crate) async fn register_wait_for_outcome(
        &self,
        agent_id: &str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
    ) -> Result<WaitForRegistrationOutcome> {
        let runtime_agent_id = self.agent_id().await?;
        if agent_id != runtime_agent_id {
            return Err(anyhow!("wait_for agent mismatch: {}", agent_id));
        }

        let mut expected_task = None;
        if wake == WaitForWakeKind::TaskResult {
            let task_id = wait_resource_required(wake, resource.clone())?;
            let task = self
                .inner
                .runtime_db
                .tasks()
                .latest(&task_id)?
                .ok_or_else(|| {
                    RuntimeError::not_found(
                        "task_not_found",
                        format!("wait_for task does not exist: {task_id}"),
                    )
                    .with_safe_context("task_id", &task_id)
                })?;
            self.validate_wait_for_task_owner(agent_id, work_item_id.as_deref(), &task)?;
            if task_state_reducer::is_terminal_task_status(&task.status) {
                return self.settle_terminal_task_result(task).await;
            }
            expected_task = Some(task_expectation(&task));
        }

        let now = self.now();
        let timer_wake_at = if wake == WaitForWakeKind::Timer {
            let timer_id = wait_resource_required(wake, resource.clone())?;
            let timer = self
                .inner
                .storage
                .latest_timer_record(&timer_id)?
                .ok_or_else(|| anyhow!("wait_for timer does not exist: {timer_id}"))?;
            if timer.agent_id != agent_id {
                return Err(anyhow!("wait_for timer agent mismatch: {timer_id}"));
            }
            if timer.status != TimerStatus::Active {
                return Err(anyhow!("wait_for timer is not active: {timer_id}"));
            }
            Some(
                timer
                    .next_fire_at
                    .ok_or_else(|| anyhow!("wait_for timer has no next fire time: {timer_id}"))?,
            )
        } else {
            None
        };
        let external_trigger_id = if wake == WaitForWakeKind::External {
            self.inner
                .runtime_db
                .external_triggers()
                .active_default_for_agent(&agent_id)?
                .map(|trigger| trigger.external_trigger_id)
        } else {
            None
        };
        let (kind, subject_ref, wake_sources) =
            wait_condition_parts(wake, resource.clone(), timer_wake_at, external_trigger_id)?;
        let recheck_at = recheck_after_ms.map(|delay| recheck_at_from(now, delay));
        let mut state = self.agent_state().await?;
        let expected_state = state.clone();
        let current_turn_id = state.current_turn_id.clone();
        let mut work_item = None;
        let active_waits = if let Some(work_item_id) = work_item_id.as_deref() {
            self.inner
                .storage
                .raw_unresolved_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .filter(|record| record.work_item_id.as_deref() == Some(work_item_id))
                .collect::<Vec<_>>()
        } else {
            self.inner
                .storage
                .raw_unresolved_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .filter(|record| record.work_item_id.is_none())
                .collect()
        };
        let mut wait_conditions = Vec::with_capacity(active_waits.len() + 1);
        let mut cancelled_wait_condition_ids = Vec::with_capacity(active_waits.len());
        for existing in active_waits {
            let mut cancelled = existing.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at = now;
            cancelled.cancelled_at = Some(now);
            cancelled_wait_condition_ids.push(existing.id);
            wait_conditions.push(cancelled);
        }
        let mut work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        let mut committed_agent_state = None;
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": work_item_id,
                    "reason": "wait_for_replaced",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        if let Some(work_item_id) = work_item_id.as_deref() {
            let existing = self.validate_owned_work_item(agent_id, work_item_id)?;
            if existing.state != WorkItemState::Open {
                return Err(RuntimeError::validation(
                    "work_item_completed",
                    format!("cannot wait on completed work item {work_item_id}"),
                )
                .with_safe_context("work_item_id", work_item_id)
                .into());
            }
            let mut updated = WorkItemRecord {
                revision: existing.revision + 1,
                blocked_by: Some(reason.clone()),
                recheck_at,
                recheck_consumed_at: None,
                updated_at: now,
                ..existing.clone()
            };
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut updated,
            )?;
            if plan_artifact_changed {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&updated) {
                    audit_events.push(event);
                }
            }
            audit_events.push(self.work_item_written_event(
                "wait_for_blocked",
                &updated,
                Value::Null,
            ));
            index_changes.extend(self.inner.storage.index_changes_for_work_item(&updated)?);
            work_items.push(crate::runtime_db::transitions::WorkItemMutation::Update {
                record: updated.clone(),
                expected_revision: existing.revision,
            });
            if state.current_turn_work_item_id.as_deref() == Some(updated.id.as_str()) {
                state.current_turn_work_item_id = None;
                audit_events.push(AuditEvent::legacy(
                    "work_item_turn_binding_released",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": updated.id.as_str(),
                        "reason": "work_item_waiting",
                        "readiness": updated.readiness(),
                        "revision": updated.revision,
                    }),
                ));
                committed_agent_state = Some(state.clone());
            }
            if wake == WaitForWakeKind::OperatorInput
                && state.current_work_item_id.as_deref() == Some(updated.id.as_str())
            {
                state.current_work_item_id = None;
                audit_events.push(AuditEvent::legacy(
                    "work_item_focus_released",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": updated.id.as_str(),
                        "reason": "operator_input_wait",
                        "readiness": updated.readiness(),
                        "revision": updated.revision,
                    }),
                ));
                committed_agent_state = Some(state.clone());
            }
            work_item = Some(updated);
        }

        let condition = WaitConditionRecord {
            id: crate::ids::wait_condition_id(),
            agent_id: agent_id.to_string(),
            work_item_id: work_item_id.clone(),
            status: WaitConditionStatus::Active,
            kind,
            source: Some("WaitFor".to_string()),
            subject_ref,
            waiting_for: reason.clone(),
            wake_sources,
            continuation: Some(serde_json::json!({
                "created_by": "WaitFor",
                "wake": wake,
                "resource": resource,
                "recheck_after_ms": recheck_after_ms,
                "recheck_at": recheck_at,
                "recovery": recheck_at.map(|_| serde_json::json!({
                    "kind": "recoverable",
                    "recheck_source": "WaitFor",
                })),
                "clear_blocker_on_task_result": wake == WaitForWakeKind::TaskResult,
            })),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: current_turn_id,
            trigger_message_id: None,
            triggered_at: None,
        };
        wait_conditions.push(condition.clone());
        audit_events.extend(
            self.inner
                .storage
                .wait_condition_auxiliary_events(&condition),
        );
        audit_events.push(AuditEvent::legacy(
            "wait_condition_registered",
            serde_json::json!({
                "agent_id": agent_id,
                "work_item_id": work_item_id,
                "wait_condition_id": condition.id,
                "source": "WaitFor",
                "kind": &condition.kind,
                "subject_ref": &condition.subject_ref,
                "waiting_for": &condition.waiting_for,
                "wake_sources": &condition.wake_sources,
                "cancelled_wait_condition_ids": &cancelled_wait_condition_ids,
            }),
        ));
        let execution_protocol =
            self.execution_wait_settlement_transition(&condition, work_item.as_ref(), now)?;
        let command = crate::runtime_db::transitions::WaitTransitionCommand {
            agent_id: agent_id.to_string(),
            work_items,
            expected_wait_conditions: Vec::new(),
            wait_conditions,
            agent_state: committed_agent_state.map(|record| {
                crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(expected_state)),
                    record: Box::new(record),
                }
            }),
            audit_events,
            index_changes,
            notify_scheduler: true,
            fault: self.take_transition_fault(),
        };
        let commit = 'retry: {
            for attempt in 0..3 {
                match self
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_wait_with_execution_protocol_and_task_expectation(
                        &command,
                        &execution_protocol,
                        expected_task.as_ref(),
                    ) {
                    Ok(commit) => break 'retry commit,
                    Err(error)
                        if expected_task.is_some()
                            && error
                                .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
                                .is_some_and(|conflict| {
                                    conflict.retryable() && conflict.domain() == "task_wait"
                                }) =>
                    {
                        let task_id = expected_task
                            .as_ref()
                            .expect("task expectation exists")
                            .id
                            .clone();
                        let Some(task) = self.inner.runtime_db.tasks().latest(&task_id)? else {
                            return Err(error);
                        };
                        self.validate_wait_for_task_owner(
                            agent_id,
                            work_item_id.as_deref(),
                            &task,
                        )?;
                        if task_state_reducer::is_terminal_task_status(&task.status) {
                            return self.settle_terminal_task_result(task).await;
                        }
                        if attempt + 1 == 3 {
                            return Err(error);
                        }
                        expected_task = Some(task_expectation(&task));
                    }
                    Err(error) => return Err(error),
                }
            }
            unreachable!("wait registration retry budget is non-empty")
        };
        self.apply_transition_commit(commit).await;

        Ok(WaitForRegistrationOutcome::Registered {
            registration: WaitForRegistration {
                scope: if condition.work_item_id.is_some() {
                    WaitForScope::WorkItem
                } else {
                    WaitForScope::Agent
                },
                condition,
                recheck_after_ms,
                recheck_at,
                work_item,
                cancelled_wait_condition_ids,
            },
        })
    }

    fn validate_wait_for_task_owner(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        task: &TaskRecord,
    ) -> Result<()> {
        if task.agent_id != agent_id {
            return Err(RuntimeError::validation(
                "task_agent_mismatch",
                format!("wait_for task belongs to another agent: {}", task.id),
            )
            .with_safe_context("task_id", &task.id)
            .into());
        }
        if task.work_item_id.as_deref() != work_item_id {
            return Err(RuntimeError::validation(
                "task_work_item_mismatch",
                format!(
                    "wait_for task owner does not match the current work item: {}",
                    task.id
                ),
            )
            .with_safe_context("task_id", &task.id)
            .into());
        }
        Ok(())
    }

    async fn settle_terminal_task_result(
        &self,
        task: TaskRecord,
    ) -> Result<WaitForRegistrationOutcome> {
        let result_message_id = task.parent_message_id.clone().ok_or_else(|| {
            RuntimeError::validation(
                "task_result_evidence_missing",
                format!(
                    "terminal task has no exact result message identity: {}",
                    task.id
                ),
            )
            .with_safe_context("task_id", &task.id)
        })?;
        let result_message = self
            .inner
            .storage
            .read_message_by_id(&result_message_id)?
            .ok_or_else(|| {
                RuntimeError::validation(
                    "task_result_evidence_missing",
                    format!(
                        "terminal task result message evidence is missing: {}",
                        task.id
                    ),
                )
                .with_safe_context("task_id", &task.id)
                .with_safe_context("result_message_id", &result_message_id)
            })?;
        let existing_entry = self
            .inner
            .storage
            .latest_queue_entries()?
            .into_iter()
            .find(|entry| entry.message_id == result_message_id);
        if existing_entry.as_ref().is_some_and(|entry| {
            matches!(
                entry.status,
                QueueEntryStatus::Dequeued
                    | QueueEntryStatus::Interjected
                    | QueueEntryStatus::Processed
                    | QueueEntryStatus::Aborted
                    | QueueEntryStatus::Dropped
                    | QueueEntryStatus::Quarantined
            )
        }) {
            return Ok(WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
                task_id: task.id,
                result_message_id,
            });
        }

        let now = self.now();
        let expected_task = task_expectation(&task);
        let execution_protocol =
            self.execution_continue_settlement_transition(&task, &result_message, now)?;
        let commit = {
            let mut guard = self.inner.agent.lock().await;
            let already_in_memory = guard
                .queue
                .peek_next_matching(|message| message.id == result_message_id)
                .is_some();
            let expected_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            committed_state.pending = guard
                .queue
                .len()
                .saturating_add(usize::from(!already_in_memory));
            committed_state.last_wake_reason = Some("TaskResult".into());
            committed_state.total_message_count = self.inner.storage.count_messages()?;
            scheduler::apply_message_wake_projection(&mut committed_state);
            let queue_entry = QueueEntryRecord {
                message_id: result_message_id.clone(),
                agent_id: task.agent_id.clone(),
                priority: result_message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: existing_entry
                    .as_ref()
                    .map_or(result_message.created_at, |entry| entry.created_at),
                updated_at: now,
            };
            let commit = self
                .inner
                .runtime_db
                .transitions()
                .commit_queue_with_execution_protocol_and_task_expectation(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: task.agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Admit,
                        mutation: crate::runtime_db::transitions::QueueMutation::Upsert(
                            queue_entry,
                        ),
                        scheduler_claim_work_item: None,
                        agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                            expected: Some(Box::new(expected_state)),
                            record: Box::new(committed_state.clone()),
                        }),
                        message_evidence: vec![result_message.clone()],
                        transcript_entries: Vec::new(),
                        turn_record: None,
                        audit_events: vec![AuditEvent::legacy(
                            "late_task_result_queued",
                            serde_json::json!({
                                "agent_id": task.agent_id,
                                "task_id": task.id,
                                "result_message_id": result_message_id,
                            }),
                        )],
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                    &execution_protocol,
                    &expected_task,
                )?;
            if !already_in_memory {
                guard.queue.push(result_message);
            }
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            commit
        };
        self.apply_transition_commit(commit).await;
        Ok(WaitForRegistrationOutcome::TaskResultQueued {
            task_id: task.id,
            result_message_id,
        })
    }

    fn execution_continue_settlement_transition(
        &self,
        task: &TaskRecord,
        result_message: &MessageEnvelope,
        settled_at: DateTime<Utc>,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            ConversationOutcome, ExecutionBinding, ExecutionOutcome, ExecutionOutcomeRecord,
            ExecutionProtocolCommand, SettleExecution, WorkItemOutcome,
        };

        let Some(state) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&task.agent_id)?
        else {
            return Ok(Default::default());
        };
        let Some(attempt) = state.open_attempt() else {
            return Ok(Default::default());
        };
        let outcome = match &attempt.binding {
            ExecutionBinding::WorkItem { work_item_id }
                if task.work_item_id.as_deref() == Some(work_item_id.as_str()) =>
            {
                ExecutionOutcome::WorkItem(WorkItemOutcome::Continue)
            }
            ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. }
                if task.work_item_id.is_none() =>
            {
                ExecutionOutcome::Conversation(ConversationOutcome::Replied)
            }
            ExecutionBinding::Command => {
                return Err(anyhow!("command execution cannot settle through WaitFor"));
            }
            _ => {
                return Err(anyhow!(
                    "task result owner does not match the current execution binding"
                ));
            }
        };
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: None,
                commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: format!(
                            "outcome:late-task-result:{}:{}",
                            attempt.attempt_id, result_message.id
                        ),
                        attempt_id: attempt.attempt_id.clone(),
                        outcome,
                        created_at: settled_at.to_rfc3339(),
                    },
                })],
            },
        )
    }

    fn execution_wait_settlement_transition(
        &self,
        condition: &WaitConditionRecord,
        work_item: Option<&WorkItemRecord>,
        settled_at: DateTime<Utc>,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            ConversationOutcome, ExecutionBinding, ExecutionOutcome, ExecutionOutcomeRecord,
            ExecutionProtocolCommand, SetWorkItemWaiting, SettleExecution, WaitReference,
            WorkItemExecutionRecord, WorkItemExecutionState, WorkItemOutcome,
        };

        let Some(state) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&condition.agent_id)?
        else {
            return Ok(Default::default());
        };
        let Some(attempt) = state.open_attempt() else {
            return Ok(Default::default());
        };
        let wait = WaitReference {
            wait_id: condition.id.clone(),
        };
        let mut commands = Vec::with_capacity(2);
        let outcome = match &attempt.binding {
            ExecutionBinding::WorkItem { work_item_id }
                if condition.work_item_id.as_deref() == Some(work_item_id.as_str()) =>
            {
                ExecutionOutcome::WorkItem(WorkItemOutcome::Wait { wait })
            }
            ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
                if let Some(work_item_id) = condition.work_item_id.as_deref() {
                    let work_item = work_item
                        .filter(|record| record.id == work_item_id)
                        .ok_or_else(|| {
                            anyhow!("WorkItem wait handoff is missing its updated WorkItem")
                        })?;
                    let expected = state.work_items.get(work_item_id).cloned();
                    let generation = expected
                        .as_ref()
                        .map_or(work_item.revision.max(1), |record| record.generation() + 1);
                    commands.push(ExecutionProtocolCommand::SetWorkItemWaiting(Box::new(
                        SetWorkItemWaiting {
                            command_id: format!(
                                "handoff:{}:work_item_wait:{}",
                                attempt.attempt_id, condition.id
                            ),
                            work_item_id: work_item_id.to_string(),
                            expected,
                            record: WorkItemExecutionRecord {
                                source_revision: work_item.revision,
                                state: WorkItemExecutionState::Waiting {
                                    generation,
                                    wait: wait.clone(),
                                },
                            },
                        },
                    )));
                    ExecutionOutcome::Conversation(ConversationOutcome::HandoffToWorkItemWait {
                        work_item_id: work_item_id.to_string(),
                        wait,
                    })
                } else {
                    ExecutionOutcome::Conversation(ConversationOutcome::Wait { wait })
                }
            }
            ExecutionBinding::Command => {
                return Err(anyhow!("command execution cannot settle through WaitFor"));
            }
            _ => {
                return Err(anyhow!(
                    "WaitFor owner does not match the current execution binding"
                ));
            }
        };
        commands.push(ExecutionProtocolCommand::Settle(SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: format!("outcome:wait-for:{}:{}", attempt.attempt_id, condition.id),
                attempt_id: attempt.attempt_id.clone(),
                outcome,
                created_at: settled_at.to_rfc3339(),
            },
        }));
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: None,
                commands,
            },
        )
    }

    pub(super) async fn clear_work_item_blocker_for_pick(
        &self,
        agent_id: &str,
        existing: WorkItemRecord,
        reason: &str,
    ) -> Result<WorkItemBlockerClearance> {
        let now = Utc::now();
        let active_waits = self
            .inner
            .storage
            .raw_unresolved_wait_conditions_for_agent(agent_id)?
            .into_iter()
            .filter(|condition| condition.work_item_id.as_deref() == Some(existing.id.as_str()))
            .collect::<Vec<_>>();
        let mut wait_conditions = Vec::with_capacity(active_waits.len());
        let mut cancelled_wait_condition_ids = Vec::with_capacity(active_waits.len());
        for condition in active_waits {
            let mut cancelled = condition.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at = now;
            cancelled.cancelled_at = Some(now);
            cancelled_wait_condition_ids.push(condition.id);
            wait_conditions.push(cancelled);
        }
        let needs_record_write = existing.blocked_by.is_some()
            || existing.recheck_at.is_some()
            || existing.recheck_consumed_at.is_some();
        if !needs_record_write && wait_conditions.is_empty() {
            return Ok(WorkItemBlockerClearance::unchanged(existing));
        }

        let mut audit_events = Vec::new();
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": existing.id,
                    "reason": "pick_work_item_clear_blocker",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        let mut record = existing.clone();
        let mut index_changes = Vec::new();
        if needs_record_write {
            record = WorkItemRecord {
                revision: existing.revision + 1,
                blocked_by: None,
                recheck_at: None,
                recheck_consumed_at: None,
                updated_at: now,
                ..existing.clone()
            };
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut record,
            )?;
            if plan_artifact_changed {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                    audit_events.push(event);
                }
            }
            audit_events.push(self.work_item_written_event(
                "pick_blocker_cleared",
                &record,
                serde_json::json!({
                    "reason": reason,
                    "cancelled_wait_condition_ids": cancelled_wait_condition_ids.clone(),
                }),
            ));
            index_changes = self.inner.storage.index_changes_for_work_item(&record)?;
        }
        Ok(WorkItemBlockerClearance {
            work_item: record,
            expected_revision: needs_record_write.then_some(existing.revision),
            wait_conditions,
            audit_events,
            index_changes,
            blocker_cleared: true,
            cancelled_wait_condition_ids,
        })
    }

    pub async fn submit_wake_hint(&self, hint: WakeHint) -> Result<WakeDisposition> {
        let runtime_agent_id = self.agent_id().await?;
        let pending = PendingWakeHint {
            reason: hint.reason.clone(),
            description: hint.description.clone(),
            source: hint.source.clone(),
            scope: hint.scope.clone(),
            external_trigger_id: hint.external_trigger_id.clone(),
            resource: hint.resource.clone(),
            body: hint.body.clone(),
            content_type: hint.content_type.clone(),
            correlation_id: hint.correlation_id.clone(),
            causation_id: hint.causation_id.clone(),
            created_at: Utc::now(),
        };
        let work_item_id = self
            .wake_hint_work_item_id(hint.external_trigger_id.as_deref())
            .await?;

        let mut trigger_now = false;
        let disposition = {
            let mut guard = self.inner.agent.lock().await;
            match guard.state.status {
                AgentStatus::Stopped => WakeDisposition::Ignored,
                AgentStatus::AwakeRunning | AgentStatus::AwaitingTask => {
                    guard.state.pending_wake_hint = Some(pending.clone());
                    guard.persist_state(&self.inner.storage)?;
                    WakeDisposition::Coalesced
                }
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep => {
                    if guard.queue.is_empty() {
                        if guard.state.pending_wake_hint.take().is_some() {
                            guard.persist_state(&self.inner.storage)?;
                        }
                        trigger_now = true;
                        WakeDisposition::Triggered
                    } else {
                        guard.state.pending_wake_hint = Some(pending.clone());
                        guard.persist_state(&self.inner.storage)?;
                        WakeDisposition::Coalesced
                    }
                }
            }
        };

        let event_kind = match disposition {
            WakeDisposition::Triggered => "wake_hint_triggered",
            WakeDisposition::Coalesced => "wake_hint_coalesced",
            WakeDisposition::Ignored => "wake_hint_ignored",
        };
        self.inner.storage.append_event(&AuditEvent::legacy(
            event_kind,
            serde_json::json!({
                "agent_id": runtime_agent_id,
                "reason": hint.reason,
                "description": hint.description,
                "source": hint.source,
                "scope": hint.scope,
                "external_trigger_id": hint.external_trigger_id,
                "work_item_id": work_item_id,
                "resource": hint.resource,
                "body": hint.body,
                "content_type": hint.content_type,
                "correlation_id": hint.correlation_id,
                "causation_id": hint.causation_id,
            }),
        ))?;

        if trigger_now {
            if let Err(err) = self
                .emit_system_tick_from_wake_hint_with_decision(&pending)
                .await
            {
                let mut guard = self.inner.agent.lock().await;
                if guard.state.pending_wake_hint.is_none() {
                    guard.state.pending_wake_hint = Some(pending);
                    guard.persist_state(&self.inner.storage)?;
                }
                return Err(err);
            }
        }

        Ok(disposition)
    }

    pub(super) async fn emit_recovered_pending_wake_hint(&self) -> Result<()> {
        let pending_wake = {
            let guard = self.inner.agent.lock().await;
            guard.state.pending_wake_hint.clone()
        };
        if let Some(pending) = pending_wake {
            self.emit_system_tick_from_wake_hint_with_decision(&pending)
                .await?;
            let mut guard = self.inner.agent.lock().await;
            if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                guard.state.pending_wake_hint = None;
                guard.persist_state(&self.inner.storage)?;
            }
        }
        Ok(())
    }

    pub async fn schedule_timer(
        &self,
        duration_ms: u64,
        interval_ms: Option<u64>,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let created_at = self.now();
        let timer = TimerRecord {
            id: crate::ids::timer_id(),
            agent_id: self.agent_id().await?,
            created_at,
            duration_ms,
            interval_ms,
            repeat: interval_ms.is_some(),
            status: TimerStatus::Active,
            summary,
            next_fire_at: Some(advance_time(created_at, duration_ms)?),
            last_fired_at: None,
            fire_count: 0,
        };
        self.record_timer_projection(&timer).await?;
        self.inner
            .storage
            .append_event(&AuditEvent::legacy("timer_created", to_json_value(&timer)))?;
        self.spawn_timer_loop(timer.clone());

        Ok(timer)
    }

    pub async fn cancel_timer(&self, timer_id: &str) -> Result<TimerRecord> {
        let mut timer = self
            .inner
            .storage
            .latest_timer_record(timer_id)?
            .ok_or_else(|| {
                RuntimeError::not_found("timer_not_found", format!("timer {timer_id} not found"))
                    .with_safe_context("timer_id", timer_id)
            })?;
        if timer.agent_id != self.agent_id().await? {
            return Err(RuntimeError::not_found(
                "timer_not_found",
                format!("timer {timer_id} not found"),
            )
            .with_safe_context("timer_id", timer_id)
            .into());
        }
        match timer.status {
            TimerStatus::Cancelled => return Ok(timer),
            TimerStatus::Completed => {
                return Err(RuntimeError::validation(
                    "timer_completed",
                    format!("cannot cancel completed timer {timer_id}"),
                )
                .with_safe_context("timer_id", timer_id)
                .into())
            }
            TimerStatus::Active => {}
        }

        timer.status = TimerStatus::Cancelled;
        timer.next_fire_at = None;
        self.record_timer_projection(&timer).await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "timer_cancelled",
            serde_json::json!({
                "timer_id": timer.id,
                "status": timer.status,
                "fire_count": timer.fire_count,
            }),
        ))?;
        self.inner.notify.notify_waiters();
        Ok(timer)
    }

    pub(crate) async fn recover_active_timers(&self, timers: Vec<TimerRecord>) -> Result<()> {
        for timer in timers {
            self.recover_timer(timer).await?;
        }
        Ok(())
    }

    fn spawn_timer_loop(&self, timer: TimerRecord) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut timer = timer;
            loop {
                let Some(next_fire_at) = timer.next_fire_at else {
                    break;
                };
                if next_fire_at > runtime.now() {
                    runtime.inner.clock.sleep_until(next_fire_at).await;
                }
                if let Err(err) = runtime.fire_timer_record(&mut timer).await {
                    let _ = runtime.inner.storage.append_event(&AuditEvent::legacy(
                        "timer_fire_failed",
                        serde_json::json!({
                            "timer_id": timer.id,
                            "error": err.to_string(),
                        }),
                    ));
                    break;
                }
                if timer.status != TimerStatus::Active {
                    break;
                }
            }
        });
    }

    async fn recover_timer(&self, timer: TimerRecord) -> Result<()> {
        let now = self.now();
        let timer = normalize_recovered_timer(timer, now);
        if timer
            .next_fire_at
            .is_some_and(|next_fire_at| next_fire_at <= now)
        {
            let mut overdue = timer.clone();
            self.fire_timer_record(&mut overdue).await?;
            if overdue.status == TimerStatus::Active {
                self.spawn_timer_loop(overdue);
            }
        } else {
            self.spawn_timer_loop(timer);
        }
        Ok(())
    }

    async fn fire_timer_record(&self, timer: &mut TimerRecord) -> Result<()> {
        if let Some(latest) = self.inner.storage.latest_timer_record(&timer.id)? {
            if latest.status != TimerStatus::Active {
                *timer = latest;
                return Ok(());
            }
        }

        let work_item_id = self
            .wait_condition_work_item_id_for_timer(&timer.id)
            .await?;
        let mut message = MessageEnvelope {
            metadata: Some(serde_json::json!({ "timer_id": timer.id })),
            ..MessageEnvelope::new(
                timer.agent_id.clone(),
                MessageKind::TimerTick,
                MessageOrigin::Timer {
                    timer_id: timer.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: timer
                        .summary
                        .clone()
                        .unwrap_or_else(|| format!("timer {} fired", timer.id)),
                },
            )
            .with_admission(
                MessageDeliverySurface::TimerScheduler,
                AdmissionContext::RuntimeOwned,
            )
        };
        message.work_item_id = work_item_id;
        message
            .source_refs
            .insert("timer_id".into(), timer.id.clone());
        self.enqueue(message).await?;

        let fired_at = self.now();
        timer.last_fired_at = Some(fired_at);
        timer.fire_count += 1;
        if let Some(interval_ms) = timer.interval_ms {
            timer.status = TimerStatus::Active;
            timer.next_fire_at = Some(advance_time(fired_at, interval_ms)?);
        } else {
            timer.status = TimerStatus::Completed;
            timer.next_fire_at = None;
        }
        self.record_timer_projection(timer).await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "timer_fired",
            serde_json::json!({
                "timer_id": timer.id,
                "summary": timer.summary.clone(),
                "status": timer.status,
                "fire_count": timer.fire_count,
                "next_fire_at": timer.next_fire_at,
            }),
        ))?;
        Ok(())
    }

    pub async fn latest_external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        let cached = {
            self.inner
                .projection_cache
                .lock()
                .await
                .latest_external_triggers()
        };
        if !cached.is_empty() {
            return Ok(cached);
        }
        let agent_id = self.agent_id().await?;
        let mut records = self
            .inner
            .runtime_db
            .external_triggers()
            .latest_for_agent(&agent_id)?;
        if !records.is_empty() {
            let mut cache = self.inner.projection_cache.lock().await;
            for record in &records {
                cache.upsert_external_trigger(record.clone());
            }
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub(super) async fn wake_hint_work_item_id(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(work_item_id) = self
            .wait_condition_work_item_id_for_external_trigger(external_trigger_id)
            .await?
        {
            return Ok(Some(work_item_id));
        }
        Ok(None)
    }

    async fn wait_condition_work_item_id_for_external_trigger(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(external_trigger_id) = external_trigger_id else {
            return Ok(None);
        };
        let agent_id = self.agent_id().await?;
        let mut work_item_ids = self
            .inner
            .storage
            .active_wait_conditions_for_agent(&agent_id)?
            .into_iter()
            .filter(|condition| {
                condition.wake_sources.iter().any(|source| match source {
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some(id),
                    } => id == external_trigger_id,
                    WakeSource::ExternalIngress {
                        external_trigger_id: None,
                    } => true,
                    _ => false,
                })
            })
            .filter_map(|condition| condition.work_item_id)
            .collect::<Vec<_>>();
        work_item_ids.sort();
        work_item_ids.dedup();
        Ok((work_item_ids.len() == 1).then(|| work_item_ids.remove(0)))
    }

    pub(super) async fn exact_external_wait_correlation(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(external_trigger_id) = external_trigger_id else {
            return Ok(None);
        };
        let agent_id = self.agent_id().await?;
        let correlations = self
            .inner
            .storage
            .active_wait_conditions_for_agent(&agent_id)?
            .into_iter()
            .filter(|condition| {
                condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::ExternalIngress {
                            external_trigger_id: Some(id),
                        } if id == external_trigger_id
                    )
                })
            })
            .map(|condition| condition.id)
            .collect::<Vec<_>>();
        let [correlation] = correlations.as_slice() else {
            return Ok(None);
        };
        Ok(Some(correlation.clone()))
    }

    async fn wait_condition_work_item_id_for_timer(
        &self,
        timer_id: &str,
    ) -> Result<Option<String>> {
        let agent_id = self.agent_id().await?;
        let matches = self
            .inner
            .storage
            .active_wait_conditions_for_agent(&agent_id)?
            .into_iter()
            .filter(|condition| {
                condition.kind == WaitConditionKind::Timer
                    && condition.subject_ref.as_deref() == Some(timer_id)
            })
            .collect::<Vec<_>>();
        Ok((matches.len() == 1)
            .then(|| matches[0].work_item_id.clone())
            .flatten())
    }

    pub(super) async fn active_wait_condition_summaries(
        &self,
    ) -> Result<Vec<WaitConditionSummary>> {
        let agent_id = self.agent_id().await?;
        Ok(self
            .inner
            .storage
            .active_wait_conditions_for_agent(&agent_id)?
            .into_iter()
            .map(WaitConditionSummary::from)
            .collect())
    }

    pub(super) async fn active_external_trigger_summaries(
        &self,
    ) -> Result<Vec<ExternalTriggerSummary>> {
        Ok(self
            .latest_external_triggers()
            .await?
            .into_iter()
            .filter(|record| record.status == ExternalTriggerStatus::Active)
            .map(|record| ExternalTriggerSummary {
                external_trigger_id: record.external_trigger_id,
                target_agent_id: record.target_agent_id,
                scope: record.scope,
                delivery_mode: record.delivery_mode,
                status: record.status,
                delivery_count: record.delivery_count,
                created_at: record.created_at,
                revoked_at: record.revoked_at,
                last_delivered_at: record.last_delivered_at,
            })
            .collect())
    }

    pub(super) async fn record_wait_reconciliation_signals(
        &self,
        message: &MessageEnvelope,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let unresolved_conditions = self
            .inner
            .storage
            .raw_unresolved_wait_conditions_for_agent(&agent_id)?;
        if unresolved_conditions.is_empty() {
            return Ok(());
        }
        let unresolved_conditions = self
            .reconciliation_conditions_for_message(&agent_id, message, unresolved_conditions)
            .await?;

        let signals = reconciliation_signals_for_message(message, &unresolved_conditions);
        for signal in &signals {
            let duplicate = self
                .inner
                .storage
                .read_recent_events(500)?
                .iter()
                .any(|event| {
                    event.kind == "wait_reconciliation_requested"
                        && event.data["dedupe_key"] == signal["dedupe_key"]
                });
            if duplicate {
                continue;
            }
            self.inner.storage.append_event(&AuditEvent::legacy(
                "wait_reconciliation_requested",
                signal.clone(),
            ))?;
        }

        // Resolve matching wait conditions and clear WorkItem blockers so the
        // scheduler can advance the WorkItem without requiring the model to
        // explicitly call PickWorkItem(clear_blocker) or CompleteWorkItem.
        self.resolve_reconciled_wait_conditions(
            &agent_id,
            message,
            &unresolved_conditions,
            &signals,
        )
        .await?;

        Ok(())
    }

    async fn reconciliation_conditions_for_message(
        &self,
        agent_id: &str,
        message: &MessageEnvelope,
        active_conditions: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let operator_input = matches!(
            (&message.kind, &message.origin),
            (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. })
        );
        let callback_event = matches!(message.kind, MessageKind::CallbackEvent);
        let external_wake_hint = matches!(message.kind, MessageKind::SystemTick)
            && message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("wake_hint"))
                .and_then(|wake_hint| wake_hint.get("external_trigger_id"))
                .is_some();
        if !operator_input && !callback_event && !external_wake_hint {
            return Ok(active_conditions);
        }

        if !self.inner.scheduler_engine.is_canonical() {
            let matching = active_conditions
                .into_iter()
                .filter(|condition| {
                    reconciliation_signal_for_condition(message, condition).is_some()
                })
                .collect::<Vec<_>>();
            if matching.len() > 1 {
                return Err(anyhow!(
                    "legacy wait resume is ambiguous for message {}: {} matching waits",
                    message.id,
                    matching.len()
                ));
            }
            return Ok(matching);
        }

        let exact_condition = self
            .canonical_consumed_wait_condition(agent_id, message, &active_conditions)
            .await?;
        if operator_input || callback_event {
            // Contentful resume messages reconcile only the exact wait
            // generation consumed by their scheduler admission. Broadly
            // matching active waits could resolve unrelated conditions.
            return Ok(exact_condition.into_iter().collect());
        }

        let mut conditions = active_conditions
            .into_iter()
            .filter(|condition| {
                !condition
                    .wake_sources
                    .iter()
                    .any(|source| matches!(source, WakeSource::ExternalIngress { .. }))
            })
            .collect::<Vec<_>>();
        conditions.extend(exact_condition);
        Ok(conditions)
    }

    async fn canonical_consumed_wait_condition(
        &self,
        agent_id: &str,
        message: &MessageEnvelope,
        active_conditions: &[WaitConditionRecord],
    ) -> Result<Option<WaitConditionRecord>> {
        use crate::domain::execution_protocol::{
            ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity,
        };

        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(agent_id)?
        else {
            return Ok(None);
        };
        let Some(attempt) = execution.attempts.values().find(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(message.id.as_str())
        }) else {
            return Ok(None);
        };
        let ExecutionSourceIdentity::TriggeredWait {
            wait_id,
            trigger_message_id,
        } = &attempt.source.identity
        else {
            return Ok(None);
        };
        if trigger_message_id != &message.id {
            return Ok(None);
        }

        let Some(condition) = active_conditions
            .iter()
            .find(|condition| condition.id == *wait_id)
        else {
            return Ok(None);
        };
        let owner_matches = match (&attempt.binding, condition.work_item_id.as_deref()) {
            (ExecutionBinding::WorkItem { work_item_id }, Some(condition_work_item_id)) => {
                work_item_id == condition_work_item_id
                    && message.work_item_id.as_deref() == Some(condition_work_item_id)
            }
            (
                ExecutionBinding::AgentLifecycle {
                    agent_id: owner_agent_id,
                },
                None,
            ) => owner_agent_id == agent_id && message.work_item_id.is_none(),
            (ExecutionBinding::Conversation { .. }, None) => message.work_item_id.is_none(),
            _ => false,
        };
        Ok(owner_matches.then(|| condition.clone()))
    }

    async fn resolve_reconciled_wait_conditions(
        &self,
        agent_id: &str,
        message: &MessageEnvelope,
        active_conditions: &[WaitConditionRecord],
        signals: &[serde_json::Value],
    ) -> Result<()> {
        if signals.is_empty() {
            return Ok(());
        }

        let now = Utc::now();
        let mut resolved_conditions = Vec::new();
        let mut work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        let mut updated_work_item_ids = std::collections::BTreeSet::new();

        for signal in signals {
            let condition_id = signal["wait_condition_id"].as_str().unwrap_or_default();
            let Some(condition) = active_conditions.iter().find(|c| c.id == condition_id) else {
                continue;
            };
            if !matches!(
                condition.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            ) {
                continue;
            }

            let mut resolved = condition.clone();
            resolved.status = WaitConditionStatus::Resolved;
            resolved.updated_at = now;
            resolved.resolved_at = Some(now);
            resolved_conditions.push(resolved.clone());

            if let Some(work_item_id) = resolved.work_item_id.as_deref() {
                if updated_work_item_ids.insert(work_item_id.to_string()) {
                    if let Some(existing) =
                        self.inner.runtime_db.work_items().latest(work_item_id)?
                    {
                        if existing.state == WorkItemState::Open
                            && existing.blocked_by.as_deref() == Some(resolved.waiting_for.as_str())
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
                                "wait_reconciliation_resolved",
                                &record,
                                serde_json::json!({
                                    "wait_condition_id": resolved.id,
                                    "message_id": message.id,
                                }),
                            ));
                            index_changes
                                .extend(self.inner.storage.index_changes_for_work_item(&record)?);
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

        if resolved_conditions.is_empty() {
            return Ok(());
        }

        audit_events.push(AuditEvent::legacy(
            "wait_conditions_resolved",
            serde_json::json!({
                "agent_id": agent_id,
                "message_id": message.id,
                "reason": "wait_reconciliation",
                "wait_condition_ids": resolved_conditions
                    .iter()
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>(),
            }),
        ));

        let commit = self.inner.runtime_db.transitions().commit_wait(
            &crate::runtime_db::transitions::WaitTransitionCommand {
                agent_id: agent_id.to_string(),
                work_items,
                expected_wait_conditions: Vec::new(),
                wait_conditions: resolved_conditions,
                agent_state: None,
                audit_events,
                index_changes,
                notify_scheduler: true,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;

        Ok(())
    }
}

fn reconciliation_signals_for_message(
    message: &MessageEnvelope,
    active_conditions: &[WaitConditionRecord],
) -> Vec<serde_json::Value> {
    let message_turn = message.turn_id.as_deref();
    active_conditions
        .iter()
        .filter(|condition| {
            // A wait condition created during the same turn that the current
            // message triggered must not be reconciled by that message.  The
            // wait is meant to be resumed by a *future* event; the triggering
            // message is by definition "before" the wait was registered.
            message_turn
                .zip(condition.turn_id.as_deref())
                .is_none_or(|(mt, ct)| mt != ct)
        })
        .filter_map(|condition| reconciliation_signal_for_condition(message, condition))
        .collect()
}

fn wait_condition_parts(
    wake: WaitForWakeKind,
    resource: Option<String>,
    timer_wake_at: Option<DateTime<Utc>>,
    external_trigger_id: Option<String>,
) -> Result<(WaitConditionKind, Option<String>, Vec<WakeSource>)> {
    match wake {
        WaitForWakeKind::OperatorInput => Ok((
            WaitConditionKind::Operator,
            resource,
            vec![WakeSource::OperatorInput],
        )),
        WaitForWakeKind::TaskResult => {
            let task_id = wait_resource_required(wake, resource)?;
            Ok((
                WaitConditionKind::Task,
                Some(task_id.clone()),
                vec![WakeSource::TaskResult { task_id }],
            ))
        }
        WaitForWakeKind::External => Ok((
            WaitConditionKind::External,
            optional_wait_resource(resource),
            vec![WakeSource::ExternalIngress {
                external_trigger_id,
            }],
        )),
        WaitForWakeKind::Timer => {
            let timer_id = wait_resource_required(wake, resource)?;
            let wake_at =
                timer_wake_at.ok_or_else(|| anyhow!("wait_for timer wake time is unavailable"))?;
            Ok((
                WaitConditionKind::Timer,
                Some(timer_id),
                vec![WakeSource::Timer { wake_at }],
            ))
        }
        WaitForWakeKind::System => Ok((
            WaitConditionKind::System,
            optional_wait_resource(resource),
            vec![WakeSource::SystemTick],
        )),
    }
}

fn optional_wait_resource(resource: Option<String>) -> Option<String> {
    resource
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn wait_resource_required(wake: WaitForWakeKind, resource: Option<String>) -> Result<String> {
    optional_wait_resource(resource)
        .ok_or_else(|| anyhow!("wait_for {:?} requires non-empty resource", wake))
}

#[cfg(test)]
mod wait_condition_parts_tests {
    use super::*;

    #[test]
    fn timer_wait_uses_supplied_timer_identity_and_deadline() {
        let wake_at = Utc::now() + chrono::Duration::seconds(30);
        let (kind, subject_ref, wake_sources) = wait_condition_parts(
            WaitForWakeKind::Timer,
            Some("timer-1".into()),
            Some(wake_at),
            None,
        )
        .unwrap();

        assert_eq!(kind, WaitConditionKind::Timer);
        assert_eq!(subject_ref.as_deref(), Some("timer-1"));
        assert_eq!(wake_sources, vec![WakeSource::Timer { wake_at }]);
    }

    #[test]
    fn system_wait_uses_system_tick_source() {
        let (kind, subject_ref, wake_sources) =
            wait_condition_parts(WaitForWakeKind::System, None, None, None).unwrap();

        assert_eq!(kind, WaitConditionKind::System);
        assert_eq!(subject_ref, None);
        assert_eq!(wake_sources, vec![WakeSource::SystemTick]);
    }
}

fn recheck_at_from(now: DateTime<Utc>, recheck_after_ms: u64) -> DateTime<Utc> {
    let recheck_after_ms = i64::try_from(recheck_after_ms).unwrap_or(i64::MAX);
    let recheck_after =
        chrono::Duration::try_milliseconds(recheck_after_ms).unwrap_or(chrono::Duration::MAX);
    now.checked_add_signed(recheck_after)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn reconciliation_signal_for_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<serde_json::Value> {
    let (wake_source, subject_ref) = matching_wake_source(message, condition)?;
    let dedupe_key = format!(
        "wait_reconciliation:{}:{}:{}",
        condition.id, wake_source, message.id
    );
    Some(serde_json::json!({
        "dedupe_key": dedupe_key,
        "message_id": message.id,
        "trigger_kind": message.trigger_kind,
        "wait_condition_id": condition.id,
        "wake_source": wake_source,
        "work_item_id": condition.work_item_id,
        "subject_ref": subject_ref.or_else(|| condition.subject_ref.clone()),
        "waiting_for": condition.waiting_for,
        "source": condition.source,
    }))
}

fn matching_wake_source(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<(String, Option<String>)> {
    if condition.status == WaitConditionStatus::Triggered
        && condition.trigger_message_id() != Some(message.id.as_str())
    {
        return None;
    }
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id))
            .then(|| ("task_result".to_string(), Some(task_id.clone()))),
        (MessageKind::CallbackEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            let correlated_wait_id = message.source_refs.get("wait_id");
            condition
                .wake_sources
                .iter()
                .any(|source| match source {
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some(id),
                    } => {
                        external_trigger_id == Some(id) && correlated_wait_id == Some(&condition.id)
                    }
                    _ => false,
                })
                .then(|| ("external_ingress".to_string(), external_trigger_id.cloned()))
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => (condition
            .subject_ref
            .as_deref()
            .is_none_or(|subject_ref| subject_ref == timer_id)
            && condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::Timer { .. })))
        .then(|| ("timer".to_string(), Some(timer_id.clone()))),
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { actor_id }) => {
            let owner_matches = match message.work_item_id.as_deref() {
                Some(work_item_id) => condition.work_item_id.as_deref() == Some(work_item_id),
                None => condition.work_item_id.is_none(),
            };
            (owner_matches
                && condition
                    .wake_sources
                    .iter()
                    .any(|source| matches!(source, WakeSource::OperatorInput)))
            .then(|| ("operator_input".to_string(), actor_id.clone()))
        }
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if let Some(external) = matching_wake_hint_external_source(message, condition) {
                return Some(external);
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
                .then(|| ("system_tick".to_string(), Some(subsystem.clone())))
        }
        _ => None,
    }
}

fn matching_wake_hint_external_source(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<(String, Option<String>)> {
    let wake_hint = message.metadata.as_ref()?.get("wake_hint")?;
    let external_trigger_id = wake_hint
        .get("external_trigger_id")
        .and_then(serde_json::Value::as_str);
    let correlated_wait_id = message.source_refs.get("wait_id");
    let matches_external = condition.wake_sources.iter().any(|source| match source {
        WakeSource::ExternalIngress {
            external_trigger_id: Some(id),
        } => Some(id.as_str()) == external_trigger_id && correlated_wait_id == Some(&condition.id),
        _ => false,
    });
    matches_external.then(|| {
        (
            "external_ingress".to_string(),
            external_trigger_id.map(ToString::to_string),
        )
    })
}

fn task_expectation(task: &TaskRecord) -> crate::runtime_db::transitions::TaskExpectation {
    crate::runtime_db::transitions::TaskExpectation {
        id: task.id.clone(),
        agent_id: task.agent_id.clone(),
        work_item_id: task.work_item_id.clone(),
        status: task.status.clone(),
        updated_at: task.updated_at,
        result_message_id: task.parent_message_id.clone(),
    }
}

fn advance_time(base: chrono::DateTime<Utc>, delta_ms: u64) -> Result<chrono::DateTime<Utc>> {
    let delta_ms = i64::try_from(delta_ms).context("duration_ms exceeds supported timer range")?;
    let delta = chrono::Duration::try_milliseconds(delta_ms)
        .ok_or_else(|| anyhow!("duration_ms exceeds supported timer range"))?;
    Ok(base + delta)
}

fn normalize_recovered_timer(mut timer: TimerRecord, now: DateTime<Utc>) -> TimerRecord {
    if timer.next_fire_at.is_some() {
        return timer;
    }

    let anchor = timer.last_fired_at.unwrap_or(timer.created_at);
    let fallback_ms = timer.interval_ms.unwrap_or(timer.duration_ms);
    timer.next_fire_at = advance_time(anchor, fallback_ms).ok().or(Some(now));
    timer
}
