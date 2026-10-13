use super::*;
use crate::types::{BriefKind, TaskStatus, TodoItemState};

const CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT: usize = 512;
const QUEUED_AVAILABLE_NO_PROGRESS_LIMIT: usize = 5;

#[derive(Debug, Clone)]
enum QueuedAvailableRetryStatus {
    Ready,
    Backoff {
        message_id: String,
        attempts: usize,
        retry_at: chrono::DateTime<chrono::Utc>,
        episode_key: String,
    },
    Exhausted {
        message_id: String,
        attempts: usize,
        episode_key: String,
    },
}

struct QueuedAvailableRetryAttempt {
    message_id: String,
    attempts: usize,
    retry_at: chrono::DateTime<chrono::Utc>,
    episode_key: String,
}

#[derive(Debug, Clone)]
enum IdleTickTrigger {
    WorkQueueActive(crate::types::WorkItemRecord, Option<u64>),
    WorkQueueQueued(crate::types::WorkItemRecord, Option<u64>),
    BlockedRecheck(Vec<crate::types::WorkItemRecord>),
    WakeHint(PendingWakeHint),
}

fn idle_tick_trigger_from_state(
    pending_wake_hint: Option<PendingWakeHint>,
    work_reactivation: Option<(
        crate::types::WorkItemRecord,
        crate::types::WorkReactivationMode,
        Option<u64>,
    )>,
    due_rechecks: Vec<crate::types::WorkItemRecord>,
) -> Option<IdleTickTrigger> {
    if let Some(pending) = pending_wake_hint {
        Some(IdleTickTrigger::WakeHint(pending))
    } else {
        work_reactivation
            .map(|(work_item, mode, generation)| match mode {
                crate::types::WorkReactivationMode::ContinueActive => {
                    IdleTickTrigger::WorkQueueActive(work_item, generation)
                }
                crate::types::WorkReactivationMode::ActivateQueued => {
                    IdleTickTrigger::WorkQueueQueued(work_item, generation)
                }
            })
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
        let mut guard = self.begin_mutation().await?;
        guard.state.last_continuation = Some(continuation_resolution.clone());
        guard.persist_state(&self.inner.storage).await?;
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
                "task_terminal": trigger.task_result_outcome.is_some(),
                "task_result_outcome": trigger.task_result_outcome,
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

    #[inline(never)]
    pub(super) fn maybe_emit_pending_system_tick<'a>(
        &'a self,
        triggering_continuation: Option<&'a ContinuationResolution>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(self.maybe_emit_pending_system_tick_inner(triggering_continuation))
    }

    async fn maybe_emit_pending_system_tick_inner(
        &self,
        triggering_continuation: Option<&ContinuationResolution>,
    ) -> Result<bool> {
        scheduler_executor::SchedulerDecisionExecutor::new(self)
            .reconcile_stale_run_projection(
                scheduler_executor::StaleRunProjectionBoundary::PendingSystemTick,
            )
            .await?;
        let mut mutation = self.begin_mutation().await?;
        let eligible = matches!(
            mutation.state.status,
            AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep
        ) && mutation.queue.is_empty();
        let consume_rechecks =
            mutation.state.status == AgentStatus::AwakeRunning || !mutation.queue.is_empty();
        let agent_id = mutation.state.id.clone();
        if !eligible {
            drop(mutation);
            if consume_rechecks {
                self.consume_due_work_item_rechecks(&agent_id).await?;
            }
            return Ok(false);
        }
        let mut read = mutation
            .prepare_scheduler_read(None, Default::default(), Default::default(), self.now())
            .await?;
        let mut pending_wake_hint = read.state.pending_wake_hint.clone();
        // The hook can invoke runtime APIs. Only selected owned observations,
        // never the owner reservation, survive its await.
        drop(mutation);
        let mut scheduler_projection = read.projection;
        let mut due_rechecks = read.due_rechecks;
        let mut queued_retry_statuses =
            self.filter_suppressed_queued_available_work_items(&mut scheduler_projection)?;
        let async_hook = self
            .inner
            .autonomous_continuation_decision_hook
            .read()
            .await
            .clone();
        let used_async_hook = async_hook.is_some();
        let selection = if pending_wake_hint.is_some() {
            None
        } else if let Some(hook) = async_hook.as_deref() {
            scheduler::select_autonomous_continuation_with_async_hook(
                &scheduler_projection,
                Some(hook),
            )
            .await
        } else {
            scheduler::select_autonomous_continuation_with_hook(
                &scheduler_projection,
                Some(self.inner.autonomous_continuation_hook.as_ref()),
            )
        };
        if used_async_hook {
            let mut mutation = self.begin_mutation().await?;
            read = mutation
                .prepare_scheduler_read(None, Default::default(), Default::default(), self.now())
                .await?;
            drop(mutation);
            pending_wake_hint = read.state.pending_wake_hint.clone();
            scheduler_projection = read.projection;
            due_rechecks = read.due_rechecks;
            queued_retry_statuses =
                self.filter_suppressed_queued_available_work_items(&mut scheduler_projection)?;
        }
        if let Some(selection) = selection.as_ref() {
            if let Some(reason) = selection.fallback_reason() {
                self.inner.storage.append_event(&AuditEvent::legacy(
                    "autonomous_continuation_selection_fallback",
                    serde_json::json!({
                        "agent_id": self.agent_id().await?,
                        "reason": reason.as_str(),
                        "candidate_count": selection.candidate_count(),
                    }),
                ))?;
            }
        }
        let work_reactivation = selection
            .as_ref()
            .and_then(|selection| {
                scheduler::resolve_autonomous_continuation_work_item(
                    &scheduler_projection,
                    selection,
                )
            })
            .map(|(work_item, mode, generation)| (work_item.clone(), mode, generation));
        let trigger = idle_tick_trigger_from_state(
            pending_wake_hint,
            work_reactivation,
            due_rechecks.clone(),
        );

        let suppress_continue_active = triggering_continuation
            .is_some_and(|continuation| continuation.model_reentry)
            || self.take_continue_active_suppression().await;

        match trigger {
            Some(IdleTickTrigger::WorkQueueActive(active, generation)) => {
                let has_due_recheck = due_rechecks
                    .iter()
                    .any(|work_item| work_item.id == active.id);
                let duplicate = if has_due_recheck {
                    None
                } else {
                    self.duplicate_continue_active_result_brief_id(&active, generation)?
                        .map(scheduler::SchedulerDuplicateEvidence::ContinueActiveBrief)
                };
                let decision = scheduler::decide_next_action(
                    &scheduler_projection,
                    scheduler::SchedulerBoundary::IdleTick,
                    scheduler::SchedulerInput::IdleSignal(
                        scheduler::SchedulerIdleSignal::ContinueActive {
                            work_item: &active,
                            work_item_generation: generation,
                            suppressed_after_model_reentry_continuation: suppress_continue_active
                                && !has_due_recheck,
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
                self.emit_system_tick_from_work_queue(
                    &active,
                    generation,
                    "continue_active",
                    Some(&decision),
                )
                .await?;
                self.consume_work_item_rechecks(&due_rechecks).await?;
                Ok(true)
            }
            Some(IdleTickTrigger::WorkQueueQueued(queued, generation)) => {
                let retry_status = queued_retry_statuses
                    .remove(&queued.id)
                    .unwrap_or(QueuedAvailableRetryStatus::Ready);
                let duplicate = match &retry_status {
                    QueuedAvailableRetryStatus::Ready => None,
                    QueuedAvailableRetryStatus::Backoff { message_id, .. }
                    | QueuedAvailableRetryStatus::Exhausted { message_id, .. } => {
                        Some(message_id.clone())
                    }
                }
                .map(scheduler::SchedulerDuplicateEvidence::QueuedAvailableMessage);
                let decision = scheduler::decide_next_action(
                    &scheduler_projection,
                    scheduler::SchedulerBoundary::IdleTick,
                    scheduler::SchedulerInput::IdleSignal(
                        scheduler::SchedulerIdleSignal::QueuedAvailable {
                            work_item: &queued,
                            work_item_generation: generation,
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
                    self.consume_work_item_rechecks(&due_rechecks).await?;
                    return Ok(false);
                }
                self.emit_system_tick_from_work_queue(
                    &queued,
                    generation,
                    "queued_available",
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
                    let mut guard = self.begin_mutation().await?;
                    if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                        guard.state.pending_wake_hint = None;
                        guard.persist_state(&self.inner.storage).await?;
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
                self.emit_system_tick_from_wake_hint(&pending, decision.work_item_id.as_deref())
                    .await?;

                #[cfg(test)]
                if crate::runtime::test_util::checkpoint_matches_agent(&self.agent_id().await?) {
                    crate::runtime::test_util::wait_at_checkpoint().await;
                }

                let mut guard = self.begin_mutation().await?;
                if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                    guard.state.pending_wake_hint = None;
                    guard.persist_state(&self.inner.storage).await?;
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
        let due_rechecks = {
            let observation = self
                .capture_work_read(&super::work_reads::live_work_request())
                .await?;
            anyhow::ensure!(
                observation.state.id == agent_id,
                "Work recheck requested for another agent"
            );
            observation
                .work
                .due_work_rechecks(self.now())?
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
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

    pub(super) fn next_queued_available_retry_at(
        &self,
        work_items: &[crate::types::WorkItemRecord],
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        if work_items.is_empty() {
            return Ok(None);
        }
        let recent_messages = self
            .inner
            .storage
            .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?;
        let mut next_retry_at: Option<chrono::DateTime<chrono::Utc>> = None;
        for work_item in work_items {
            if let Some(attempt) = Self::queued_available_retry_attempt(work_item, &recent_messages)
                .filter(|attempt| attempt.attempts < QUEUED_AVAILABLE_NO_PROGRESS_LIMIT)
            {
                next_retry_at = Some(
                    next_retry_at.map_or(attempt.retry_at, |current| current.min(attempt.retry_at)),
                );
            }
        }
        Ok(next_retry_at)
    }

    pub(super) async fn emit_due_agent_wait_recheck(&self) -> Result<bool> {
        let now = self.now();
        let (agent_id, due) = {
            let request = super::work_reads::live_work_request();
            let mut mutation = self.begin_mutation().await?;
            mutation.prepare_owner_work_request(&request).await?;
            let observation = mutation.capture_work_read(&request)?;
            let due = observation.work.due_agent_wait_recheck(now)?.cloned();
            (observation.state.id.clone(), due)
        };
        let Some(condition) = due else {
            return Ok(false);
        };
        let recheck_at = condition
            .recheck_at()
            .expect("due agent wait must have a recheck deadline");
        let recheck_at_key = recheck_at.to_rfc3339();
        let wake = condition
            .continuation
            .as_ref()
            .and_then(|value| value.get("wake"))
            .cloned();
        let resource = condition
            .continuation
            .as_ref()
            .and_then(|value| value.get("resource"))
            .cloned();
        let mut message = MessageEnvelope::new(
            agent_id.clone(),
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wait_condition_recheck".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: format!(
                    "WaitFor recheck deadline reached for wait {}.",
                    condition.id
                ),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.id = crate::ids::wait_recheck_message_id(&condition.id, &recheck_at_key);
        message
            .source_refs
            .insert("wait_id".into(), condition.id.clone());
        message.metadata = Some(serde_json::json!({
            "wait_condition_recheck": {
                "wait_id": condition.id,
                "recheck_at": recheck_at,
                "wake": wake,
                "resource": resource,
                "waiting_for": condition.waiting_for,
            }
        }));
        self.inner.storage.append_event(&AuditEvent::legacy(
            "wait_condition_recheck_due",
            serde_json::json!({
                "agent_id": agent_id,
                "wait_id": message.source_refs.get("wait_id"),
                "message_id": message.id,
                "recheck_at": recheck_at,
            }),
        ))?;
        let _ = self.enqueue(message).await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "wait_condition_recheck_enqueued",
            serde_json::json!({
                "agent_id": agent_id,
                "wait_id": condition.id,
                "message_id": crate::ids::wait_recheck_message_id(
                    &condition.id,
                    &recheck_at_key,
                ),
                "recheck_at": recheck_at,
            }),
        ))?;
        Ok(true)
    }

    fn filter_suppressed_queued_available_work_items(
        &self,
        projection: &mut scheduler::SchedulerProjection,
    ) -> Result<std::collections::HashMap<String, QueuedAvailableRetryStatus>> {
        let now = self.now();
        let recent_messages = if projection.queued_runnable_work_items.is_empty() {
            Vec::new()
        } else {
            self.inner
                .storage
                .read_recent_messages(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
        };
        let mut retry_statuses =
            std::collections::HashMap::with_capacity(projection.queued_runnable_work_items.len());
        for work_item in &projection.queued_runnable_work_items {
            let status = Self::queued_available_retry_status(work_item, &now, &recent_messages);
            match &status {
                QueuedAvailableRetryStatus::Backoff {
                    attempts,
                    retry_at,
                    episode_key,
                    message_id,
                } => self.append_queued_available_retry_event(
                    "work_queue_redispatch_backoff_scheduled",
                    episode_key,
                    Some(*attempts),
                    serde_json::json!({
                        "subsystem": "work_queue",
                        "work_item_id": work_item.id,
                        "work_item_revision": work_item.revision,
                        "no_progress_attempts": attempts,
                        "retry_at": retry_at.to_rfc3339(),
                        "message_id": message_id
                    }),
                )?,
                QueuedAvailableRetryStatus::Exhausted {
                    attempts,
                    episode_key,
                    message_id,
                } => self.append_queued_available_retry_event(
                    "work_queue_redispatch_stalled",
                    episode_key,
                    None,
                    serde_json::json!({
                        "subsystem": "work_queue",
                        "work_item_id": work_item.id,
                        "work_item_revision": work_item.revision,
                        "no_progress_attempts": attempts,
                        "awaiting": "new_external_signal_or_work_item_update",
                        "message_id": message_id
                    }),
                )?,
                QueuedAvailableRetryStatus::Ready => {}
            }
            retry_statuses.insert(work_item.id.clone(), status);
        }
        projection.queued_runnable_work_items.retain(|work_item| {
            retry_statuses
                .get(&work_item.id)
                .is_some_and(|status| matches!(status, QueuedAvailableRetryStatus::Ready))
        });
        Ok(retry_statuses)
    }

    fn queued_available_retry_status(
        work_item: &crate::types::WorkItemRecord,
        now: &chrono::DateTime<chrono::Utc>,
        recent_messages: &[MessageEnvelope],
    ) -> QueuedAvailableRetryStatus {
        let Some(attempt) = Self::queued_available_retry_attempt(work_item, recent_messages) else {
            return QueuedAvailableRetryStatus::Ready;
        };
        if attempt.attempts >= QUEUED_AVAILABLE_NO_PROGRESS_LIMIT {
            return QueuedAvailableRetryStatus::Exhausted {
                message_id: attempt.message_id,
                attempts: attempt.attempts,
                episode_key: attempt.episode_key,
            };
        }
        if now < &attempt.retry_at {
            return QueuedAvailableRetryStatus::Backoff {
                message_id: attempt.message_id,
                attempts: attempt.attempts,
                retry_at: attempt.retry_at,
                episode_key: attempt.episode_key,
            };
        }
        QueuedAvailableRetryStatus::Ready
    }

    fn queued_available_retry_attempt(
        work_item: &crate::types::WorkItemRecord,
        recent_messages: &[MessageEnvelope],
    ) -> Option<QueuedAvailableRetryAttempt> {
        let latest_external_signal = recent_messages
            .iter()
            .filter(|message| is_queued_available_retry_signal(message))
            .max_by_key(|message| message.created_at);
        let episode_key = format!(
            "{}:{}:{}",
            work_item.id,
            work_item.revision,
            latest_external_signal.map_or("initial", |message| message.id.as_str())
        );
        let latest_signal_at = latest_external_signal.map(|message| message.created_at);
        let mut attempts = recent_messages
            .iter()
            .filter(|message| {
                is_runtime_work_queue_message_for_work_item(
                    message,
                    &work_item.id,
                    "queued_available",
                ) && message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("work_queue"))
                    .and_then(|metadata| metadata.get("work_item_revision"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(work_item.revision)
                    && latest_signal_at.is_none_or(|signal_at| message.created_at > signal_at)
            })
            .collect::<Vec<_>>();
        attempts.sort_by_key(|message| message.created_at);
        let Some(last_tick) = attempts.last() else {
            return None;
        };
        let attempt_count = attempts.len();
        let retry_delay = match attempt_count {
            1 => chrono::Duration::minutes(1),
            2 => chrono::Duration::minutes(5),
            _ => chrono::Duration::minutes(30),
        };
        let retry_at = last_tick.created_at + retry_delay;
        Some(QueuedAvailableRetryAttempt {
            message_id: last_tick.id.clone(),
            attempts: attempt_count,
            retry_at,
            episode_key,
        })
    }

    fn append_queued_available_retry_event(
        &self,
        kind: &str,
        episode_key: &str,
        attempt_count: Option<usize>,
        data: serde_json::Value,
    ) -> Result<()> {
        let already_recorded = self
            .inner
            .storage
            .read_recent_events(CONTINUE_ACTIVE_SIGNAL_SCAN_LIMIT)?
            .iter()
            .any(|event| {
                event.kind == kind
                    && event
                        .data
                        .get("episode_key")
                        .and_then(serde_json::Value::as_str)
                        == Some(episode_key)
                    && attempt_count.is_none_or(|attempt_count| {
                        event
                            .data
                            .get("no_progress_attempts")
                            .and_then(serde_json::Value::as_u64)
                            == Some(attempt_count as u64)
                    })
            });
        if already_recorded {
            return Ok(());
        }
        let mut data = data;
        data["episode_key"] = serde_json::Value::String(episode_key.to_string());
        self.inner
            .storage
            .append_event(&AuditEvent::legacy(kind, data))?;
        Ok(())
    }

    fn duplicate_continue_active_result_brief_id(
        &self,
        work_item: &crate::types::WorkItemRecord,
        generation: Option<u64>,
    ) -> Result<Option<String>> {
        if let Some((message_id, message_created_at)) = self.duplicate_work_queue_tick_message_id(
            &scheduler::work_queue_tick_idempotency_key(work_item, "continue_active", generation),
        )? {
            // Same rationale as duplicate_queued_available_message_id: a provider
            // failure during the active continuation turn must not permanently
            // suppress the retry tick when the WorkItem still has pending work.
            if self.has_work_signal_after(work_item, message_created_at, "continue_active")? {
                return Ok(None);
            }
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
    ) -> Result<Option<(String, chrono::DateTime<chrono::Utc>)>> {
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
            .map(|message| (message.id, message.created_at)))
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
        let mut mutation = self.begin_mutation().await?;
        let read = mutation
            .prepare_scheduler_read(None, Default::default(), Default::default(), self.now())
            .await?;
        drop(mutation);
        let projection = &read.projection;
        let duplicate = self
            .duplicate_wake_hint_message_id(pending)?
            .map(scheduler::SchedulerDuplicateEvidence::WakeHintMessage);
        let decision = scheduler::decide_next_action(
            projection,
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
        self.emit_system_tick_from_wake_hint(pending, decision.work_item_id.as_deref())
            .await?;
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
        work_item_id_override: Option<&str>,
    ) -> Result<()> {
        let correlation_id = pending.correlation_id.clone();
        let causation_id = pending.causation_id.clone();
        let work_item_id = match work_item_id_override {
            Some(id) => Some(id.to_string()),
            None => {
                self.wake_hint_work_item_id(pending.external_trigger_id.as_deref())
                    .await?
            }
        };
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
        if let Some(wait_id) = self
            .exact_external_wait_correlation(pending.external_trigger_id.as_deref())
            .await?
        {
            message.source_refs.insert("wait_id".into(), wait_id);
        }
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
        work_item_generation: Option<u64>,
        reason: &str,
        decision: Option<&scheduler::SchedulerDecision>,
    ) -> Result<()> {
        let idempotency_key =
            scheduler::work_queue_tick_idempotency_key(work_item, reason, work_item_generation);
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
                "work_item_generation": work_item_generation,
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
        let base_audit_events = audit_events.clone();
        let mut commit = {
            let agent_id = message.agent_id.clone();
            let mut attempt = 0;
            loop {
                if attempt >= super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
                    return Err(anyhow!(
                        "work queue tick admission OCC retry exhausted for agent {}",
                        agent_id
                    ));
                }
                let mut guard = self.begin_mutation().await?;
                let expected_persisted_state = guard.last_persisted_state.clone();
                let mut committed_state = guard.state.clone();
                let previous_status = committed_state.status.clone();
                let previous_sleeping_until = committed_state.sleeping_until;
                committed_state.pending = guard.queue.len().saturating_add(1);
                committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
                committed_state.total_message_count =
                    self.inner.storage.count_messages()?.saturating_add(1);
                let mut audit_events = base_audit_events.clone();
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
                let command = crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Admit,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(queue_record),
                    scheduler_claim_work_item: None,
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_persisted_state)),
                        record: Box::new(committed_state.clone()),
                    }),
                    message_evidence: vec![message.clone()],
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                };
                let operation_id = command.audit_events[0].id.clone();
                let commit_result = guard
                    .commit_prepared_queue(
                        operation_id,
                        crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(
                            &command,
                        )),
                    )
                    .await;
                let commit = match commit_result {
                    Ok(commit) => commit,
                    Err(error) => {
                        let can_retry = attempt + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                            && super::retryable_enqueue_conflict(&error, &agent_id);
                        if !can_retry {
                            return Err(error);
                        }
                        drop(guard);
                        if !self.refresh_enqueue_agent_state_baseline(&agent_id).await? {
                            return Err(error);
                        }
                        attempt += 1;
                        continue;
                    }
                };
                if !commit.applied {
                    return Err(anyhow!(
                        "work queue tick admission made no durable progress"
                    ));
                }
                break commit;
            }
        };
        commit.effects.notify_scheduler = true;
        self.apply_transition_commit(commit).await;
        Ok(())
    }

    pub(super) async fn emit_system_tick_from_blocked_recheck(
        &self,
        work_items: &[crate::types::WorkItemRecord],
    ) -> Result<()> {
        // Stable primary selection: earliest due recheck, then WorkItem id.
        let mut ordered: Vec<&crate::types::WorkItemRecord> = work_items.iter().collect();
        ordered.sort_by(|left, right| {
            left.recheck_at
                .cmp(&right.recheck_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let items = ordered
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
        let primary = ordered.first().copied();
        let primary_work_item_id = primary.map(|item| item.id.clone());
        let primary_work_item_revision = primary.map(|item| item.revision);
        let recheck_lines = ordered
            .iter()
            .map(|item| {
                format!(
                    "- work_item={} objective={} blocked_by={} recheck_at={}",
                    item.id,
                    item.objective,
                    item.blocked_by.as_deref().unwrap_or("unset"),
                    item.recheck_at
                        .map(|recheck_at| recheck_at.to_rfc3339())
                        .unwrap_or_else(|| "unscheduled".to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
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
                    "{} blocked WorkItem recheck{} due; inspect the blocker and refresh or clear blocked_by when ready.\n{}",
                    work_items.len(),
                    if work_items.len() == 1 { " is" } else { "s are" },
                    recheck_lines,
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
                "primary_work_item_id": primary_work_item_id.clone(),
                "primary_work_item_revision": primary_work_item_revision,
                "items": items
            }
        }));
        message.work_item_id = primary_work_item_id;
        message.normalize_admission_fields();
        message.turn_id = normalized_turn_id(message.turn_id.as_deref());
        if message.turn_id.is_none() {
            message.turn_id = Some(crate::ids::turn_id());
        }
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
}

fn is_queued_available_retry_signal(message: &MessageEnvelope) -> bool {
    matches!(
        (&message.kind, &message.origin),
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. })
            | (MessageKind::ChannelEvent, MessageOrigin::Channel { .. })
            | (MessageKind::WebhookEvent, MessageOrigin::Webhook { .. })
            | (MessageKind::CallbackEvent, MessageOrigin::Callback { .. })
            | (MessageKind::TimerTick, MessageOrigin::Timer { .. })
            | (MessageKind::TaskResult, MessageOrigin::Task { .. })
    )
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
    use rusqlite::OptionalExtension;
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

    struct BlockingContinuationHook {
        entered: std::sync::atomic::AtomicBool,
        release: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl scheduler::AsyncSemanticCandidateSelectionHook for BlockingContinuationHook {
        async fn select_autonomous_continuation(
            &self,
            _context: &scheduler::AutonomousContinuationSelectionContext,
        ) -> Result<
            scheduler::SemanticCandidateSelectionHookResult,
            scheduler::SemanticCandidateSelectionHookError,
        > {
            self.entered.store(true, Ordering::SeqCst);
            self.release.notified().await;
            Ok(scheduler::SemanticCandidateSelectionHookResult::Abstain)
        }
    }

    #[tokio::test]
    async fn continuation_hook_refresh_waits_for_normal_commit_installation() {
        let test_runtime = test_runtime();
        let runtime = &test_runtime.runtime;
        {
            let mut mutation = runtime.begin_mutation().await.unwrap();
            mutation.state.status = AgentStatus::AwakeIdle;
            mutation.state.current_run_id = None;
            mutation.current_run_abort = None;
            mutation.persist_state(runtime.storage()).await.unwrap();
        }
        for (id, target) in [("first", "first candidate"), ("second", "second candidate")] {
            let mut work = WorkItemRecord::new("default", target, WorkItemState::Open);
            work.id = id.into();
            persist_test_work_item_async(&test_runtime, &work).await;
        }
        let hook = Arc::new(BlockingContinuationHook {
            entered: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
        });
        *runtime
            .inner
            .autonomous_continuation_decision_hook
            .write()
            .await = Some(hook.clone());
        let mut emit = runtime.maybe_emit_pending_system_tick(None);
        assert!(futures_util::poll!(emit.as_mut()).is_pending());
        assert!(hook.entered.load(Ordering::SeqCst));

        let previous_turn_index = runtime.agent_state().await.unwrap().turn_index;
        let cache = runtime.inner.projection_cache.lock().await;
        let writer_runtime = runtime.clone();
        let writer = tokio::spawn(async move {
            let mut mutation = writer_runtime.begin_mutation().await.unwrap();
            mutation.state.turn_index += 1;
            mutation.persist_state(writer_runtime.storage()).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    runtime.inner.mutation_owner.capture(),
                    Err(mutation_owner::MutationOwnerError::NeedsReconcile(_))
                ) && runtime.storage().read_agent().unwrap().unwrap().turn_index
                    == previous_turn_index + 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        hook.release.notify_one();
        assert!(futures_util::poll!(emit.as_mut()).is_pending());
        drop(cache);
        writer.await.unwrap().unwrap();
        assert!(emit.await.unwrap());
        assert_eq!(
            runtime.agent_state().await.unwrap().turn_index,
            previous_turn_index + 1
        );
        assert!(runtime.inner.mutation_owner.capture().is_ok());
    }

    #[test]
    fn due_agent_wait_recheck_is_exact_idempotent_and_model_reentering() {
        let test_runtime = test_runtime();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let registration = rt
            .block_on(test_runtime.runtime.register_wait_for(
                "default",
                None,
                WaitForWakeKind::OperatorInput,
                None,
                "recheck operator wait".into(),
                Some(0),
            ))
            .unwrap();
        let recheck_at = registration.condition.recheck_at().unwrap();
        let expected_message_id = crate::ids::wait_recheck_message_id(
            &registration.condition.id,
            &recheck_at.to_rfc3339(),
        );
        let persisted = test_runtime
            .runtime
            .storage()
            .raw_unresolved_wait_conditions_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .expect("agent wait should remain unresolved before its recheck");
        assert_eq!(persisted.status, crate::types::WaitConditionStatus::Active);
        assert_eq!(persisted.work_item_id, None);
        assert!(
            persisted.recheck_at().unwrap() <= test_runtime.runtime.now(),
            "zero-delay agent wait should already be due: persisted={:?}, now={:?}",
            persisted.recheck_at(),
            test_runtime.runtime.now(),
        );

        assert!(rt
            .block_on(test_runtime.runtime.emit_due_agent_wait_recheck())
            .unwrap());
        assert!(!rt
            .block_on(test_runtime.runtime.emit_due_agent_wait_recheck())
            .unwrap());

        let triggered = test_runtime
            .runtime
            .storage()
            .latest_wait_conditions_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap();
        assert_eq!(
            triggered.status,
            crate::types::WaitConditionStatus::Triggered
        );
        assert_eq!(
            triggered.trigger_message_id(),
            Some(expected_message_id.as_str())
        );

        let poll = rt
            .block_on(
                super::scheduler_executor::SchedulerDecisionExecutor::new(&test_runtime.runtime)
                    .poll(),
            )
            .unwrap();
        let super::scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            panic!("due agent wait recheck should be scheduled");
        };
        assert_eq!(scheduled.message.id, expected_message_id);
        assert!(scheduled.dispatch_plan.model_turn_allowed);
        assert!(scheduled
            .dispatch_plan
            .continuation_trigger
            .as_ref()
            .is_some_and(|trigger| trigger.exact_wait_recheck));
    }

    fn fixture_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Runtime::new().unwrap().block_on(future)
    }

    fn publish_fixture_state(
        test_runtime: &TestRuntime,
        update: impl FnOnce(&mut super::super::state_mutation::RuntimeMutation),
    ) {
        fixture_block_on(async {
            let mut owner = test_runtime.runtime.begin_mutation().await.unwrap();
            update(&mut owner);
            owner
                .persist_state(test_runtime.runtime.storage())
                .await
                .unwrap();
        });
    }

    fn set_agent_idle(test_runtime: &TestRuntime) {
        publish_fixture_state(test_runtime, |owner| {
            owner.state.status = AgentStatus::AwakeIdle
        });
    }

    fn clear_queue(test_runtime: &TestRuntime) {
        // Complete each actual admission; dropping a private queue would leave
        // its canonical pending responsibility and published root unchanged.
        fixture_block_on(async {
            let messages = {
                let agent = test_runtime.runtime.inner.agent.lock().await;
                agent
                    .queue
                    .shared_messages()
                    .map(|message| message.as_ref().clone())
                    .collect::<Vec<_>>()
            };
            for message in messages {
                assert!(test_runtime
                    .runtime
                    .commit_queue_settlement(
                        QueueEntryRecord {
                            message_id: message.id,
                            agent_id: message.agent_id,
                            priority: message.priority,
                            status: QueueEntryStatus::Dropped,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        Vec::new(),
                        false,
                    )
                    .await
                    .unwrap());
            }
        });
    }

    fn set_agent_status(test_runtime: &TestRuntime, status: AgentStatus) {
        publish_fixture_state(test_runtime, |owner| owner.state.status = status);
    }

    fn set_agent_live_running(test_runtime: &TestRuntime, run_id: &str) {
        publish_fixture_state(test_runtime, |owner| {
            owner.state.status = AgentStatus::AwakeRunning;
            owner.state.current_run_id = Some(run_id.into());
            owner.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: run_id.into(),
                token: CancellationToken::new(),
                reason: Arc::new(StdMutex::new("operator_aborted".into())),
            });
        });
    }

    fn persist_fixture_message(test_runtime: &TestRuntime, message: &MessageEnvelope) {
        use crate::runtime_db::{
            prepared_input_evidence::PreparedInputEvidence, RuntimeIndexChange,
        };
        fixture_block_on(async {
            let mut owner = test_runtime.runtime.begin_mutation().await.unwrap();
            let prepared = PreparedInputEvidence::message(
                owner.input_evidence_scope(),
                crate::ids::audit_event_id(),
                Utc::now(),
                message.clone(),
                vec![RuntimeIndexChange::for_message(message)],
            )
            .unwrap();
            owner
                .commit_prepared_evidence(prepared, None)
                .await
                .unwrap();
        });
    }

    fn add_queued_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let mut record = WorkItemRecord::new("default", target, WorkItemState::Open);
        record.id = id.to_string();
        persist_test_work_item(test_runtime, &record);
        record
    }

    fn append_queued_available_tick(
        test_runtime: &TestRuntime,
        work_item: &WorkItemRecord,
        id: &str,
        created_at: chrono::DateTime<chrono::Utc>,
        generation: u64,
    ) {
        let mut message = MessageEnvelope::new(
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
        message.id = id.into();
        message.created_at = created_at;
        message.work_item_id = Some(work_item.id.clone());
        message.metadata = Some(serde_json::json!({
            "work_queue": {
                "idempotency_key": scheduler::work_queue_tick_idempotency_key(
                    work_item,
                    "queued_available",
                    Some(generation),
                ),
                "reason": "queued_available",
                "work_item_id": work_item.id,
                "work_item_revision": work_item.revision,
                "work_item_generation": generation,
            }
        }));
        persist_fixture_message(&test_runtime, &message);
    }

    fn append_task_retry_signal(test_runtime: &TestRuntime, kind: MessageKind, id: &str) {
        let mut message = MessageEnvelope::new(
            "default",
            kind,
            MessageOrigin::Task {
                task_id: "child-task".into(),
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: "child task update".into(),
            },
        );
        message.id = id.into();
        persist_fixture_message(&test_runtime, &message);
    }

    fn persist_test_work_item(test_runtime: &TestRuntime, record: &WorkItemRecord) {
        fixture_block_on(persist_test_work_item_async(test_runtime, record));
    }

    // The fixture chooses exact IDs/readiness but still acknowledges actual
    // Work and protocol writes in one owner-held transaction before publication.
    fn persist_test_work_item_async<'a>(
        test_runtime: &'a TestRuntime,
        record: &'a WorkItemRecord,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            use crate::domain::execution_protocol::{
                WorkItemExecutionRecord, WorkItemExecutionState,
            };
            use crate::runtime::resident_work::{WorkFamily, WorkKey};
            use crate::runtime_db::{
                input_evidence_snapshot::{
                    register_committed_input_evidence_tx, validate_existing_input_evidence_tx,
                    CanonicalInputEvidence,
                },
                transitions::{PostCommitEffects, TransitionCommit},
                work_snapshot::{prepare_work_write_tx, register_work_writes_tx},
            };
            let runtime = &test_runtime.runtime;
            let mut owner = runtime.begin_mutation().await.unwrap();
            let (_, record_limit, byte_limit) = owner.context_input_evidence_policy().unwrap();
            let request = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit,
                byte_limit,
            };
            let final_state = owner.state.clone();
            let keys =
                std::collections::BTreeSet::from([WorkKey::new(WorkFamily::Work, &record.id)]);
            owner.commit_transition(crate::ids::audit_event_id(), |scope| {
                runtime.inner.runtime_db.transaction(|tx| {
                    let proof = prepare_work_write_tx(tx, scope, &keys)?;
                    let initialized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id=?1)", [&record.agent_id], |row| row.get(0))?;
                    let mut execution = if initialized {
                        crate::runtime_db::transitions::execution_protocol_repository::load_state_tx(tx, &record.agent_id)?
                    } else {
                        crate::domain::execution_protocol::ExecutionProtocolState::empty(&record.agent_id)
                    };
                    let execution_key = super::super::resident_input_evidence::InputEvidenceKey::new(
                        super::super::resident_input_evidence::InputEvidenceFamily::WorkExecution, &record.id,
                    );
                    validate_existing_input_evidence_tx(tx, scope, &execution_key)?;
                    let generation = execution.work_items.get(&record.id).map_or(record.revision.max(1), WorkItemExecutionRecord::generation);
                    let state = record.blocked_by.as_ref().map_or_else(
                        || WorkItemExecutionState::Runnable { generation, recovery_ref: None },
                        |reason| WorkItemExecutionState::Paused { generation, reason: reason.clone() },
                    );
                    let current = tx.query_row("SELECT revision FROM work_items WHERE work_item_id=?1", [&record.id], |row| row.get::<_, u64>(0)).optional()?;
                    let applied = match current {
                        Some(revision) => crate::runtime_db::repositories::update_expected_work_item_tx(tx, record, revision)?,
                        None => crate::runtime_db::repositories::insert_new_work_item_tx(tx, record)?,
                    };
                    anyhow::ensure!(applied, "fixture Work write was not applied");
                    execution.work_items.insert(record.id.clone(), WorkItemExecutionRecord { source_revision: record.revision, state });
                    crate::runtime_db::transitions::persist_state_tx(tx, &execution)?;
                    let execution = crate::runtime_db::transitions::execution_protocol_repository::load_state_tx(tx, &record.agent_id)?;
                    let stored = execution.work_items.get(&record.id).unwrap().clone();
                    let input_delta = register_committed_input_evidence_tx(tx, scope, &[CanonicalInputEvidence::WorkExecution { work_item_id: record.id.clone(), record: stored }])?;
                    let work = register_work_writes_tx(tx, &proof, &keys, &Default::default(), &final_state, &request)?;
                    Ok(TransitionCommit {
                        applied,
                        effects: PostCommitEffects {
                            work_items: vec![record.clone()],
                            execution_protocol: Some(execution),
                            input_evidence_delta: input_delta,
                            work_delta: Some(work.delta),
                            work_delta_requires_prepared_domain: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                })
            }).await.unwrap();
        })
    }

    fn set_work_item_generation(test_runtime: &TestRuntime, work_item_id: &str, generation: u64) {
        use crate::domain::execution_protocol::WorkItemExecutionState;
        use crate::runtime_db::input_evidence_snapshot::{
            register_committed_input_evidence_tx, validate_existing_input_evidence_tx,
            CanonicalInputEvidence,
        };
        fixture_block_on(async {
            let runtime = &test_runtime.runtime;
            let mut owner = runtime.begin_mutation().await.unwrap();
            owner.commit_transition(crate::ids::audit_event_id(), |scope| {
                runtime.inner.runtime_db.transaction(|tx| {
                    let mut execution = crate::runtime_db::transitions::execution_protocol_repository::load_state_tx(tx, "default")?;
                    let key = super::super::resident_input_evidence::InputEvidenceKey::new(super::super::resident_input_evidence::InputEvidenceFamily::WorkExecution, work_item_id);
                    validate_existing_input_evidence_tx(tx, scope, &key)?;
                    let work = execution.work_items.get_mut(work_item_id).expect("test WorkItem execution exists");
                    let recovery_ref = match &work.state {
                        WorkItemExecutionState::Runnable { recovery_ref, .. } => recovery_ref.clone(),
                        state => panic!("expected runnable test WorkItem, got {state:?}"),
                    };
                    work.state = WorkItemExecutionState::Runnable { generation, recovery_ref };
                    crate::runtime_db::transitions::persist_state_tx(tx, &execution)?;
                    let execution = crate::runtime_db::transitions::execution_protocol_repository::load_state_tx(tx, "default")?;
                    let stored = execution.work_items.get(work_item_id).unwrap().clone();
                    let delta = register_committed_input_evidence_tx(tx, scope, &[CanonicalInputEvidence::WorkExecution { work_item_id: work_item_id.to_owned(), record: stored }])?;
                    Ok(crate::runtime_db::transitions::TransitionCommit {
                        applied: true,
                        effects: crate::runtime_db::transitions::PostCommitEffects { execution_protocol: Some(execution), input_evidence_delta: delta, ..Default::default() },
                        ..Default::default()
                    })
                })
            }).await.unwrap();
        });
    }

    fn add_current_work_item(test_runtime: &TestRuntime, id: &str, target: &str) -> WorkItemRecord {
        let mut record = WorkItemRecord::new("default", target, WorkItemState::Open);
        record.id = id.to_string();
        persist_test_work_item(test_runtime, &record);
        publish_fixture_state(test_runtime, |owner| {
            owner.state.current_work_item_id = Some(record.id.clone())
        });
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
        fixture_block_on(test_runtime.runtime.persist_prepared_evidence(
            crate::runtime_db::prepared_input_evidence::InputEvidenceWrite::BriefCreated {
                brief: brief.clone(),
                event: AuditEvent::legacy(
                    "brief_created",
                    serde_json::json!({"brief_id": brief.id}),
                ),
            },
        ))
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
        publish_fixture_state(test_runtime, |owner| {
            owner.state.pending_wake_hint = Some(hint.clone())
        });
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

        // Admit a real pending message before testing queue precedence
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Test message".to_string(),
            },
        );

        // Keep canonical and published pending responsibility together
        fixture_block_on(test_runtime.runtime.enqueue(message)).unwrap();

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
    fn idle_agent_without_pending_work_emits_no_system_tick() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(!emitted, "idle agent without pending work should stay idle");
        assert!(get_emitted_system_ticks(&test_runtime).is_empty());
        assert!(
            test_runtime
                .runtime
                .inner
                .agent
                .blocking_lock()
                .queue
                .is_empty(),
            "idle reconciliation should not enqueue a message without a trigger"
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
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued input".to_string(),
            },
        );
        fixture_block_on(test_runtime.runtime.enqueue(message)).unwrap();

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
        set_agent_live_running(&test_runtime, "run-live");

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
    fn stale_running_projection_emits_pending_wake_hint_in_same_call() {
        let test_runtime = test_runtime();
        publish_fixture_state(&test_runtime, |owner| {
            owner.state.status = AgentStatus::AwakeRunning;
            owner.state.current_run_id = Some("run-stale".into());
            owner.state.pending_wake_hint = Some(PendingWakeHint {
                reason: "stale wake".into(),
                description: None,
                scope: None,
                external_trigger_id: None,
                source: Some("test".into()),
                resource: None,
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
                created_at: Utc::now(),
            });
        });

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted);
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "wake_hint");
        let state = rt.block_on(test_runtime.runtime.agent_state()).unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
        assert_eq!(state.pending_wake_hint, None);
        assert!(test_runtime
            .runtime
            .storage()
            .read_recent_events(20)
            .unwrap()
            .iter()
            .any(|event| {
                event.kind == "scheduler_stale_run_projection_recovered"
                    && event.data["boundary"] == "pending_system_tick"
            }));
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
    fn blocked_recheck_tick_names_target_work_item() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        let blocked =
            block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_item_recheck");
        assert_eq!(
            ticks[0].1["primary_work_item_id"].as_str(),
            Some(blocked.id.as_str())
        );
        assert_eq!(
            ticks[0].1["primary_work_item_revision"].as_u64(),
            Some(blocked.revision)
        );

        let message = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_messages(10)
            .unwrap()
            .into_iter()
            .find(|message| {
                matches!(
                    message.origin,
                    MessageOrigin::System { ref subsystem } if subsystem == "work_item_recheck"
                )
            })
            .expect("recheck tick message should be durable");
        assert_eq!(message.work_item_id.as_deref(), Some(blocked.id.as_str()));
        let MessageBody::Text { text } = &message.body else {
            panic!("recheck tick should carry a text body");
        };
        assert!(
            text.contains(&blocked.id),
            "body should name the target WorkItem: {text}"
        );
        assert!(
            text.contains("blocked-target"),
            "body should carry the target objective: {text}"
        );
        assert!(
            text.contains("waiting for timer"),
            "body should carry the blocker: {text}"
        );

        let guard = test_runtime.runtime.inner.agent.blocking_lock();
        assert!(
            guard.state.current_work_item_id.is_none(),
            "emitting a recheck must not switch the persistent current WorkItem"
        );
    }

    #[test]
    fn multiple_due_blocked_rechecks_choose_stable_primary_target() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let later = add_queued_work_item(&test_runtime, "wi-later", "later-target");
        let mut later = block_work_item_with_due_recheck(&test_runtime, &later, "later blocker");
        later.revision += 1;
        later.recheck_at = Some(chrono::Utc::now() - chrono::Duration::seconds(2));
        later.updated_at = chrono::Utc::now();
        persist_test_work_item(&test_runtime, &later);

        let earlier = add_queued_work_item(&test_runtime, "wi-earlier", "earlier-target");
        let mut earlier =
            block_work_item_with_due_recheck(&test_runtime, &earlier, "earlier blocker");
        earlier.revision += 1;
        earlier.recheck_at = Some(chrono::Utc::now() - chrono::Duration::seconds(10));
        earlier.updated_at = chrono::Utc::now();
        persist_test_work_item(&test_runtime, &earlier);

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());

        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_item_recheck");
        assert_eq!(ticks[0].1["count"].as_u64(), Some(2));
        assert_eq!(
            ticks[0].1["primary_work_item_id"].as_str(),
            Some("wi-earlier"),
            "the earliest due recheck is the stable primary target"
        );
        let items = ticks[0].1["items"]
            .as_array()
            .expect("recheck items should be structured");
        let ids = items
            .iter()
            .filter_map(|item| item["work_item_id"].as_str())
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&"wi-earlier"),
            "items should list every target"
        );
        assert!(ids.contains(&"wi-later"), "items should list every target");

        let message = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_messages(10)
            .unwrap()
            .into_iter()
            .find(|message| {
                matches!(
                    message.origin,
                    MessageOrigin::System { ref subsystem } if subsystem == "work_item_recheck"
                )
            })
            .expect("recheck tick message should be durable");
        let MessageBody::Text { text } = &message.body else {
            panic!("recheck tick should carry a text body");
        };
        assert!(
            text.contains("earlier-target") && text.contains("later-target"),
            "body should carry every target objective: {text}"
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
    fn current_work_item_takes_precedence_over_due_blocked_recheck() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        add_current_work_item(&test_runtime, "wi-active", "active-target");
        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "current runnable work should emit a system tick");
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");
        assert_eq!(ticks[0].1["reason"].as_str(), Some("continue_active"));
        assert_eq!(ticks[0].1["work_item_id"].as_str(), Some("wi-active"));

        let latest = latest_work_item(&test_runtime, "wi-blocked");
        assert!(
            latest
                .recheck_consumed_at
                .zip(latest.recheck_at)
                .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
            "the execution opportunity should consume the due recheck"
        );
    }

    #[test]
    fn queued_work_item_takes_precedence_over_due_blocked_recheck() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let blocked = add_queued_work_item(&test_runtime, "wi-blocked", "blocked-target");
        block_work_item_with_due_recheck(&test_runtime, &blocked, "waiting for timer");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "queued runnable work should emit a system tick");
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");
        assert_eq!(ticks[0].1["reason"].as_str(), Some("queued_available"));
        assert_eq!(ticks[0].1["work_item_id"].as_str(), Some("wi-queued"));

        let latest = latest_work_item(&test_runtime, "wi-blocked");
        assert!(
            latest
                .recheck_consumed_at
                .zip(latest.recheck_at)
                .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
            "the execution opportunity should consume the due recheck"
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
    fn queued_notification_selects_the_oldest_runnable_work_item() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let now = Utc::now();
        let mut newer = WorkItemRecord::new("default", "newer-target", WorkItemState::Open);
        newer.id = "wi-newer".to_string();
        newer.created_at = now;
        newer.updated_at = now;
        persist_test_work_item(&test_runtime, &newer);

        let mut older = WorkItemRecord::new("default", "older-target", WorkItemState::Open);
        older.id = "wi-older".to_string();
        older.created_at = now - chrono::Duration::seconds(1);
        older.updated_at = older.created_at;
        persist_test_work_item(&test_runtime, &older);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "oldest queued work should emit a system tick");
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].0, "work_queue");
        assert_eq!(ticks[0].1["reason"].as_str(), Some("queued_available"));
        assert_eq!(ticks[0].1["work_item_id"].as_str(), Some("wi-older"));
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
    fn queued_system_tick_persists_admission_facts() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");

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
            set_agent_idle(&test_runtime);
            add_queued_work_item(&test_runtime, "wi-fault", "fault-target");
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
    fn queued_available_generation_and_tool_activity_do_not_reset_backoff() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued_id = "wi-queued";
        let queued = add_queued_work_item(&test_runtime, queued_id, "queued-target");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();
        assert!(emitted, "first queued notification should be emitted");

        clear_queue(&test_runtime);
        set_work_item_generation(&test_runtime, queued_id, 2);
        let tool_now = chrono::Utc::now();
        fixture_block_on(test_runtime.runtime.persist_prepared_evidence(
            crate::runtime_db::prepared_input_evidence::InputEvidenceWrite::Tool(
                crate::types::ToolExecutionRecord {
                    id: "no-progress-tool".into(),
                    agent_id: "default".into(),
                    work_item_id: Some(queued_id.into()),
                    turn_index: 1,
                    turn_id: None,
                    tool_name: "ExecCommand".into(),
                    created_at: tool_now,
                    completed_at: Some(tool_now),
                    duration_ms: 1,
                    authority_class: AuthorityClass::RuntimeInstruction,
                    status: crate::types::ToolExecutionStatus::Success,
                    input: serde_json::json!({}),
                    output: serde_json::json!({"ok": true}),
                    summary: "A successful tool call without a WorkItem update.".into(),
                    invocation_surface: None,
                },
            ),
        ))
        .unwrap();

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
            .read_recent_events(100)
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "work_queue_redispatch_backoff_scheduled"
                && event.data["work_item_id"] == queued_id
                && event.data["work_item_revision"] == queued.revision
                && event.data["no_progress_attempts"] == 1
        }));
    }

    #[test]
    fn task_status_does_not_reset_backoff_but_task_result_does() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        append_queued_available_tick(
            &test_runtime,
            &queued,
            "prior-queued-tick",
            chrono::Utc::now() - chrono::Duration::seconds(10),
            1,
        );
        append_task_retry_signal(&test_runtime, MessageKind::TaskStatus, "task-started");

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(!rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        assert!(get_emitted_system_ticks(&test_runtime).is_empty());

        append_task_retry_signal(&test_runtime, MessageKind::TaskResult, "task-result");
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        assert_eq!(get_emitted_system_ticks(&test_runtime).len(), 1);
    }

    #[test]
    fn backed_off_and_stalled_items_do_not_starve_later_runnable_items() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let backoff = add_queued_work_item(&test_runtime, "wi-backoff", "backed-off target");
        let exhausted = add_queued_work_item(&test_runtime, "wi-exhausted", "stalled target");
        let ready = add_queued_work_item(&test_runtime, "wi-ready", "runnable target");
        let now = chrono::Utc::now();
        append_queued_available_tick(
            &test_runtime,
            &backoff,
            "backoff-tick",
            now - chrono::Duration::seconds(10),
            1,
        );
        for attempt in 0..QUEUED_AVAILABLE_NO_PROGRESS_LIMIT {
            append_queued_available_tick(
                &test_runtime,
                &exhausted,
                &format!("stalled-tick-{attempt}"),
                now - chrono::Duration::minutes(60 - attempt as i64),
                attempt as u64 + 1,
            );
        }

        let emitted = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "later runnable work should bypass suppressed items"
        );
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].1["work_item_id"], ready.id);
        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(100)
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "work_queue_redispatch_backoff_scheduled"
                && event.data["work_item_id"] == backoff.id
        }));
        assert!(events.iter().any(|event| {
            event.kind == "work_queue_redispatch_stalled"
                && event.data["work_item_id"] == exhausted.id
        }));
    }

    #[test]
    fn queued_available_retry_resumes_after_backoff_deadline() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        append_queued_available_tick(
            &test_runtime,
            &queued,
            "prior-queued-tick",
            chrono::Utc::now() - chrono::Duration::minutes(2),
            1,
        );
        set_work_item_generation(&test_runtime, &queued.id, 2);

        let emitted = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "the first bounded retry should run after one minute"
        );
        assert_eq!(get_emitted_system_ticks(&test_runtime).len(), 1);
    }

    #[test]
    fn overdue_queued_retry_deadline_is_preserved_for_idle_wake() {
        let test_runtime = test_runtime();
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let tick_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        append_queued_available_tick(&test_runtime, &queued, "prior-queued-tick", tick_at, 1);

        let retry_at = tick_at + chrono::Duration::minutes(1);
        let next_retry_at = test_runtime
            .runtime
            .next_queued_available_retry_at(std::slice::from_ref(&queued))
            .unwrap();

        assert_eq!(next_retry_at, Some(retry_at));
    }

    #[test]
    fn queued_available_work_item_revision_resets_retry_episode() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        append_queued_available_tick(
            &test_runtime,
            &queued,
            "prior-queued-tick",
            chrono::Utc::now() - chrono::Duration::seconds(10),
            1,
        );
        let mut revised = queued.clone();
        revised.revision += 1;
        revised.updated_at = chrono::Utc::now();
        persist_test_work_item(&test_runtime, &revised);

        let emitted = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(emitted, "a WorkItem revision change should reset backoff");
        assert_eq!(get_emitted_system_ticks(&test_runtime).len(), 1);
    }

    #[test]
    fn queued_available_stops_after_five_no_progress_attempts_until_new_input() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);
        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let now = chrono::Utc::now();
        for attempt in 0..QUEUED_AVAILABLE_NO_PROGRESS_LIMIT {
            append_queued_available_tick(
                &test_runtime,
                &queued,
                &format!("prior-queued-tick-{attempt}"),
                now - chrono::Duration::minutes(60 - attempt as i64),
                attempt as u64 + 1,
            );
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(!rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(100)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "work_queue_redispatch_stalled")
                .count(),
            1
        );

        assert!(!rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        let events = test_runtime
            .runtime
            .inner
            .storage
            .read_recent_events(100)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "work_queue_redispatch_stalled")
                .count(),
            1,
            "the escalation event is emitted once per no-progress episode"
        );

        let mut operator_signal = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue the queued item".into(),
            },
        );
        operator_signal.created_at = chrono::Utc::now();
        persist_fixture_message(&test_runtime, &operator_signal);
        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        assert_eq!(get_emitted_system_ticks(&test_runtime).len(), 1);
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
            Some("work_queue:queued_available:wi-queued:1:generation:1")
        );

        let mut updated = queued.clone();
        updated.revision += 1;
        updated.updated_at = chrono::Utc::now();
        persist_test_work_item(&test_runtime, &updated);

        assert!(rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap());
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 2);
        assert_eq!(
            ticks[1].1["idempotency_key"].as_str(),
            Some("work_queue:queued_available:wi-queued:2:generation:1")
        );
    }

    #[test]
    fn queued_system_tick_explicit_idempotency_key_allows_newer_signals() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let idempotency_key =
            scheduler::work_queue_tick_idempotency_key(&queued, "queued_available", Some(1));
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
        existing_tick.created_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        existing_tick.work_item_id = Some(queued.id.clone());
        existing_tick.metadata = Some(serde_json::json!({
            "work_queue": {
                "idempotency_key": idempotency_key,
                "reason": "queued_available",
                "work_item_id": queued.id,
                "work_item_revision": queued.revision
            }
        }));
        persist_fixture_message(&test_runtime, &existing_tick);

        let mut newer_operator_signal = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "newer operator signal".into(),
            },
        );
        newer_operator_signal.created_at = chrono::Utc::now();
        persist_fixture_message(&test_runtime, &newer_operator_signal);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let emitted = rt
            .block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
            .unwrap();

        assert!(
            emitted,
            "a newer work signal must allow another queued tick despite the matching idempotency key"
        );
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(
            ticks[0].1["idempotency_key"].as_str(),
            Some("work_queue:queued_available:wi-queued:1:generation:1")
        );
    }

    #[test]
    fn queued_system_tick_retries_after_runtime_error() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let queued = add_queued_work_item(&test_runtime, "wi-queued", "queued-target");
        let idempotency_key =
            scheduler::work_queue_tick_idempotency_key(&queued, "queued_available", None);
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
        existing_tick.id = "failed-work-queue-tick".into();
        existing_tick.created_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        existing_tick.work_item_id = Some(queued.id.clone());
        existing_tick.metadata = Some(serde_json::json!({
            "work_queue": {
                "idempotency_key": idempotency_key,
                "reason": "queued_available",
                "work_item_id": queued.id,
                "work_item_revision": queued.revision
            }
        }));
        persist_fixture_message(&test_runtime, &existing_tick);
        test_runtime
            .runtime
            .inner
            .storage
            .append_event(&AuditEvent::legacy(
                "runtime_error",
                serde_json::json!({"message_id": existing_tick.id}),
            ))
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(
            rt.block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
                .unwrap(),
            "provider failure must allow a queued WorkItem retry tick"
        );
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].1["reason"].as_str(), Some("queued_available"));
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

        publish_fixture_state(&test_runtime, |owner| {
            owner.state.pending_wake_hint = Some(hint.clone())
        });

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
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "New follow-up".to_string(),
            },
        );
        persist_fixture_message(&test_runtime, &message);

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
    fn continue_active_retries_after_runtime_error() {
        let test_runtime = test_runtime();
        set_agent_idle(&test_runtime);

        let active = add_current_work_item(&test_runtime, "wi-active", "active-target");
        set_work_item_generation(&test_runtime, &active.id, 2);
        let idempotency_key =
            scheduler::work_queue_tick_idempotency_key(&active, "continue_active", Some(1));
        let mut existing_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "continue current work item".into(),
            },
        );
        existing_tick.id = "failed-continue-active-tick".into();
        existing_tick.created_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        existing_tick.work_item_id = Some(active.id.clone());
        existing_tick.metadata = Some(serde_json::json!({
            "work_queue": {
                "idempotency_key": idempotency_key,
                "reason": "continue_active",
                "work_item_id": active.id.clone(),
                "work_item_revision": active.revision,
                "work_item_generation": 1
            }
        }));
        persist_fixture_message(&test_runtime, &existing_tick);
        append_result_brief_for_work_item(&test_runtime, &active.id, "Failed progress.");
        test_runtime
            .runtime
            .inner
            .storage
            .append_event(&AuditEvent::legacy(
                "runtime_error",
                serde_json::json!({"work_item_id": active.id}),
            ))
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(
            rt.block_on(test_runtime.runtime.maybe_emit_pending_system_tick(None))
                .unwrap(),
            "provider failure must allow an active WorkItem retry tick"
        );
        let ticks = get_emitted_system_ticks(&test_runtime);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].1["reason"].as_str(), Some("continue_active"));
        assert_eq!(
            ticks[0].1["work_item_generation"].as_u64(),
            Some(2),
            "a newer execution generation must not be suppressed by an older tick"
        );
        assert_eq!(
            ticks[0].1["idempotency_key"].as_str(),
            Some("work_queue:continue_active:wi-active:1:generation:2")
        );
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
        persist_test_work_item(&test_runtime, &active);
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
        set_agent_live_running(&test_runtime, "run-live");

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
