use super::*;

use crate::callbacks::{build_callback_url, generate_callback_token};
use crate::ingress::WakeHint;

impl RuntimeHandle {
    pub async fn create_callback(
        &self,
        summary: String,
        source: String,
        condition: String,
        resource: Option<String>,
        delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        self.create_external_trigger(
            summary,
            source,
            ExternalTriggerScope::WorkItem,
            delivery_mode,
            Some(condition),
            resource,
        )
        .await
    }

    pub async fn create_external_trigger(
        &self,
        description: String,
        source: String,
        scope: ExternalTriggerScope,
        delivery_mode: CallbackDeliveryMode,
        condition: Option<String>,
        resource: Option<String>,
    ) -> Result<ExternalTriggerCapability> {
        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        let waiting_intent_id = Uuid::new_v4().to_string();
        let external_trigger_id = Uuid::new_v4().to_string();
        let token = generate_callback_token();
        let work_item_id = match scope {
            ExternalTriggerScope::WorkItem => Some(
                self.current_waiting_work_item_anchor(&agent_id)
                    .await?
                    .ok_or_else(|| {
                        anyhow!("work_item scoped external trigger requires a current work item")
                    })?,
            ),
            ExternalTriggerScope::Agent => None,
        };
        if let Some(work_item_id) = work_item_id.as_deref() {
            self.cancel_work_item_waiting_intents(work_item_id, "waiting_condition_replaced")
                .await?;
        }
        let waiting = WaitingIntentRecord {
            id: waiting_intent_id.clone(),
            agent_id: agent_id.clone(),
            scope: scope.clone(),
            work_item_id,
            description,
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
        let trigger_url = build_callback_url(&self.inner.callback_base_url, &delivery_mode, &token);
        let descriptor = ExternalTriggerRecord {
            external_trigger_id: external_trigger_id.clone(),
            target_agent_id: agent_id.clone(),
            waiting_intent_id: waiting_intent_id.clone(),
            scope: scope.clone(),
            delivery_mode: delivery_mode.clone(),
            trigger_url: Some(trigger_url.clone()),
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
                "scope": descriptor.scope,
                "delivery_mode": descriptor.delivery_mode,
            }),
        ))?;

        Ok(ExternalTriggerCapability {
            waiting_intent_id,
            external_trigger_id,
            trigger_url,
            target_agent_id: self.agent_id().await?,
            scope,
            delivery_mode,
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
                    "work_item_id": waiting.work_item_id,
                    "description": waiting.description,
                    "source": waiting.source,
                    "scope": waiting.scope,
                    "resource": waiting.resource,
                    "content_type": payload.content_type,
                }));
                message.correlation_id = correlation_id.clone();
                message.causation_id = causation_id.clone();
                self.enqueue(message).await?;
                CallbackIngressDisposition::Enqueued
            }
            CallbackDeliveryMode::WakeHint => {
                let disposition = self
                    .submit_wake_hint(WakeHint {
                        agent_id: agent_id.clone(),
                        reason: callback_wake_reason(&waiting, payload.body.as_ref()),
                        description: Some(waiting.description.clone()),
                        source: Some(waiting.source.clone()),
                        scope: Some(waiting.scope.clone()),
                        waiting_intent_id: Some(waiting.id.clone()),
                        external_trigger_id: Some(descriptor.external_trigger_id.clone()),
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
            CallbackDeliveryMode::WakeHint => {
                crate::types::MessageDeliverySurface::HttpCallbackWake
            }
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "callback_delivered",
            serde_json::json!({
                "agent_id": agent_id,
                "waiting_intent_id": updated_waiting.id,
                "work_item_id": updated_waiting.work_item_id,
                "external_trigger_id": updated_descriptor_id,
                "scope": updated_descriptor.scope,
                "delivery_mode": updated_descriptor.delivery_mode,
                "source": updated_waiting.source,
                "resource": updated_waiting.resource,
                "trigger_count": updated_waiting.trigger_count,
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
            scope: updated_descriptor.scope,
            delivery_mode: updated_descriptor.delivery_mode,
            disposition,
        })
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
        _ => format!("external trigger fired: {}", waiting.description),
    }
}

fn truncate_activation_text(text: &str) -> String {
    if text.chars().count() <= 160 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}
