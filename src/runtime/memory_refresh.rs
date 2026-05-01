use super::*;
use crate::{
    storage::WorkQueuePromptProjection,
    types::{
        BriefKind, TaskStatus, WorkPlanStepStatus, WorkReactivationMode, WorkReactivationSignal,
    },
};

const CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT: usize = 512;

#[derive(Debug, Clone)]
enum IdleTickTrigger {
    WorkQueueActive(crate::types::WorkItemRecord),
    WorkQueueQueued(crate::types::WorkItemRecord),
    WakeHint(PendingWakeHint),
}

pub(super) fn work_queue_reactivation_signal(
    projection: &WorkQueuePromptProjection,
) -> Option<WorkReactivationSignal> {
    if let Some(current) = projection.current.as_ref() {
        return Some(WorkReactivationSignal {
            work_item_id: current.id.clone(),
            state: current.state.clone(),
            reactivation_mode: WorkReactivationMode::ContinueActive,
        });
    }
    projection
        .queued_blocked
        .iter()
        .find(|item| item.blocked_by.is_none())
        .map(|queued| WorkReactivationSignal {
            work_item_id: queued.id.clone(),
            state: queued.state.clone(),
            reactivation_mode: WorkReactivationMode::ActivateQueued,
        })
}

fn idle_tick_trigger_from_state(
    pending_wake_hint: Option<PendingWakeHint>,
    projection: WorkQueuePromptProjection,
) -> Option<IdleTickTrigger> {
    if let Some(pending) = pending_wake_hint {
        Some(IdleTickTrigger::WakeHint(pending))
    } else if let Some(current) = projection.current {
        Some(IdleTickTrigger::WorkQueueActive(current))
    } else {
        projection
            .queued_blocked
            .into_iter()
            .find(|item| item.blocked_by.is_none())
            .map(IdleTickTrigger::WorkQueueQueued)
    }
}

impl RuntimeHandle {
    pub(super) async fn arm_continue_active_suppression(&self) {
        let mut guard = self.inner.suppress_next_continue_active_tick.lock().await;
        *guard = true;
    }

    async fn take_continue_active_suppression(&self) -> bool {
        let mut guard = self.inner.suppress_next_continue_active_tick.lock().await;
        let suppressed = *guard;
        *guard = false;
        suppressed
    }

    pub(super) async fn persist_last_continuation(
        &self,
        continuation_resolution: &ContinuationResolution,
    ) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        guard.state.last_continuation = Some(continuation_resolution.clone());
        self.inner.storage.write_agent(&guard.state)?;
        Ok(())
    }

    pub(super) async fn record_continuation_trigger_received(
        &self,
        message: &MessageEnvelope,
        trigger: &ContinuationTrigger,
        prior_closure: &ClosureDecision,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::new(
            "continuation_trigger_received",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "message_id": message.id,
                "trigger_kind": trigger.kind,
                "contentful": trigger.contentful,
                "task_terminal": trigger.task_terminal,
                "task_blocking": trigger.task_blocking,
                "wake_hint_source": trigger.wake_hint_source,
                "prior_closure_outcome": prior_closure.outcome,
                "prior_waiting_reason": prior_closure.waiting_reason,
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn record_continuation_resolution_event(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: &ContinuationResolution,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::new(
            "continuation_resolved",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "message_id": message.id,
                "resolution": continuation_resolution,
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn maybe_emit_pending_system_tick(
        &self,
        triggering_continuation: Option<&ContinuationResolution>,
    ) -> Result<bool> {
        let pending_wake_hint = {
            let guard = self.inner.agent.lock().await;
            let eligible = matches!(
                guard.state.status,
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep
            ) && guard.queue.is_empty();
            if !eligible {
                return Ok(false);
            }

            guard.state.pending_wake_hint.clone()
        };

        let projection = self.inner.storage.work_queue_prompt_projection()?;
        let trigger = idle_tick_trigger_from_state(pending_wake_hint, projection);

        let suppress_continue_active = triggering_continuation
            .is_some_and(|continuation| continuation.model_visible)
            || self.take_continue_active_suppression().await;

        match trigger {
            Some(IdleTickTrigger::WorkQueueActive(active)) => {
                if suppress_continue_active {
                    return Ok(false);
                }
                if let Some(result_brief_id) =
                    self.duplicate_continue_active_result_brief_id(&active)?
                {
                    self.inner.storage.append_event(&AuditEvent::new(
                        "system_tick_suppressed",
                        serde_json::json!({
                            "subsystem": "work_queue",
                            "reason": "no_new_signal_after_result_brief",
                            "work_item_id": active.id,
                            "result_brief_id": result_brief_id,
                        }),
                    ))?;
                    return Ok(false);
                }
                self.emit_system_tick_from_work_queue(&active, "continue_active", false)
                    .await?;
                Ok(true)
            }
            Some(IdleTickTrigger::WorkQueueQueued(queued)) => {
                if let Some(active) = self.activate_queued_work_item(&queued.id).await? {
                    self.emit_system_tick_from_work_queue(&active, "activate_queued", true)
                        .await?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Some(IdleTickTrigger::WakeHint(pending)) => {
                self.emit_system_tick_from_wake_hint(&pending).await?;

                #[cfg(test)]
                crate::runtime::test_util::wait_at_checkpoint().await;

                let mut guard = self.inner.agent.lock().await;
                if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                    guard.state.pending_wake_hint = None;
                    self.inner.storage.write_agent(&guard.state)?;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn duplicate_continue_active_result_brief_id(
        &self,
        work_item: &crate::types::WorkItemRecord,
    ) -> Result<Option<String>> {
        let recent_briefs = self
            .inner
            .storage
            .read_recent_briefs(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?;
        let Some(result_brief) =
            latest_nonempty_result_brief_for_work_item(&recent_briefs, &work_item.id)
        else {
            return Ok(None);
        };

        if self.has_work_signal_after_result(work_item, &result_brief)? {
            return Ok(None);
        }

        Ok(Some(result_brief.id.clone()))
    }

    fn has_work_signal_after_result(
        &self,
        work_item: &crate::types::WorkItemRecord,
        result_brief: &BriefRecord,
    ) -> Result<bool> {
        let work_item_id = work_item.id.as_str();

        if work_item.updated_at > result_brief.created_at {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .latest_work_plan(&work_item.id)?
            .is_some_and(|plan| {
                plan.created_at > result_brief.created_at
                    || plan
                        .items
                        .iter()
                        .any(|item| item.status != WorkPlanStepStatus::Completed)
            })
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|message| {
                message.created_at > result_brief.created_at
                    && !is_runtime_continue_active_message_for_work_item(message, work_item_id)
            })
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .read_recent_tool_executions(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|tool| {
                tool.created_at > result_brief.created_at
                    && tool
                        .work_item_id
                        .as_deref()
                        .is_none_or(|id| id == work_item_id)
            })
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .latest_task_records()?
            .iter()
            .any(|task| {
                task.updated_at > result_brief.created_at
                    && matches!(
                        task.status,
                        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                    )
            })
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .read_recent_waiting_intents(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|waiting| {
                waiting
                    .work_item_id
                    .as_deref()
                    .is_none_or(|id| id == work_item_id)
                    && (waiting.created_at > result_brief.created_at
                        || waiting
                            .last_triggered_at
                            .is_some_and(|triggered_at| triggered_at > result_brief.created_at))
            })
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .read_recent_events(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|event| {
                event.kind == "runtime_error" && event.created_at > result_brief.created_at
            })
        {
            return Ok(true);
        }

        Ok(false)
    }

    pub(super) async fn emit_system_tick_from_wake_hint(
        &self,
        pending: &PendingWakeHint,
    ) -> Result<()> {
        let correlation_id = pending.correlation_id.clone();
        let causation_id = pending.causation_id.clone();
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wake_hint".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Next,
            MessageBody::Text {
                text: format!("wake hint: {}", pending.reason),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(serde_json::json!({
            "wake_hint": {
                "reason": pending.reason,
                "source": pending.source,
                "resource": pending.resource,
                "body": pending.body,
                "content_type": pending.content_type,
                "correlation_id": correlation_id,
                "causation_id": causation_id,
                "created_at": pending.created_at,
            }
        }));
        message.correlation_id = correlation_id;
        message.causation_id = causation_id;
        self.inner.storage.append_event(&AuditEvent::new(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "wake_hint",
                "wake_hint": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("wake_hint"))
                    .cloned(),
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        Ok(())
    }

    pub(super) async fn emit_system_tick_from_work_queue(
        &self,
        work_item: &crate::types::WorkItemRecord,
        reason: &str,
        activated_from_queue: bool,
    ) -> Result<()> {
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: if activated_from_queue {
                    format!(
                        "Activate and continue queued work item: {}",
                        work_item.delivery_target
                    )
                } else {
                    format!("Continue current work item: {}", work_item.delivery_target)
                },
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(serde_json::json!({
            "work_queue": {
                "reason": reason,
                "work_item_id": work_item.id,
                "delivery_target": work_item.delivery_target,
                "state": work_item.state,
                "activated_from_queue": activated_from_queue,
            }
        }));
        self.inner.storage.append_event(&AuditEvent::new(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "work_queue",
                "work_queue": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .cloned(),
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        Ok(())
    }

    pub(super) async fn activate_queued_work_item(
        &self,
        work_item_id: &str,
    ) -> Result<Option<crate::types::WorkItemRecord>> {
        let projection = self.inner.storage.work_queue_prompt_projection()?;
        if projection.current.is_some() {
            return Ok(None);
        }

        let Some(latest) = self.inner.storage.latest_work_item(work_item_id)? else {
            return Ok(None);
        };
        if latest.state != crate::types::WorkItemState::Open || latest.blocked_by.is_some() {
            return Ok(None);
        }
        let record = latest.clone();
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.current_work_item_id = Some(record.id.clone());
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "work_item_picked",
            serde_json::json!({
                "action": "queue_activated",
                "current_work_item_id": record.id,
            }),
        ))?;
        self.inner.storage.append_event(&AuditEvent::new(
            "work_item_queue_activated",
            serde_json::json!({
                "work_item_id": record.id,
                "delivery_target": record.delivery_target,
            }),
        ))?;
        Ok(Some(record))
    }

    pub(super) fn current_work_reactivation_signal(
        &self,
    ) -> Result<Option<WorkReactivationSignal>> {
        let projection = self.inner.storage.work_queue_prompt_projection()?;
        Ok(work_queue_reactivation_signal(&projection))
    }

    pub(super) async fn emit_system_tick_from_interrupted_tasks(
        &self,
        tasks: &[TaskRecord],
    ) -> Result<()> {
        let items = tasks
            .iter()
            .map(|task| {
                serde_json::json!({
                    "task_id": task.id,
                    "kind": task.kind,
                    "status_before_restart": task
                        .detail
                        .as_ref()
                        .and_then(|detail| detail.get("status_before_restart"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("running"),
                    "summary": task.summary,
                    "wait_policy": task.wait_policy(),
                })
            })
            .collect::<Vec<_>>();
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "task_restart".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Next,
            MessageBody::Text {
                text: format!(
                    "runtime restarted and interrupted {} task{}",
                    tasks.len(),
                    if tasks.len() == 1 { "" } else { "s" }
                ),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(serde_json::json!({
            "interrupted_tasks": {
                "count": tasks.len(),
                "items": items,
            }
        }));
        self.inner.storage.append_event(&AuditEvent::new(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "task_restart",
                "interrupted_tasks": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("interrupted_tasks"))
                    .cloned(),
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        Ok(())
    }
}

fn latest_nonempty_result_brief_for_work_item<'a>(
    briefs: &'a [BriefRecord],
    work_item_id: &str,
) -> Option<&'a BriefRecord> {
    briefs
        .iter()
        .filter(|brief| {
            brief.kind == BriefKind::Result
                && brief.work_item_id.as_deref() == Some(work_item_id)
                && !brief.text.trim().is_empty()
        })
        .max_by_key(|brief| brief.created_at)
}

fn is_runtime_continue_active_message_for_work_item(
    message: &MessageEnvelope,
    work_item_id: &str,
) -> bool {
    matches!(
        (&message.kind, &message.origin),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) if subsystem == "work_queue"
    ) && message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("work_queue"))
        .is_some_and(|metadata| {
            metadata.get("reason").and_then(|value| value.as_str()) == Some("continue_active")
                && metadata
                    .get("work_item_id")
                    .and_then(|value| value.as_str())
                    == Some(work_item_id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextConfig;
    use crate::provider::StubProvider;
    use crate::types::{
        AgentStatus, WorkItemRecord, WorkItemState, WorkPlanItem, WorkPlanSnapshot,
    };
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    struct TestRuntime {
        runtime: RuntimeHandle,
        _dir: TempDir,
        _workspace: TempDir,
    }

    fn test_runtime() -> TestRuntime {
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

        TestRuntime {
            runtime,
            _dir: dir,
            _workspace: workspace,
        }
    }

    fn set_agent_idle(test_runtime: &TestRuntime) {
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.status = AgentStatus::AwakeIdle;
        // Don't write - just update in-memory state for the test
    }

    fn clear_queue(test_runtime: &TestRuntime) {
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        while guard.queue.pop().is_some() {
            // Pop all messages
        }
        // Don't write - just update in-memory state for the test
    }

    fn set_agent_status(test_runtime: &TestRuntime, status: AgentStatus) {
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.status = status;
        // Don't write - just update in-memory state for the test
    }

    fn add_queued_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let record = WorkItemRecord {
            id: id.to_string(),
            agent_id: "default".to_string(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.to_string(),
            delivery_target: target.to_string(),
            state: WorkItemState::Open,
            blocked_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        test_runtime
            .runtime
            .inner
            .storage
            .append_work_item(&record)
            .unwrap();
        record
    }

    fn add_current_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let record = WorkItemRecord {
            id: id.to_string(),
            agent_id: "default".to_string(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.to_string(),
            delivery_target: target.to_string(),
            state: WorkItemState::Open,
            blocked_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        test_runtime
            .runtime
            .inner
            .storage
            .append_work_item(&record)
            .unwrap();
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.current_work_item_id = Some(record.id.clone());
        test_runtime
            .runtime
            .inner
            .storage
            .write_agent(&guard.state)
            .unwrap();
        record
    }

    fn append_result_brief_for_work_item(
        test_runtime: &TestRuntime,
        work_item_id: &str,
        text: &str,
    ) -> BriefRecord {
        let mut brief = BriefRecord::new("default", BriefKind::Result, text, None, None);
        brief.work_item_id = Some(work_item_id.to_string());
        brief.created_at = chrono::Utc::now();
        test_runtime
            .runtime
            .inner
            .storage
            .append_brief(&brief)
            .unwrap();
        brief
    }

    fn set_wake_hint(test_runtime: &TestRuntime, reason: &str) -> PendingWakeHint {
        let hint = PendingWakeHint {
            reason: reason.to_string(),
            source: Some("test".to_string()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("test-correlation".to_string()),
            causation_id: Some("test-causation".to_string()),
            created_at: chrono::Utc::now(),
        };
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.pending_wake_hint = Some(hint.clone());
        // Don't write - just update in-memory state for the test
        hint
    }

    fn get_emitted_system_ticks(test_runtime: &TestRuntime) -> Vec<(String, serde_json::Value)> {
        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(100)
            .unwrap();
        events
            .into_iter()
            .filter(|e| e.kind == "system_tick_emitted")
            .filter_map(|e| {
                e.data.get("subsystem").and_then(|subsystem| {
                    subsystem.as_str().map(|s| {
                        let metadata = e.data.get(s).cloned().unwrap_or(serde_json::json!(null));
                        (s.to_string(), metadata)
                    })
                })
            })
            .collect()
    }

    #[test]
    fn queue_nonempty_suppresses_idle_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Add a message directly to the in-memory queue
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Test message".to_string(),
            },
        );

        // Push to the in-memory queue
        {
            let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
            guard.queue.push(message);
        }

        // Attempt to emit idle tick - should be suppressed because queue is nonempty
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "Idle tick should be suppressed when queue is nonempty"
        );
    }

    #[test]
    fn pending_wake_hint_takes_precedence_over_current_work_item() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Add both a wake hint and an current work item
        set_wake_hint(&test_runtime, "wake-test");
        add_current_work_item(&test_runtime, "wi-active", "active-target");

        // Emit system tick
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "System tick should be emitted");

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "wake_hint", "Wake hint should take precedence");

        // Verify wake hint was cleared
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(
            guard.state.pending_wake_hint.is_none(),
            "Wake hint should be cleared after emission"
        );
    }

    #[test]
    fn current_work_item_takes_precedence_over_queued_activation() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Add both active and queued work items
        add_current_work_item(&test_runtime, "wi-active", "active-target");
        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        // Emit system tick
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "System tick should be emitted");

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");

        // Verify it was for continuing the active item, not activating queued
        let metadata = &ticks[0].1;
        assert_eq!(
            metadata["reason"].as_str().unwrap(),
            "continue_active",
            "Active item should be continued, not queued item activated"
        );
    }

    #[test]
    fn queued_item_activation_skipped_when_active_exists() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Create an current work item first
        add_current_work_item(&test_runtime, "wi-active", "active-target");

        // Try to manually activate a queued item - should fail
        let queued_id = "wi-queued";
        add_queued_work_item(&test_runtime, queued_id, "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(test_runtime.runtime.activate_queued_work_item(queued_id))
            .unwrap();

        assert!(
            result.is_none(),
            "Activation should be skipped when active item exists"
        );

        // Verify queued item wasn't activated
        let projection = test_runtime
            .runtime
            .inner
            .storage
            .work_queue_prompt_projection()
            .unwrap();
        assert!(
            projection.current.is_some(),
            "Active item should still exist"
        );
        assert_eq!(projection.current.unwrap().id, "wi-active");
    }

    #[test]
    fn queued_item_activation_emits_audit_events() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Add only a queued work item
        let queued_id = "wi-queued";
        let _queued_item = add_queued_work_item(&test_runtime, queued_id, "queued-target");

        // Activate it
        let rt = tokio::runtime::Runtime::new().unwrap();
        let activated = rt
            .block_on(test_runtime.runtime.activate_queued_work_item(queued_id))
            .unwrap();

        assert!(activated.is_some(), "Queued item should be activated");

        // Check audit events
        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(100)
            .unwrap();
        let picked_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "work_item_picked")
            .filter(|e| {
                e.data
                    .get("action")
                    .and_then(|a| a.as_str())
                    .map(|a| a == "queue_activated")
                    .unwrap_or(false)
            })
            .collect();

        assert!(
            !picked_events.is_empty(),
            "work_item_picked event should be emitted"
        );

        let activated_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "work_item_queue_activated")
            .collect();

        assert!(
            !activated_events.is_empty(),
            "work_item_queue_activated event should be emitted"
        );

        // Verify the activated record
        let activated_record = activated.unwrap();
        assert_eq!(activated_record.state, WorkItemState::Open);
        assert_eq!(activated_record.id, queued_id);
    }

    #[test]
    fn wake_hint_system_tick_preserves_metadata() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Set wake hint with full metadata
        let hint = PendingWakeHint {
            reason: "test-wake".to_string(),
            source: Some("test-source".to_string()),
            resource: Some("test-resource".to_string()),
            body: Some(MessageBody::Json {
                value: serde_json::json!({"key": "value"}),
            }),
            content_type: Some("application/json".to_string()),
            correlation_id: Some("corr-123".to_string()),
            causation_id: Some("caus-456".to_string()),
            created_at: chrono::Utc::now(),
        };

        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.pending_wake_hint = Some(hint.clone());
        drop(guard);

        // Emit system tick
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "System tick should be emitted");

        // Check that the message was enqueued with preserved metadata
        let messages = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_messages(10)
            .unwrap();
        let system_tick = messages.iter().find(|m| {
            matches!(m.kind, MessageKind::SystemTick)
                && matches!(
                    &m.origin,
                    MessageOrigin::System { subsystem } if subsystem == "wake_hint"
                )
        });

        assert!(
            system_tick.is_some(),
            "Wake hint system tick should be enqueued"
        );

        let tick = system_tick.unwrap();
        assert_eq!(tick.correlation_id, Some("corr-123".to_string()));
        assert_eq!(tick.causation_id, Some("caus-456".to_string()));

        // Check metadata preservation
        let metadata = tick.metadata.as_ref().unwrap();
        assert!(metadata.get("wake_hint").is_some());

        let wake_hint_meta = &metadata["wake_hint"];
        assert_eq!(wake_hint_meta["reason"].as_str().unwrap(), "test-wake");
        assert_eq!(wake_hint_meta["source"].as_str().unwrap(), "test-source");
        assert_eq!(
            wake_hint_meta["resource"].as_str().unwrap(),
            "test-resource"
        );
        assert_eq!(
            wake_hint_meta["content_type"].as_str().unwrap(),
            "application/json"
        );

        // body serializes Json variant with a "value" wrapper
        assert!(
            wake_hint_meta.get("body").is_some(),
            "body field should exist"
        );
        let body = &wake_hint_meta["body"];
        assert!(body.get("value").is_some(), "body should have value field");
        assert_eq!(body["value"]["key"].as_str().unwrap(), "value");
    }

    #[test]
    fn wake_hint_cleared_only_if_still_pending() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Set initial wake hint
        set_wake_hint(&test_runtime, "original-hint");

        // Call the real production path
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();
        assert!(emitted, "Should emit tick for wake hint");

        // Verify hint was cleared (same hint was still pending)
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(
            guard.state.pending_wake_hint.is_none(),
            "Hint should be cleared when it's still the same one"
        );
        drop(guard);

        // Clear the queue and reset status for next test
        // (system tick enqueues a message, which would block the next tick)
        clear_queue(&test_runtime);
        set_agent_idle(&test_runtime);

        // Set a new hint
        set_wake_hint(&test_runtime, "new-hint");

        // Verify the new hint exists
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        let current_hint = guard.state.pending_wake_hint.as_ref().unwrap();
        assert_eq!(current_hint.reason, "new-hint");
        drop(guard);

        // Call the real production path again
        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();
        assert!(emitted, "Should emit tick again");

        // Verify hint was cleared again
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(
            guard.state.pending_wake_hint.is_none(),
            "Hint should be cleared after emission"
        );
    }

    #[test]
    fn wake_hint_preserved_when_replaced_during_emission() {
        // Test the replacement race scenario:
        // 1. Old hint starts emitting
        // 2. Before emission completes, new hint replaces old hint
        // 3. After emission, new hint should be preserved (not cleared)

        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Set initial wake hint
        set_wake_hint(&test_runtime, "old-hint");

        // Capture the old hint for later comparison
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        let old_hint = guard.state.pending_wake_hint.clone().unwrap();
        drop(guard);

        // Start emission via the real production path
        // This will:
        // 1. Capture the old hint
        // 2. Emit the tick
        // 3. Check if pending_wake_hint == old_hint before clearing
        let rt = tokio::runtime::Runtime::new().unwrap();
        let test_runtime_clone = test_runtime.runtime.clone();
        let handle = rt.spawn(async move {
            test_runtime_clone
                .maybe_emit_pending_system_tick(None)
                .await
        });

        // Give the emission a moment to start, then replace the hint
        // In a real race, this would happen during emit_system_tick_from_wake_hint
        // Before the guard is reacquired for the clearing check
        std::thread::sleep(std::time::Duration::from_millis(10));
        set_wake_hint(&test_runtime, "new-hint");

        // Wait for emission to complete
        let emitted = rt.block_on(handle).unwrap().unwrap();
        assert!(emitted, "Should emit tick for old hint");

        // Critical assertion: The new hint should be preserved
        // The protection `if guard.state.pending_wake_hint.as_ref() == Some(&pending)`
        // should prevent clearing the new hint that was set during emission
        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        let current_hint = guard.state.pending_wake_hint.as_ref();
        assert!(
            current_hint.is_some(),
            "New hint should be preserved, not cleared by old emission"
        );
        assert_eq!(
            current_hint.unwrap().reason,
            "new-hint",
            "Should preserve the new hint, not the old one"
        );
        assert_ne!(
            current_hint.unwrap().reason,
            old_hint.reason,
            "Old hint should have been replaced"
        );
    }

    #[test]
    fn restart_ordering_deterministic_with_wake_hint() {
        let test_runtime = test_runtime();
        set_agent_status(&test_runtime, AgentStatus::Asleep);

        // Wake hint should be processed before work items
        set_wake_hint(&test_runtime, "wake-first");
        add_current_work_item(&test_runtime, "wi-active", "active-target");
        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted);

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "wake_hint", "Wake hint should be prioritized");
    }

    #[test]
    fn restart_ordering_deterministic_without_wake_hint() {
        let test_runtime = test_runtime();
        set_agent_status(&test_runtime, AgentStatus::Asleep);

        // Without wake hint, current work item should be prioritized
        add_current_work_item(&test_runtime, "wi-active", "active-target");
        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted);

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");

        let metadata = &ticks[0].1;
        assert_eq!(metadata["reason"].as_str().unwrap(), "continue_active");
    }

    #[test]
    fn duplicate_continue_active_suppressed_after_result_without_new_signal() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        append_result_brief_for_work_item(&test_runtime, &active.id, "Already answered.");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "continue_active should be suppressed after a result brief with no new signal"
        );
        assert!(
            get_emitted_system_ticks(&test_runtime).is_empty(),
            "suppression must not enqueue a model-visible system tick"
        );

        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(20)
            .unwrap();
        let active_id = active.id.as_str();
        assert!(events.iter().any(|event| {
            event.kind == "system_tick_suppressed"
                && event.data["reason"] == "no_new_signal_after_result_brief"
                && event.data["work_item_id"] == active_id
        }));
    }

    #[test]
    fn continue_active_preserved_after_new_operator_signal() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        append_result_brief_for_work_item(&test_runtime, &active.id, "Initial result.");

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "New follow-up".to_string(),
            },
        );
        test_runtime
            .runtime
            .inner
            .storage
            .append_message(&message)
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "a newer operator message should keep continue_active eligible"
        );

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");
        assert_eq!(ticks[0].1["reason"].as_str(), Some("continue_active"));
    }

    #[test]
    fn continue_active_preserved_when_work_plan_has_pending_step() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        test_runtime
            .runtime
            .inner
            .storage
            .append_work_plan(&WorkPlanSnapshot::new(
                "default",
                &active.id,
                vec![WorkPlanItem {
                    step: "finish remaining implementation".to_string(),
                    status: WorkPlanStepStatus::InProgress,
                }],
            ))
            .unwrap();
        append_result_brief_for_work_item(&test_runtime, &active.id, "Progress report.");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "an unfinished work plan should keep continue_active eligible"
        );
    }

    #[test]
    fn restart_ordering_activates_queued_when_no_active() {
        let test_runtime = test_runtime();
        set_agent_status(&test_runtime, AgentStatus::Asleep);

        // Without wake hint or active item, queued item should be activated
        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted);

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");

        let metadata = &ticks[0].1;
        assert_eq!(metadata["reason"].as_str().unwrap(), "activate_queued");
        assert!(metadata["activated_from_queue"].as_bool().unwrap());
    }

    #[test]
    fn restart_ordering_no_tick_when_ineligible_status() {
        let test_runtime = test_runtime();

        // ineligible status suppresses idle tick emission
        set_agent_status(&test_runtime, AgentStatus::AwakeRunning);

        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "No tick should be emitted when agent is not in eligible status"
        );
    }
}
