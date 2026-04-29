use super::*;

use crate::callbacks::{build_callback_url, generate_callback_token};
use crate::types::WaitingReason;

impl RuntimeHandle {
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

    pub async fn create_callback(
        &self,
        summary: String,
        source: String,
        condition: String,
        resource: Option<String>,
        delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        let waiting_intent_id = Uuid::new_v4().to_string();
        let external_trigger_id = Uuid::new_v4().to_string();
        let token = generate_callback_token();
        let waiting = WaitingIntentRecord {
            id: waiting_intent_id.clone(),
            agent_id: agent_id.clone(),
            work_item_id: self
                .inner
                .agent
                .lock()
                .await
                .state
                .current_turn_work_item_id
                .clone(),
            summary,
            source: source.clone(),
            resource,
            condition,
            delivery_mode: delivery_mode.clone(),
            status: WaitingIntentStatus::Active,
            external_trigger_id: external_trigger_id.clone(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        };
        let descriptor = ExternalTriggerRecord {
            external_trigger_id: external_trigger_id.clone(),
            target_agent_id: agent_id.clone(),
            waiting_intent_id: waiting_intent_id.clone(),
            delivery_mode: delivery_mode.clone(),
            token_hash: crate::callbacks::hash_callback_token(&token),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        };

        self.inner.storage.append_waiting_intent(&waiting)?;
        self.inner.storage.append_external_trigger(&descriptor)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "waiting_intent_created",
            serde_json::json!({
                "waiting_intent_id": waiting.id,
                "external_trigger_id": descriptor.external_trigger_id,
                "agent_id": agent_id,
                "source": source,
                "delivery_mode": descriptor.delivery_mode,
            }),
        ))?;

        let trigger_url = build_callback_url(&self.inner.callback_base_url, &delivery_mode, &token);
        Ok(ExternalTriggerCapability {
            waiting_intent_id,
            external_trigger_id,
            trigger_url,
            target_agent_id: self.agent_id().await?,
            delivery_mode,
        })
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

        if let Some(descriptor) = self
            .latest_external_triggers()
            .await?
            .into_iter()
            .find(|record| record.external_trigger_id == waiting.external_trigger_id)
        {
            if descriptor.status != ExternalTriggerStatus::Revoked {
                let mut revoked = descriptor;
                revoked.status = ExternalTriggerStatus::Revoked;
                revoked.revoked_at = Some(now);
                self.inner.storage.append_external_trigger(&revoked)?;
            }
        }

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

    pub async fn deliver_callback(
        &self,
        descriptor_id: &str,
        payload: CallbackDeliveryPayload,
    ) -> Result<CallbackDeliveryResult> {
        let descriptor = self
            .latest_external_triggers()
            .await?
            .into_iter()
            .find(|record| record.external_trigger_id == descriptor_id)
            .ok_or_else(|| anyhow!("external trigger {} not found", descriptor_id))?;
        if descriptor.status != ExternalTriggerStatus::Active {
            return Err(anyhow!("external trigger is not active"));
        }

        let waiting = self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .find(|record| record.id == descriptor.waiting_intent_id)
            .ok_or_else(|| anyhow!("waiting intent {} not found", descriptor.waiting_intent_id))?;
        if waiting.status != WaitingIntentStatus::Active {
            return Err(anyhow!("waiting intent is not active"));
        }

        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        let correlation_id = payload
            .correlation_id
            .clone()
            .or_else(|| waiting.correlation_id.clone());
        let causation_id = payload
            .causation_id
            .clone()
            .or_else(|| waiting.causation_id.clone());

        let disposition = match descriptor.delivery_mode {
            CallbackDeliveryMode::EnqueueMessage => {
                let body = payload
                    .body
                    .clone()
                    .ok_or_else(|| anyhow!("enqueue_message callback requires a request body"))?;
                let external_trigger_id = descriptor.external_trigger_id.clone();

                let mut message = MessageEnvelope::new(
                    agent_id.clone(),
                    MessageKind::CallbackEvent,
                    MessageOrigin::Callback {
                        descriptor_id: descriptor.external_trigger_id.clone(),
                        source: Some(waiting.source.clone()),
                    },
                    TrustLevel::TrustedIntegration,
                    Priority::Next,
                    body,
                )
                .with_admission(
                    crate::types::MessageDeliverySurface::HttpCallbackEnqueue,
                    crate::types::AdmissionContext::ExternalTriggerCapability,
                );
                message.metadata = Some(serde_json::json!({
                    "waiting_intent_id": waiting.id,
                    "external_trigger_id": external_trigger_id,
                    "source": waiting.source,
                    "resource": waiting.resource,
                    "content_type": payload.content_type,
                }));
                message.correlation_id = correlation_id.clone();
                message.causation_id = causation_id.clone();
                self.enqueue(message).await?;
                CallbackIngressDisposition::Enqueued
            }
            CallbackDeliveryMode::WakeOnly => {
                let disposition = self
                    .submit_wake_hint(WakeHint {
                        agent_id: agent_id.clone(),
                        reason: callback_wake_reason(&waiting, payload.body.as_ref()),
                        source: Some(waiting.source.clone()),
                        resource: waiting.resource.clone(),
                        body: payload.body.clone(),
                        content_type: payload.content_type.clone(),
                        correlation_id: correlation_id.clone(),
                        causation_id: causation_id.clone(),
                    })
                    .await?;
                match disposition {
                    WakeDisposition::Triggered => CallbackIngressDisposition::Triggered,
                    WakeDisposition::Coalesced => CallbackIngressDisposition::Coalesced,
                    WakeDisposition::Ignored => CallbackIngressDisposition::Ignored,
                }
            }
        };

        let mut updated_waiting = waiting;
        if updated_waiting.correlation_id.is_none() {
            updated_waiting.correlation_id = correlation_id.clone();
        }
        if updated_waiting.causation_id.is_none() {
            updated_waiting.causation_id = causation_id.clone();
        }
        updated_waiting.last_triggered_at = Some(now);
        updated_waiting.trigger_count += 1;
        self.inner.storage.append_waiting_intent(&updated_waiting)?;

        let mut updated_descriptor = descriptor;
        updated_descriptor.last_delivered_at = Some(now);
        updated_descriptor.delivery_count += 1;
        self.inner
            .storage
            .append_external_trigger(&updated_descriptor)?;

        let updated_descriptor_id = updated_descriptor.external_trigger_id.clone();
        let delivery_surface = match &updated_descriptor.delivery_mode {
            CallbackDeliveryMode::EnqueueMessage => {
                crate::types::MessageDeliverySurface::HttpCallbackEnqueue
            }
            CallbackDeliveryMode::WakeOnly => {
                crate::types::MessageDeliverySurface::HttpCallbackWake
            }
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "callback_delivered",
            serde_json::json!({
                "agent_id": agent_id,
                "waiting_intent_id": updated_waiting.id,
                "external_trigger_id": updated_descriptor_id,
                "delivery_mode": updated_descriptor.delivery_mode,
                "origin": "callback",
                "delivery_surface": delivery_surface,
                "disposition": disposition,
                "admission_context": crate::types::AdmissionContext::ExternalTriggerCapability,
                "authority_class": crate::types::AuthorityClass::IntegrationSignal,
            }),
        ))?;

        Ok(CallbackDeliveryResult {
            agent_id,
            waiting_intent_id: updated_waiting.id,
            external_trigger_id: updated_descriptor.external_trigger_id,
            delivery_mode: updated_descriptor.delivery_mode,
            disposition,
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
        let active_waiting = self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .collect::<Vec<_>>();
        if active_waiting.is_empty() {
            return Ok(());
        }

        let current_active_work_item_id = self
            .inner
            .storage
            .work_queue_prompt_projection()?
            .active
            .as_ref()
            .map(|item| item.id.clone());
        let prior_anchor_id = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .working_memory
                .current_working_memory
                .active_work_item_id
                .clone()
        };

        if current_active_work_item_id.is_none() {
            let cancelled_ids = self
                .cancel_waiting_intents(
                    active_waiting
                        .iter()
                        .map(|record| record.id.clone())
                        .collect(),
                )
                .await?;
            self.inner.storage.append_event(&AuditEvent::new(
                "missing_active_work_item_before_wait",
                serde_json::json!({
                    "agent_id": self.agent_id().await?,
                    "message_id": message.id,
                    "waiting_intent_ids": cancelled_ids,
                    "prior_active_work_item_id": prior_anchor_id,
                    "closure": pre_cleanup_closure,
                }),
            ))?;
            return Ok(());
        }

        let mut stale_ids = Vec::new();
        let anchor_switched = prior_anchor_id.is_some()
            && prior_anchor_id.as_deref() != current_active_work_item_id.as_deref();
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
                "prior_active_work_item_id": prior_anchor_id,
                "current_active_work_item_id": current_active_work_item_id,
                "closure": pre_cleanup_closure,
            }),
        ))?;
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

fn callback_wake_reason(waiting: &WaitingIntentRecord, body: Option<&MessageBody>) -> String {
    match body {
        Some(MessageBody::Text { text }) if !text.trim().is_empty() => text.trim().to_string(),
        Some(MessageBody::Json { value }) => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            truncate_activation_text(&rendered)
        }
        Some(MessageBody::Brief { text, .. }) if !text.trim().is_empty() => text.trim().to_string(),
        _ => format!("callback triggered: {}", waiting.summary),
    }
}

fn truncate_activation_text(text: &str) -> String {
    if text.chars().count() <= 160 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}
