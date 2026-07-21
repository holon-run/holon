use super::*;
use crate::{
    storage::WorkQueueReadModel,
    types::{BriefKind, TaskStatus, TodoItemState},
};

const CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT: usize = 512;

#[derive(Debug, Clone)]
enum IdleTickTrigger {
    WorkQueueActive(crate::types::WorkItemRecord),
    WorkQueueQueued(crate::types::WorkItemRecord),
    BlockedRecheck(Vec<crate::types::WorkItemRecord>),
    WakeHint(PendingWakeHint),
}

fn idle_tick_trigger_from_state(
    pending_wake_hint: Option<PendingWakeHint>,
    projection: WorkQueueReadModel,
    due_rechecks: Vec<crate::types::WorkItemRecord>,
) -> Option<IdleTickTrigger> {
    if let Some(pending) = pending_wake_hint {
        Some(IdleTickTrigger::WakeHint(pending))
    } else {
        if let Some(current) = projection.current_runnable {
            return Some(IdleTickTrigger::WorkQueueActive(current.work_item));
        }
        projection
            .queued_runnable
            .into_iter()
            .next()
            .map(|item| IdleTickTrigger::WorkQueueQueued(item.work_item))
            .or_else(|| {
                if due_rechecks.is_empty() {
                    None
                } else {
                    Some(IdleTickTrigger::BlockedRecheck(due_rechecks))
                }
            })
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
        guard.persist_state(&self.inner.storage)?;
        Ok(())
    }

    pub(super) async fn record_continuation_trigger_received(
        &self,
        message: &MessageEnvelope,
        trigger: &ContinuationTrigger,
        prior_closure: &ClosureDecision,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::legacy(
            "continuation_trigger_received",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "message_id": message.id,
                "trigger_kind": trigger.kind,
                "contentful": trigger.contentful,
                "task_terminal": trigger.task_terminal,
                "wake_hint_source": trigger.wake_hint_source,
                "prior_closure_outcome": prior_closure.outcome,
                "prior_waiting_reason": prior_closure.waiting_reason
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn record_continuation_resolution_event(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: &ContinuationResolution,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::legacy(
            "continuation_resolved",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "message_id": message.id,
                "resolution": continuation_resolution
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn maybe_emit_pending_system_tick(
        &self,
        triggering_continuation: Option<&ContinuationResolution>,
    ) -> Result<bool> {
        let (scheduler_snapshot, queue_len, pending_wake_hint) = {
            let guard = self.inner.agent.lock().await;
            let eligible = matches!(
                guard.state.status,
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep
            ) && guard.queue.is_empty();
            if !eligible {
                if guard.state.status == AgentStatus::AwakeRunning || !guard.queue.is_empty() {
                    let agent_id = guard.state.id.clone();
                    drop(guard);
                    self.consume_due_work_item_rechecks(&agent_id).await?;
                }
                return Ok(false);
            }

            (
                scheduler::SchedulerAgentSnapshot::from_state(&guard.state),
                guard.queue.len(),
                guard.state.pending_wake_hint.clone(),
            )
        };

        let work_queue_projection = self.inner.storage.work_queue_prompt_projection()?;
        let due_rechecks = self
            .inner
            .storage
            .due_blocked_work_item_rechecks(scheduler_snapshot.id(), self.now())?;
        let scheduler_projection =
            scheduler::SchedulerProjection::from_snapshot_with_queue_len_and_work_queue_at(
                &self.inner.storage,
                &scheduler_snapshot,
                queue_len,
                work_queue_projection.clone(),
                self.now(),
            )?;
        let trigger = idle_tick_trigger_from_state(
            pending_wake_hint,
            work_queue_projection,
            due_rechecks.clone(),
        );

        let suppress_continue_active = triggering_continuation
            .is_some_and(|continuation| continuation.model_reentry)
            || self.take_continue_active_suppression().await;

        match trigger {
            Some(IdleTickTrigger::WorkQueueActive(active)) => {
                let duplicate = self
                    .duplicate_continue_active_result_brief_id(&active)?
                    .map(scheduler::SchedulerDuplicateEvidence::ContinueActiveBrief);
                let decision = scheduler::decide_next_action(
                    &scheduler_projection,
                    scheduler::SchedulerBoundary::IdleTick,
                    scheduler::SchedulerInput::IdleSignal(
                        scheduler::SchedulerIdleSignal::ContinueActive {
                            work_item: &active,
                            suppressed_after_model_reentry_continuation: suppress_continue_active,
                            duplicate: duplicate.clone(),
                        },
                    ),
                );
                if !matches!(
                    decision.kind,
                    scheduler::SchedulerDecisionKind::EmitSystemTick
                ) {
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                    if let Some(scheduler::SchedulerDuplicateEvidence::ContinueActiveBrief(
                        result_brief_id,
                    )) = duplicate
                    {
                        self.inner.storage.append_event(&AuditEvent::legacy(
                            "system_tick_suppressed",
                            serde_json::json!({
                                "subsystem": "work_queue",
                                "reason": "no_new_signal_after_result_brief",
                                "work_item_id": active.id,
                                "result_brief_id": result_brief_id
                            }),
                        ))?;
                    }
                    self.consume_work_item_rechecks(&due_rechecks).await?;
                    return Ok(false);
                }
                let shadow_comparison = scheduler::shadow_comparison_for_work_queue_tick(
                    &scheduler_projection,
                    &active,
                    "continue_active",
                    &decision,
                    scheduler::SchedulerBoundary::IdleTick,
                );
                self.emit_system_tick_from_work_queue(
                    &active,
                    "continue_active",
                    shadow_comparison,
                    Some(&decision),
                )
                .await?;
                self.consume_work_item_rechecks(&due_rechecks).await?;
                Ok(true)
            }
            Some(IdleTickTrigger::WorkQueueQueued(queued)) => {
                let duplicate = self
                    .duplicate_queued_available_message_id(&queued)?
                    .map(scheduler::SchedulerDuplicateEvidence::QueuedAvailableMessage);
                let decision = scheduler::decide_next_action(
                    &scheduler_projection,
                    scheduler::SchedulerBoundary::IdleTick,
                    scheduler::SchedulerInput::IdleSignal(
                        scheduler::SchedulerIdleSignal::QueuedAvailable {
                            work_item: &queued,
                            duplicate: duplicate.clone(),
                        },
                    ),
                );
                if !matches!(
                    decision.kind,
                    scheduler::SchedulerDecisionKind::EmitSystemTick
                ) {
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                    if let Some(scheduler::SchedulerDuplicateEvidence::QueuedAvailableMessage(
                        message_id,
                    )) = duplicate
                    {
                        self.inner.storage.append_event(&AuditEvent::legacy(
                            "system_tick_suppressed",
                            serde_json::json!({
                                "subsystem": "work_queue",
                                "reason": "no_new_signal_after_queued_available",
                                "work_item_id": queued.id,
                                "message_id": message_id
                            }),
                        ))?;
                    }
                    self.consume_work_item_rechecks(&due_rechecks).await?;
                    return Ok(false);
                }
                let shadow_comparison = scheduler::shadow_comparison_for_work_queue_tick(
                    &scheduler_projection,
                    &queued,
                    "queued_available",
                    &decision,
                    scheduler::SchedulerBoundary::IdleTick,
                );
                self.emit_system_tick_from_work_queue(
                    &queued,
                    "queued_available",
                    shadow_comparison,
                    Some(&decision),
                )
                .await?;
                self.consume_work_item_rechecks(&due_rechecks).await?;
                Ok(true)
            }
            Some(IdleTickTrigger::WakeHint(pending)) => {
                let duplicate = self
                    .duplicate_wake_hint_message_id(&pending)?
                    .map(scheduler::SchedulerDuplicateEvidence::WakeHintMessage);
                let decision = scheduler::decide_next_action(
                    &scheduler_projection,
                    scheduler::SchedulerBoundary::IdleTick,
                    scheduler::SchedulerInput::IdleSignal(
                        scheduler::SchedulerIdleSignal::WakeHint {
                            pending: &pending,
                            duplicate,
                        },
                    ),
                );
                if !matches!(
                    decision.kind,
                    scheduler::SchedulerDecisionKind::EmitSystemTick
                ) {
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                    let mut guard = self.inner.agent.lock().await;
                    if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                        guard.state.pending_wake_hint = None;
                        guard.persist_state(&self.inner.storage)?;
                    }
                    drop(guard);
                    self.consume_work_item_rechecks(&due_rechecks).await?;
                    return Ok(false);
                }
                scheduler::append_scheduler_decision(
                    &self.inner.storage,
                    &self.inner.default_agent_id,
                    &decision,
                )?;
                self.emit_system_tick_from_wake_hint(&pending).await?;

                #[cfg(test)]
                if crate::runtime::test_util::checkpoint_matches_agent(&self.agent_id().await?) {
                    crate::runtime::test_util::wait_at_checkpoint().await;
                }

                let mut guard = self.inner.agent.lock().await;
                if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                    guard.state.pending_wake_hint = None;
                    guard.persist_state(&self.inner.storage)?;
                }
                drop(guard);
                self.consume_work_item_rechecks(&due_rechecks).await?;
                Ok(true)
            }
            Some(IdleTickTrigger::BlockedRecheck(items)) => {
                self.emit_system_tick_from_blocked_recheck(&items).await?;
                for item in items {
                    let _ = self.consume_work_item_recheck(&item.id).await?;
                }
                Ok(true)
            }
            None => {
                if let Some(decision) =
                    scheduler::wait_decision_for_projection(&scheduler_projection).map(|decision| {
                        decision.boundary(scheduler::SchedulerBoundary::IdleTick.as_str())
                    })
                {
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                }
                Ok(false)
            }
        }
    }

    async fn consume_due_work_item_rechecks(&self, agent_id: &str) -> Result<()> {
        let due_rechecks = self
            .inner
            .storage
            .due_blocked_work_item_rechecks(agent_id, self.now())?;
        self.consume_work_item_rechecks(&due_rechecks).await
    }

    async fn consume_work_item_rechecks(
        &self,
        work_items: &[crate::types::WorkItemRecord],
    ) -> Result<()> {
        if work_items.is_empty() {
            return Ok(());
        }
        for item in work_items {
            let _ = self.consume_work_item_recheck(&item.id).await?;
        }
        Ok(())
    }

    pub(super) async fn next_blocked_work_item_recheck_at(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let agent_id = self.agent_id().await?;
        self.inner
            .storage
            .next_blocked_work_item_recheck_at(&agent_id)
    }

    fn duplicate_queued_available_message_id(
        &self,
        work_item: &crate::types::WorkItemRecord,
    ) -> Result<Option<String>> {
        if let Some(message_id) = self.duplicate_work_queue_tick_message_id(
            &scheduler::work_queue_tick_idempotency_key(work_item, "queued_available"),
        )? {
            return Ok(Some(message_id));
        }
        let recent_messages = self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?;
        let Some(message) = recent_messages
            .iter()
            .filter(|message| {
                is_runtime_work_queue_message_for_work_item(
                    message,
                    &work_item.id,
                    "queued_available",
                )
            })
            .max_by_key(|message| message.created_at)
        else {
            return Ok(None);
        };

        if self.has_work_signal_after(work_item, message.created_at, "queued_available")? {
            return Ok(None);
        }

        Ok(Some(message.id.clone()))
    }

    fn duplicate_continue_active_result_brief_id(
        &self,
        work_item: &crate::types::WorkItemRecord,
    ) -> Result<Option<String>> {
        if let Some(message_id) = self.duplicate_work_queue_tick_message_id(
            &scheduler::work_queue_tick_idempotency_key(work_item, "continue_active"),
        )? {
            return Ok(Some(message_id));
        }
        let recent_briefs = self
            .inner
            .storage
            .read_recent_briefs(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?;
        let Some(result_brief) =
            latest_nonempty_result_brief_for_work_item(&recent_briefs, &work_item.id)
        else {
            return Ok(None);
        };

        if self.has_work_signal_after(work_item, result_brief.created_at, "continue_active")? {
            return Ok(None);
        }

        Ok(Some(result_brief.id.clone()))
    }

    fn duplicate_work_queue_tick_message_id(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .into_iter()
            .rev()
            .filter(|message| {
                matches!(
                    (&message.kind, &message.origin),
                    (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                        if subsystem == "work_queue"
                )
            })
            .find(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("work_queue"))
                    .and_then(|metadata| metadata.get("idempotency_key"))
                    .and_then(|value| value.as_str())
                    == Some(idempotency_key)
            })
            .map(|message| message.id))
    }

    fn duplicate_wake_hint_message_id(&self, pending: &PendingWakeHint) -> Result<Option<String>> {
        let idempotency_key = scheduler::wake_hint_idempotency_key(pending);
        Ok(self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .into_iter()
            .rev()
            .filter(|message| {
                matches!(
                    (&message.kind, &message.origin),
                    (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                        if subsystem == "wake_hint"
                )
            })
            .find(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("wake_hint"))
                    .and_then(|metadata| metadata.get("idempotency_key"))
                    .and_then(|value| value.as_str())
                    == Some(idempotency_key.as_str())
            })
            .map(|message| message.id))
    }

    pub(super) async fn emit_system_tick_from_wake_hint_with_decision(
        &self,
        pending: &PendingWakeHint,
    ) -> Result<bool> {
        let (snapshot, queue_len) = {
            let guard = self.inner.agent.lock().await;
            (
                scheduler::SchedulerAgentSnapshot::from_state(&guard.state),
                guard.queue.len(),
            )
        };
        let projection = scheduler::SchedulerProjection::from_snapshot_with_queue_len_at(
            &self.inner.storage,
            &snapshot,
            queue_len,
            self.now(),
        )?;
        let duplicate = self
            .duplicate_wake_hint_message_id(pending)?
            .map(scheduler::SchedulerDuplicateEvidence::WakeHintMessage);
        let decision = scheduler::decide_next_action(
            &projection,
            scheduler::SchedulerBoundary::IdleTick,
            scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::WakeHint {
                pending,
                duplicate,
            }),
        );
        scheduler::append_scheduler_decision(
            &self.inner.storage,
            &self.inner.default_agent_id,
            &decision,
        )?;
        if !matches!(
            decision.kind,
            scheduler::SchedulerDecisionKind::EmitSystemTick
        ) {
            return Ok(false);
        }
        self.emit_system_tick_from_wake_hint(pending).await?;
        Ok(true)
    }

    fn has_work_signal_after(
        &self,
        work_item: &crate::types::WorkItemRecord,
        anchor: chrono::DateTime<chrono::Utc>,
        ignored_runtime_reason: &str,
    ) -> Result<bool> {
        let work_item_id = work_item.id.as_str();

        if work_item.updated_at > anchor {
            return Ok(true);
        }

        if work_item
            .todo_list
            .iter()
            .any(|item| item.state != TodoItemState::Completed)
        {
            return Ok(true);
        }

        if self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|message| {
                message.created_at > anchor
                    && !is_runtime_work_queue_message_for_work_item(
                        message,
                        work_item_id,
                        ignored_runtime_reason,
                    )
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
                tool.created_at > anchor
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
                task.updated_at > anchor
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
            .read_recent_events(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|event| event.kind == "runtime_error" && event.created_at > anchor)
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
        let work_item_id = self
            .wake_hint_work_item_id(pending.external_trigger_id.as_deref())
            .await?;
        let idempotency_key = scheduler::wake_hint_idempotency_key(pending);
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wake_hint".into(),
            },
            AuthorityClass::RuntimeInstruction,
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
                "idempotency_key": idempotency_key,
                "reason": pending.reason,
                "description": pending.description,
                "source": pending.source,
                "scope": pending.scope,
                "external_trigger_id": pending.external_trigger_id,
                "work_item_id": work_item_id,
                "resource": pending.resource,
                "body": pending.body,
                "content_type": pending.content_type,
                "correlation_id": correlation_id,
                "causation_id": causation_id,
                "created_at": pending.created_at
            }
        }));
        message.work_item_id = work_item_id;
        message.correlation_id = correlation_id;
        message.causation_id = causation_id;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "wake_hint",
                "wake_hint": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("wake_hint"))
                    .cloned()
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        Ok(())
    }

    pub(super) async fn emit_system_tick_from_work_queue(
        &self,
        work_item: &crate::types::WorkItemRecord,
        reason: &str,
        shadow_comparison: Option<scheduler::SchedulerShadowComparison>,
        decision: Option<&scheduler::SchedulerDecision>,
    ) -> Result<()> {
        let idempotency_key = scheduler::work_queue_tick_idempotency_key(work_item, reason);
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: if reason == "queued_available" {
                    format!("Queued work item is available: {}", work_item.objective)
                } else {
                    format!("Continue current work item: {}", work_item.objective)
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
                "idempotency_key": idempotency_key,
                "work_item_id": work_item.id,
                "work_item_revision": work_item.revision,
                "objective": work_item.objective,
                "state": work_item.state,
                "runtime_switched_current_item": false
            }
        }));
        message.work_item_id = Some(work_item.id.clone());
        message.normalize_admission_fields();
        message.turn_id = normalized_turn_id(message.turn_id.as_deref());
        if message.turn_id.is_none() {
            message.turn_id = Some(crate::ids::turn_id());
        }
        let scheduler_shadow_comparison = shadow_comparison
            .map(super::scheduler_executor::scheduler_shadow_comparison_command)
            .transpose()?;
        let work_queue_metadata = message
            .metadata
            .as_ref()
            .and_then(|value| value.get("work_queue"))
            .cloned();
        let mut audit_events = decision
            .map(scheduler::scheduler_decision_event)
            .into_iter()
            .collect::<Vec<_>>();
        audit_events.extend([
            AuditEvent::legacy(
                "system_tick_emitted",
                serde_json::json!({
                    "subsystem": "work_queue",
                    "work_queue": work_queue_metadata
                }),
            ),
            AuditEvent::legacy(
                "message_admitted",
                serde_json::json!({
                    "message_id": message.id.clone(),
                    "agent_id": message.agent_id.clone(),
                    "kind": message.kind.clone(),
                    "origin": message.origin.clone(),
                    "authority_class": message.authority_class,
                    "delivery_surface": message.delivery_surface,
                    "admission_context": message.admission_context,
                    "trigger_kind": message.trigger_kind,
                    "work_item_id": message.work_item_id.clone(),
                    "task_id": message.task_id.clone(),
                    "source_refs": message.source_refs.clone(),
                    "correlation_id": message.correlation_id.clone(),
                    "causation_id": message.causation_id.clone(),
                }),
            ),
            AuditEvent::typed(
                RuntimeEventKind::MessageEnqueued,
                &MessageLifecycleAuditEvent::from_message(&message),
            )?,
        ]);
        let mut commit = {
            let mut guard = self.inner.agent.lock().await;
            let expected_persisted_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            let previous_status = committed_state.status.clone();
            let previous_sleeping_until = committed_state.sleeping_until;
            committed_state.pending = guard.queue.len().saturating_add(1);
            committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
            committed_state.total_message_count =
                self.inner.storage.count_messages()?.saturating_add(1);
            if scheduler::apply_message_wake_projection(&mut committed_state) {
                audit_events.push(AuditEvent::legacy(
                    "scheduler_posture_decision",
                    serde_json::json!({
                        "boundary": "message_admission",
                        "reason": "message_admission_wake",
                        "previous_status": previous_status,
                        "next_status": committed_state.status,
                        "evidence": [
                            format!("message_id={}", message.id),
                            format!("message_kind={:?}", message.kind),
                            format!("previous_sleeping_until={previous_sleeping_until:?}"),
                        ],
                    }),
                ));
            }
            let queue_record = QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: Utc::now(),
            };
            let mut commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Admit,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(queue_record),
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_persisted_state)),
                        record: Box::new(committed_state.clone()),
                    }),
                    message_evidence: vec![message.clone()],
                    transcript_entries: Vec::new(),
                    audit_events,
                    scheduler_shadow_comparison,
                    scheduler_delivery_shadow_comparison: None,
                    scheduler_semantic_shadow: None,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
            if !commit.applied {
                return Err(anyhow!(
                    "work queue tick admission made no durable progress"
                ));
            }
            guard.queue.push(message);
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            commit.effects.agent_state = None;
            commit
        };
        commit.effects.notify_scheduler = true;
        self.apply_transition_commit(commit).await;
        Ok(())
    }

    pub(super) async fn emit_system_tick_from_blocked_recheck(
        &self,
        work_items: &[crate::types::WorkItemRecord],
    ) -> Result<()> {
        let items = work_items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "work_item_id": item.id,
                    "work_item_revision": item.revision,
                    "objective": item.objective,
                    "blocked_by": item.blocked_by,
                    "recheck_at": item.recheck_at
                })
            })
            .collect::<Vec<_>>();
        let idempotency_key = work_items
            .iter()
            .filter_map(|item| item.recheck_at.map(|recheck_at| (item, recheck_at)))
            .map(|(item, recheck_at)| format!("{}@{}", item.id, recheck_at.to_rfc3339()))
            .collect::<Vec<_>>()
            .join(",");
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_item_recheck".into()
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Background,
            MessageBody::Text {
                text: format!(
                    "{} blocked WorkItem recheck{} due; inspect blockers and refresh or clear blocked_by.",
                    work_items.len(),
                    if work_items.len() == 1 { " is" } else { "s are" }
                )
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(serde_json::json!({
            "work_item_recheck": {
                "idempotency_key": idempotency_key,
                "count": work_items.len(),
                "items": items
            }
        }));
        self.inner.storage.append_event(&AuditEvent::legacy(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "work_item_recheck",
                "work_item_recheck": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_item_recheck"))
                    .cloned()
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        Ok(())
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
                    "wait_policy": task.wait_policy()
                })
            })
            .collect::<Vec<_>>();
        let mut message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "task_restart".into(),
            },
            AuthorityClass::RuntimeInstruction,
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
                "items": items
            }
        }));
        self.inner.storage.append_event(&AuditEvent::legacy(
            "system_tick_emitted",
            serde_json::json!({
                "subsystem": "task_restart",
                "interrupted_tasks": message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("interrupted_tasks"))
                    .cloned()
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

fn is_runtime_work_queue_message_for_work_item(
    message: &MessageEnvelope,
    work_item_id: &str,
    reason: &str,
) -> bool {
    matches!(
        (&message.kind, &message.origin),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) if subsystem == "work_queue"
    ) && message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("work_queue"))
        .is_some_and(|metadata| {
            metadata.get("reason").and_then(|value| value.as_str()) == Some(reason)
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
    use crate::types::{AgentStatus, TodoItem, TodoItemState, WorkItemRecord, WorkItemState};
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

    fn enable_scheduler_shadow_scenario(test_runtime: &TestRuntime, scenario_class: &str) {
        let connection = test_runtime.runtime.inner.runtime_db.connection().unwrap();
        connection
            .execute(
                "UPDATE scheduler_protocol_config
                 SET protocol_mode = 'shadow',
                     config_revision = config_revision + 1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE config_id = 1",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO scheduler_scenario_authorities (
                   scenario_class, mode, rollback_target,
                   manifest_revision, preflight_revision, updated_at
                 ) VALUES (?1, 'shadow', 'off', NULL, NULL, CURRENT_TIMESTAMP)",
                [scenario_class],
            )
            .unwrap();
    }

    fn wait_for_audit_event(test_runtime: &TestRuntime, kind: &str, label: &str) -> AuditEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let events = test_runtime
                .runtime
                .inner
                .storage
                .read_recent_events(20)
                .unwrap();
            if let Some(event) = events.iter().find(|event| event.kind == kind) {
                return event.clone();
            }
            assert!(std::time::Instant::now() < deadline, "{label}");
            std::thread::sleep(std::time::Duration::from_millis(10));
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
        guard
            .persist_state(&test_runtime.runtime.inner.storage)
            .unwrap();
    }

    fn add_queued_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let mut record = WorkItemRecord::new("default", target, WorkItemState::Open);
        record.id = id.to_string();
        persist_test_work_item(test_runtime, &record);
        record
    }

    fn persist_test_work_item(test_runtime: &TestRuntime, record: &WorkItemRecord) {
        let repository = test_runtime.runtime.inner.runtime_db.work_items();
        if let Some(existing) = repository.latest(&record.id).unwrap() {
            repository
                .update_expected(record, existing.revision)
                .unwrap();
        } else {
            repository.insert_new(record).unwrap();
        }
    }

    fn add_current_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let mut record = WorkItemRecord::new("default", target, WorkItemState::Open);
        record.id = id.to_string();
        persist_test_work_item(test_runtime, &record);
        let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
        guard.state.current_work_item_id = Some(record.id.clone());
        guard
            .persist_state(&test_runtime.runtime.inner.storage)
            .unwrap();
        record
    }

    fn block_work_item(
        test_runtime: &TestRuntime,
        record: &WorkItemRecord,
        blocked_by: &str,
    ) -> WorkItemRecord {
        let mut updated = record.clone();
        updated.revision += 1;
        updated.blocked_by = Some(blocked_by.to_string());
        updated.updated_at = chrono::Utc::now();
        persist_test_work_item(test_runtime, &updated);
        updated
    }

    fn block_work_item_with_due_recheck(
        test_runtime: &TestRuntime,
        record: &WorkItemRecord,
        blocked_by: &str,
    ) -> WorkItemRecord {
        let mut updated = record.clone();
        updated.revision += 1;
        updated.blocked_by = Some(blocked_by.to_string());
        updated.recheck_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
        updated.recheck_consumed_at = None;
        updated.updated_at = chrono::Utc::now();
        persist_test_work_item(test_runtime, &updated);
        updated
    }

    fn latest_work_item(test_runtime: &TestRuntime, id: &str) -> WorkItemRecord {
        test_runtime
            .runtime
            .inner
            .runtime_db
            .work_items()
            .latest(id)
            .unwrap()
            .expect("work item should exist")
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
            description: None,
            scope: None,
            external_trigger_id: None,
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
            AuthorityClass::OperatorInstruction,
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
    fn queue_nonempty_consumes_due_blocked_recheck_without_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued input".to_string(),
            },
        );
        {
            let mut guard = test_runtime.runtime.inner.agent.blocking_lock();
            guard.queue.push(message);
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "queued input should suppress system tick emission"
        );
        assert!(get_emitted_system_ticks(&test_runtime).is_empty());

        let latest = latest_work_item(&test_runtime, "wi-blocked");
        assert!(
            latest
                .recheck_consumed_at
                .zip(latest.recheck_at)
                .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
            "due recheck should be consumed while queued input will wake the agent"
        );
    }

    #[test]
    fn running_agent_consumes_due_blocked_recheck_without_tick() {
        let test_runtime = test_runtime();
        set_agent_status(&test_runtime, AgentStatus::AwakeRunning);

        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(!emitted, "running agent should not receive a recheck tick");
        assert!(get_emitted_system_ticks(&test_runtime).is_empty());

        let latest = latest_work_item(&test_runtime, "wi-blocked");
        assert!(
            latest
                .recheck_consumed_at
                .zip(latest.recheck_at)
                .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
            "due recheck should be consumed while the active turn can inspect work state"
        );
    }

    #[test]
    fn idle_agent_emits_due_blocked_recheck_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "idle agent should still receive due recheck tick");
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_item_recheck");
        assert_eq!(ticks[0].1["count"].as_u64(), Some(1));
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
    fn pending_wake_hint_consumes_due_blocked_recheck_without_recheck_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        set_wake_hint(&test_runtime, "wake-test");
        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for wake");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "wake hint should still emit a system tick");
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(
            ticks[0].0, "wake_hint",
            "wake hint should provide the execution opportunity"
        );

        let latest = latest_work_item(&test_runtime, "wi-blocked");
        assert!(
            latest
                .recheck_consumed_at
                .zip(latest.recheck_at)
                .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
            "due recheck should be consumed because wake hint already woke the agent"
        );
    }

    #[test]
    fn current_work_item_takes_precedence_over_queued_notification() {
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
    fn blocked_current_work_item_does_not_emit_continue_active_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        block_work_item(&test_runtime, &active, "Waiting for external PR metadata.");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "blocked current work must not idle-spin through continue_active"
        );
        assert!(
            get_emitted_system_ticks(&test_runtime).is_empty(),
            "blocked current work should wait for an external signal"
        );
    }

    #[test]
    fn blocked_current_work_item_notifies_queued_without_switching_current() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        block_work_item(&test_runtime, &active, "Waiting for external PR metadata.");
        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "unblocked queued work should be eligible while current work is blocked"
        );

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");
        assert_eq!(ticks[0].1["reason"].as_str(), Some("queued_available"));
        assert_eq!(ticks[0].1["work_item_id"].as_str(), Some("wi-queued"));

        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert_eq!(
            guard.state.current_work_item_id.as_deref(),
            Some("wi-active")
        );
    }

    #[test]
    fn queued_system_tick_does_not_mutate_current_work_item_id() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued_id = "wi-queued";
        add_queued_work_item(&test_runtime, queued_id, "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "queued work should emit a visible system tick");

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].1["reason"].as_str(), Some("queued_available"));
        assert_eq!(ticks[0].1["work_item_id"].as_str(), Some(queued_id));
        let messages = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_messages(10)
            .unwrap();
        let tick_message = messages
            .iter()
            .find(|message| {
                matches!(
                    message.origin,
                    MessageOrigin::System { ref subsystem } if subsystem == "work_queue"
                )
            })
            .expect("work queue tick message should be recorded");
        assert_eq!(tick_message.work_item_id.as_deref(), Some(queued_id));

        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(guard.state.current_work_item_id.is_none());
    }

    #[test]
    fn queued_system_tick_persists_shadow_comparison_with_admission_facts() {
        let test_runtime = test_runtime();
        enable_scheduler_shadow_scenario(&test_runtime, "work_item_autonomous_continuation");
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-shadow", "shadow-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(
            ticks[0].1["work_item_id"].as_str(),
            Some(queued.id.as_str())
        );
        let tick_message = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_messages(10)
            .unwrap()
            .into_iter()
            .find(|message| {
                is_runtime_work_queue_message_for_work_item(message, &queued.id, "queued_available")
            })
            .expect("work queue tick message should be durable");

        let queue_entries = test_runtime
            .runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest_all()
            .unwrap();
        assert_eq!(queue_entries.len(), 1);
        assert_eq!(queue_entries[0].message_id, tick_message.id);
        assert_eq!(queue_entries[0].status, QueueEntryStatus::Queued);

        let connection = test_runtime.runtime.inner.runtime_db.connection().unwrap();
        let (boundary, input_identity, outcome, authority_mode): (String, String, String, String) =
            connection
                .query_row(
                    "SELECT boundary, input_identity, comparison_outcome, authority_mode
                 FROM scheduler_shadow_comparisons
                 WHERE agent_id = 'default'
                   AND scenario_class = 'work_item_autonomous_continuation'
                   AND comparison_identity = ?1",
                    [format!(
                        "work_queue_idle_tick:work_queue:queued_available:{}:{}",
                        queued.id, queued.revision
                    )],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
        assert_eq!(boundary, "idle_tick");
        assert_eq!(
            input_identity,
            format!(
                "work_queue_tick:work_queue:queued_available:{}:{}",
                queued.id, queued.revision
            )
        );
        assert_eq!(outcome, "matched");
        assert_eq!(authority_mode, "shadow");

        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(20)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "system_tick_emitted"));
        assert!(events.iter().any(|event| event.kind == "message_admitted"));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::MessageEnqueued.descriptor().wire_name
        }));
    }

    #[test]
    fn queued_system_tick_fault_rolls_back_all_admission_facts() {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let test_runtime = test_runtime();
            enable_scheduler_shadow_scenario(&test_runtime, "work_item_autonomous_continuation");
            set_agent_idle(&test_runtime);
            add_queued_work_item(&test_runtime, "wi-shadow-fault", "shadow-target");
            let initial_state = test_runtime
                .runtime
                .inner
                .runtime_db
                .agent_states()
                .latest("default")
                .unwrap()
                .unwrap();
            let initial_message_count =
                test_runtime.runtime.inner.storage.count_messages().unwrap();
            test_runtime.runtime.inject_next_transition_fault(fault);

            let rt = tokio::runtime::Runtime::new().unwrap();
            let error = rt
                .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected runtime transition fault"),
                "unexpected error for {fault:?}: {error:#}"
            );

            assert_eq!(
                test_runtime.runtime.inner.storage.count_messages().unwrap(),
                initial_message_count
            );
            assert!(test_runtime
                .runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest_all()
                .unwrap()
                .is_empty());
            assert_eq!(
                test_runtime
                    .runtime
                    .inner
                    .runtime_db
                    .agent_states()
                    .latest("default")
                    .unwrap(),
                Some(initial_state.clone())
            );
            let comparison_count: i64 = test_runtime
                .runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM scheduler_shadow_comparisons",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(comparison_count, 0);
            assert!(get_emitted_system_ticks(&test_runtime).is_empty());
            let events = test_runtime
                .runtime
                .inner
                .storage
                .read_recent_events(20)
                .unwrap();
            assert!(!events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    "scheduler_decision"
                        | "system_tick_emitted"
                        | "message_admitted"
                        | "message_enqueued"
                )
            }));

            let guard = test_runtime.runtime.inner.agent.blocking_lock();
            assert!(guard.queue.is_empty());
            assert_eq!(guard.state, initial_state);
        }
    }

    #[test]
    fn queued_system_tick_is_suppressed_without_new_signal() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued_id = "wi-queued";
        add_queued_work_item(&test_runtime, queued_id, "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();
        assert!(emitted, "first queued notification should be emitted");

        clear_queue(&test_runtime);

        let emitted_again = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();
        assert!(
            !emitted_again,
            "queued notification should not repeat without a new signal"
        );

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);

        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(20)
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "system_tick_suppressed"
                && event.data["reason"] == "no_new_signal_after_queued_available"
                && event.data["work_item_id"] == queued_id
        }));
    }

    #[test]
    fn queued_system_tick_idempotency_key_includes_work_item_revision() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        clear_queue(&test_runtime);

        let first_ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(
            first_ticks[0].1["idempotency_key"].as_str(),
            Some("work_queue:queued_available:wi-queued:1")
        );

        let mut updated = queued.clone();
        updated.revision += 1;
        updated.updated_at = chrono::Utc::now();
        test_runtime
            .runtime
            .inner
            .storage
            .append_work_item(&updated)
            .unwrap();

        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 2);
        assert_eq!(
            ticks[1].1["idempotency_key"].as_str(),
            Some("work_queue:queued_available:wi-queued:2")
        );
    }

    #[test]
    fn queued_system_tick_explicit_idempotency_key_wins_over_newer_signals() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let idempotency_key =
            scheduler::work_queue_tick_idempotency_key(&queued, "queued_available");
        let mut existing_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "queued work item is available".into(),
            },
        );
        existing_tick.id = "existing-work-queue-tick".into();
        existing_tick.created_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        existing_tick.work_item_id = Some(queued.id.clone());
        existing_tick.metadata = Some(serde_json::json!({
            "work_queue": {
                "idempotency_key": idempotency_key,
                "reason": "queued_available",
                "work_item_id": queued.id,
                "work_item_revision": queued.revision
            }
        }));
        test_runtime
            .runtime
            .inner
            .storage
            .append_message(&existing_tick)
            .unwrap();

        let mut newer_operator_signal = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "newer operator signal".into(),
            },
        );
        newer_operator_signal.created_at = chrono::Utc::now();
        test_runtime
            .runtime
            .inner
            .storage
            .append_message(&newer_operator_signal)
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            !emitted,
            "same work-item revision must not emit another queued tick even when recent-ledger fallback would see a newer signal"
        );
        assert!(get_emitted_system_ticks(&test_runtime).is_empty());
        let decision = wait_for_audit_event(
            &test_runtime,
            "scheduler_decision",
            "duplicate decision should be recorded",
        );
        assert_eq!(decision.data["decision"].as_str(), Some("Noop"));
        assert_eq!(
            decision.data["reason"].as_str(),
            Some("duplicate_queued_available")
        );
        assert!(decision.data["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("message_id=existing-work-queue-tick")));
    }

    #[test]
    fn queued_system_tick_emits_no_pick_or_activation_events() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued_id = "wi-queued";
        add_queued_work_item(&test_runtime, queued_id, "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "queued item should emit a system tick");

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
            picked_events.is_empty(),
            "queued notification must not emit work_item_picked"
        );

        let activated_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "work_item_queue_activated")
            .collect();

        assert!(
            activated_events.is_empty(),
            "queued notification must not emit activation events"
        );
    }

    #[test]
    fn wake_hint_system_tick_preserves_metadata() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        // Set wake hint with full metadata
        let hint = PendingWakeHint {
            reason: "test-wake".to_string(),
            description: None,
            scope: None,
            external_trigger_id: None,
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
            "suppression must not enqueue a model-reentry system tick"
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
            AuthorityClass::OperatorInstruction,
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
    fn continue_active_preserved_when_todo_list_has_pending_item() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let mut active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        active.revision += 1;
        active.todo_list = vec![TodoItem {
            text: "finish remaining implementation".to_string(),
            state: TodoItemState::InProgress,
        }];
        active.updated_at = chrono::Utc::now();
        test_runtime
            .runtime
            .inner
            .storage
            .append_work_item(&active)
            .unwrap();
        append_result_brief_for_work_item(&test_runtime, &active.id, "Progress report.");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "an unfinished todo list should keep continue_active eligible"
        );
    }

    #[test]
    fn restart_ordering_notifies_queued_when_no_active() {
        let test_runtime = test_runtime();
        set_agent_status(&test_runtime, AgentStatus::Asleep);

        // Without wake hint or active item, queued item should be surfaced
        // without mutating current_work_item_id.
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
        assert_eq!(metadata["reason"].as_str().unwrap(), "queued_available");
        assert!(!metadata["runtime_switched_current_item"].as_bool().unwrap());

        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(guard.state.current_work_item_id.is_none());
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
