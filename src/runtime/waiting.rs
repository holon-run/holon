use super::*;

use crate::ingress::WakeHint;
use crate::types::{WaitConditionRecord, WaitConditionSummary, WaitingReason, WakeSource};
use std::time::Duration;

impl RuntimeHandle {
    pub async fn submit_wake_hint(&self, hint: WakeHint) -> Result<WakeDisposition> {
        let runtime_agent_id = self.agent_id().await?;
        let pending = PendingWakeHint {
            reason: hint.reason.clone(),
            description: hint.description.clone(),
            source: hint.source.clone(),
            scope: hint.scope.clone(),
            waiting_intent_id: hint.waiting_intent_id.clone(),
            external_trigger_id: hint.external_trigger_id.clone(),
            resource: hint.resource.clone(),
            body: hint.body.clone(),
            content_type: hint.content_type.clone(),
            correlation_id: hint.correlation_id.clone(),
            causation_id: hint.causation_id.clone(),
            created_at: Utc::now(),
        };
        let work_item_id = self
            .waiting_intent_work_item_id(hint.waiting_intent_id.as_deref())
            .await?;

        let mut trigger_now = false;
        let disposition = {
            let mut guard = self.inner.agent.lock().await;
            match guard.state.status {
                AgentStatus::Stopped => WakeDisposition::Ignored,
                AgentStatus::AwakeRunning | AgentStatus::AwaitingTask => {
                    guard.state.pending_wake_hint = Some(pending.clone());
                    self.inner.storage.write_agent(&guard.state)?;
                    WakeDisposition::Coalesced
                }
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep => {
                    if guard.queue.is_empty() {
                        if guard.state.pending_wake_hint.take().is_some() {
                            self.inner.storage.write_agent(&guard.state)?;
                        }
                        trigger_now = true;
                        WakeDisposition::Triggered
                    } else {
                        guard.state.pending_wake_hint = Some(pending.clone());
                        self.inner.storage.write_agent(&guard.state)?;
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
        self.inner.storage.append_event(&AuditEvent::new(
            event_kind,
            serde_json::json!({
                "agent_id": runtime_agent_id,
                "reason": hint.reason,
                "description": hint.description,
                "source": hint.source,
                "scope": hint.scope,
                "waiting_intent_id": hint.waiting_intent_id,
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
                    self.inner.storage.write_agent(&guard.state)?;
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
                self.inner.storage.write_agent(&guard.state)?;
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
        let created_at = Utc::now();
        let timer = TimerRecord {
            id: Uuid::new_v4().to_string(),
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
        self.inner.storage.append_timer(&timer)?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("timer_created", to_json_value(&timer)))?;
        self.spawn_timer_loop(timer.clone());

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
                let now = Utc::now();
                if next_fire_at > now {
                    let wait = (next_fire_at - now)
                        .to_std()
                        .unwrap_or_else(|_| Duration::from_millis(0));
                    tokio::time::sleep(wait).await;
                }
                if let Err(err) = runtime.fire_timer_record(&mut timer).await {
                    let _ = runtime.inner.storage.append_event(&AuditEvent::new(
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
        let timer = normalize_recovered_timer(timer);
        let now = Utc::now();
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
        let message = MessageEnvelope {
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
        self.enqueue(message).await?;

        let fired_at = Utc::now();
        timer.last_fired_at = Some(fired_at);
        timer.fire_count += 1;
        if let Some(interval_ms) = timer.interval_ms {
            timer.status = TimerStatus::Active;
            timer.next_fire_at = Some(advance_time(fired_at, interval_ms)?);
        } else {
            timer.status = TimerStatus::Completed;
            timer.next_fire_at = None;
        }
        self.inner.storage.append_timer(timer)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "timer_fired",
            serde_json::json!({
                "timer_id": timer.id,
                "status": timer.status,
                "fire_count": timer.fire_count,
                "next_fire_at": timer.next_fire_at,
            }),
        ))?;
        Ok(())
    }

    pub async fn latest_waiting_intents(&self) -> Result<Vec<WaitingIntentRecord>> {
        let agent_id = self.agent_id().await?;
        let mut records = self
            .inner
            .storage
            .latest_waiting_intents()?
            .into_iter()
            .filter(|record| record.agent_id == agent_id)
            .collect::<Vec<_>>();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub async fn latest_external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        let agent_id = self.agent_id().await?;
        let mut records = self
            .inner
            .storage
            .latest_external_triggers()?
            .into_iter()
            .filter(|record| record.target_agent_id == agent_id)
            .collect::<Vec<_>>();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub(super) async fn waiting_intent_work_item_id(
        &self,
        waiting_intent_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(waiting_intent_id) = waiting_intent_id else {
            return Ok(None);
        };
        Ok(self
            .inner
            .storage
            .latest_waiting_intent(&self.agent_id().await?, waiting_intent_id)?
            .and_then(|record| record.work_item_id))
    }

    pub async fn cancel_waiting(&self, waiting_intent_id: &str) -> Result<CancelWaitingResult> {
        let waiting = self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .find(|record| record.id == waiting_intent_id)
            .ok_or_else(|| anyhow!("waiting intent {} not found", waiting_intent_id))?;
        let now = Utc::now();

        let updated_waiting = if waiting.status == WaitingIntentStatus::Cancelled {
            waiting.clone()
        } else {
            let mut updated = waiting.clone();
            updated.status = WaitingIntentStatus::Cancelled;
            updated.cancelled_at = Some(now);
            self.inner.storage.append_waiting_intent(&updated)?;
            updated
        };

        self.inner.storage.append_event(&AuditEvent::new(
            "waiting_intent_cancelled",
            serde_json::json!({
                "waiting_intent_id": updated_waiting.id,
                "external_trigger_id": updated_waiting.external_trigger_id,
            }),
        ))?;

        Ok(CancelWaitingResult {
            waiting_intent_id: updated_waiting.id,
            external_trigger_id: updated_waiting.external_trigger_id,
            status: updated_waiting.status,
        })
    }

    pub(super) async fn active_waiting_intent_summaries(
        &self,
    ) -> Result<Vec<WaitingIntentSummary>> {
        Ok(self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .map(|record| WaitingIntentSummary {
                id: record.id,
                scope: record.scope,
                description: record.description,
                source: record.source,
                resource: record.resource,
                condition: record.condition,
                delivery_mode: record.delivery_mode,
                status: record.status,
                trigger_count: record.trigger_count,
                created_at: record.created_at,
                cancelled_at: record.cancelled_at,
                last_triggered_at: record.last_triggered_at,
            })
            .collect())
    }

    pub(super) async fn active_work_item_waiting_intent_count(&self) -> Result<usize> {
        Ok(self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .filter(|record| record.scope == ExternalTriggerScope::WorkItem)
            .count())
    }

    pub(super) async fn active_wait_condition_summaries(
        &self,
    ) -> Result<Vec<WaitConditionSummary>> {
        let agent_id = self.agent_id().await?;
        Ok(self
            .inner
            .storage
            .latest_active_wait_conditions_for_agent(&agent_id)?
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
                waiting_intent_id: record.waiting_intent_id,
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

    pub(super) async fn reconcile_waiting_contract(
        &self,
        message: &MessageEnvelope,
        pre_cleanup_closure: &ClosureDecision,
    ) -> Result<()> {
        self.record_wait_reconciliation_signals(message).await?;

        let active_waiting = self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .filter(|record| record.scope == ExternalTriggerScope::WorkItem)
            .collect::<Vec<_>>();
        if active_waiting.is_empty() {
            return Ok(());
        }

        let current_work_item_id = self
            .inner
            .storage
            .work_queue_prompt_projection()?
            .current
            .as_ref()
            .map(|item| item.id.clone());
        let prior_anchor_id = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .working_memory
                .current_working_memory
                .current_work_item_id
                .clone()
        };

        if current_work_item_id.is_none() {
            let cancelled_ids = self
                .cancel_waiting_intents(
                    active_waiting
                        .iter()
                        .map(|record| record.id.clone())
                        .collect(),
                )
                .await?;
            self.inner.storage.append_event(&AuditEvent::new(
                "missing_current_work_item_before_wait",
                serde_json::json!({
                    "agent_id": self.agent_id().await?,
                    "message_id": message.id,
                    "waiting_intent_ids": cancelled_ids,
                    "prior_current_work_item_id": prior_anchor_id,
                    "closure": pre_cleanup_closure,
                }),
            ))?;
            return Ok(());
        }

        let mut stale_ids = Vec::new();
        let anchor_switched = prior_anchor_id.is_some()
            && prior_anchor_id.as_deref() != current_work_item_id.as_deref();
        if anchor_switched {
            stale_ids.extend(
                active_waiting
                    .iter()
                    .filter(|record| record.created_at < message.created_at)
                    .map(|record| record.id.clone()),
            );
        } else if pre_cleanup_closure.waiting_reason != Some(WaitingReason::AwaitingExternalChange)
        {
            stale_ids.extend(
                active_waiting
                    .iter()
                    .filter(|record| record.created_at < message.created_at)
                    .map(|record| record.id.clone()),
            );
        }

        if stale_ids.is_empty() {
            return Ok(());
        }

        let reason = if anchor_switched {
            "active_work_switched"
        } else {
            "closure_no_longer_waiting_on_external_change"
        };
        let cancelled_ids = self.cancel_waiting_intents(stale_ids).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "stale_waiting_intents_cancelled",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "message_id": message.id,
                "reason": reason,
                "waiting_intent_ids": cancelled_ids,
                "prior_current_work_item_id": prior_anchor_id,
                "current_work_item_id": current_work_item_id,
                "closure": pre_cleanup_closure,
            }),
        ))?;
        Ok(())
    }

    async fn record_wait_reconciliation_signals(&self, message: &MessageEnvelope) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let active_conditions = self
            .inner
            .storage
            .latest_active_wait_conditions_for_agent(&agent_id)?;
        if active_conditions.is_empty() {
            return Ok(());
        }

        for signal in reconciliation_signals_for_message(message, &active_conditions) {
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
            self.inner
                .storage
                .append_event(&AuditEvent::new("wait_reconciliation_requested", signal))?;
        }
        Ok(())
    }

    async fn cancel_waiting_intents(&self, waiting_intent_ids: Vec<String>) -> Result<Vec<String>> {
        let mut cancelled = Vec::new();
        for waiting_intent_id in waiting_intent_ids {
            self.cancel_waiting(&waiting_intent_id).await?;
            cancelled.push(waiting_intent_id);
        }
        Ok(cancelled)
    }
}

fn reconciliation_signals_for_message(
    message: &MessageEnvelope,
    active_conditions: &[WaitConditionRecord],
) -> Vec<serde_json::Value> {
    active_conditions
        .iter()
        .filter_map(|condition| reconciliation_signal_for_condition(message, condition))
        .collect()
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
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id))
            .then(|| ("task_result".to_string(), Some(task_id.clone()))),
        (MessageKind::CallbackEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            let waiting_intent_id = message.source_refs.get("waiting_intent_id");
            condition
                .wake_sources
                .iter()
                .any(|source| match source {
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some(id),
                    } => external_trigger_id == Some(id),
                    WakeSource::ExternalIngress {
                        external_trigger_id: None,
                    } => true,
                    _ => false,
                })
                .then(|| {
                    (
                        "external_ingress".to_string(),
                        external_trigger_id
                            .cloned()
                            .or_else(|| waiting_intent_id.cloned()),
                    )
                })
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::Timer { .. }))
            .then(|| ("timer".to_string(), Some(timer_id.clone()))),
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { actor_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput))
            .then(|| ("operator_input".to_string(), actor_id.clone())),
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
    let waiting_intent_id = wake_hint
        .get("waiting_intent_id")
        .and_then(serde_json::Value::as_str);
    let matches_external = condition.wake_sources.iter().any(|source| match source {
        WakeSource::ExternalIngress {
            external_trigger_id: Some(id),
        } => Some(id.as_str()) == external_trigger_id,
        WakeSource::ExternalIngress {
            external_trigger_id: None,
        } => true,
        _ => false,
    });
    matches_external.then(|| {
        (
            "external_ingress".to_string(),
            external_trigger_id
                .or(waiting_intent_id)
                .map(ToString::to_string),
        )
    })
}

fn advance_time(base: chrono::DateTime<Utc>, delta_ms: u64) -> Result<chrono::DateTime<Utc>> {
    let delta_ms = i64::try_from(delta_ms).context("duration_ms exceeds supported timer range")?;
    let delta = chrono::Duration::try_milliseconds(delta_ms)
        .ok_or_else(|| anyhow!("duration_ms exceeds supported timer range"))?;
    Ok(base + delta)
}

fn normalize_recovered_timer(mut timer: TimerRecord) -> TimerRecord {
    if timer.next_fire_at.is_some() {
        return timer;
    }

    let anchor = timer.last_fired_at.unwrap_or(timer.created_at);
    let fallback_ms = timer.interval_ms.unwrap_or(timer.duration_ms);
    timer.next_fire_at = advance_time(anchor, fallback_ms).ok().or(Some(Utc::now()));
    timer
}
