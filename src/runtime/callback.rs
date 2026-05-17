use super::*;

use crate::callbacks::{build_callback_url, generate_callback_token};
use crate::ingress::WakeHint;

impl RuntimeHandle {
    pub async fn create_callback(
        &self,
        _summary: String,
        _source: String,
        _condition: String,
        _resource: Option<String>,
        delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        self.default_external_trigger(delivery_mode).await
    }

    pub async fn create_external_trigger(
        &self,
        _description: String,
        _source: String,
        _scope: ExternalTriggerScope,
        delivery_mode: CallbackDeliveryMode,
        _condition: Option<String>,
        _resource: Option<String>,
    ) -> Result<ExternalTriggerCapability> {
        self.default_external_trigger(delivery_mode).await
    }

    pub async fn default_external_trigger(
        &self,
        delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        let agent_id = self.agent_id().await?;
        self.ensure_default_external_ingress(&agent_id, delivery_mode)
            .await
    }

    pub async fn ensure_default_external_ingress(
        &self,
        agent_id: &str,
        delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        let now = Utc::now();
        if let Some(descriptor) =
            self.latest_external_triggers()
                .await?
                .into_iter()
                .find(|descriptor| {
                    descriptor.status == ExternalTriggerStatus::Active
                        && descriptor.scope == ExternalTriggerScope::Agent
                        && descriptor.target_agent_id == agent_id
                        && descriptor.delivery_mode == delivery_mode
                })
        {
            return capability_from_record(&descriptor);
        }

        let external_trigger_id = Uuid::new_v4().to_string();
        let token = generate_callback_token();
        let trigger_url = build_callback_url(&self.inner.callback_base_url, &delivery_mode, &token);
        let descriptor = ExternalTriggerRecord {
            external_trigger_id: external_trigger_id.clone(),
            target_agent_id: agent_id.to_string(),
            waiting_intent_id: None,
            scope: ExternalTriggerScope::Agent,
            delivery_mode: delivery_mode.clone(),
            trigger_url: Some(trigger_url.clone()),
            token_hash: crate::callbacks::hash_callback_token(&token),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        };

        self.inner.storage.append_external_trigger(&descriptor)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "external_trigger_created",
            serde_json::json!({
                "external_trigger_id": descriptor.external_trigger_id,
                "agent_id": agent_id,
                "scope": descriptor.scope,
                "delivery_mode": descriptor.delivery_mode,
            }),
        ))?;

        Ok(ExternalTriggerCapability {
            external_trigger_id,
            trigger_url,
            target_agent_id: agent_id.to_string(),
            delivery_mode,
            status: ExternalTriggerStatus::Active,
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

        let waiting = self.linked_active_waiting_intent(&descriptor).await?;

        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        let correlation_id = payload.correlation_id.clone().or_else(|| {
            waiting
                .as_ref()
                .and_then(|waiting| waiting.correlation_id.clone())
        });
        let causation_id = payload.causation_id.clone().or_else(|| {
            waiting
                .as_ref()
                .and_then(|waiting| waiting.causation_id.clone())
        });

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
                        source: waiting.as_ref().map(|waiting| waiting.source.clone()),
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
                    "waiting_intent_id": waiting.as_ref().map(|waiting| waiting.id.clone()),
                    "external_trigger_id": external_trigger_id,
                    "work_item_id": waiting.as_ref().and_then(|waiting| waiting.work_item_id.clone()),
                    "description": waiting.as_ref().map(|waiting| waiting.description.clone()),
                    "source": waiting.as_ref().map(|waiting| waiting.source.clone()),
                    "scope": descriptor.scope,
                    "resource": waiting.as_ref().and_then(|waiting| waiting.resource.clone()),
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
                        reason: callback_wake_reason(waiting.as_ref(), payload.body.as_ref()),
                        description: waiting.as_ref().map(|waiting| waiting.description.clone()),
                        source: waiting.as_ref().map(|waiting| waiting.source.clone()),
                        scope: Some(descriptor.scope.clone()),
                        waiting_intent_id: waiting.as_ref().map(|waiting| waiting.id.clone()),
                        external_trigger_id: Some(descriptor.external_trigger_id.clone()),
                        resource: waiting
                            .as_ref()
                            .and_then(|waiting| waiting.resource.clone()),
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

        let updated_waiting = if let Some(mut waiting) = waiting {
            if waiting.correlation_id.is_none() {
                waiting.correlation_id = correlation_id.clone();
            }
            if waiting.causation_id.is_none() {
                waiting.causation_id = causation_id.clone();
            }
            waiting.last_triggered_at = Some(now);
            waiting.trigger_count += 1;
            self.inner.storage.append_waiting_intent(&waiting)?;
            Some(waiting)
        } else {
            None
        };

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
                "waiting_intent_id": updated_waiting.as_ref().map(|waiting| waiting.id.clone()),
                "work_item_id": updated_waiting.as_ref().and_then(|waiting| waiting.work_item_id.clone()),
                "external_trigger_id": updated_descriptor_id,
                "scope": updated_descriptor.scope,
                "delivery_mode": updated_descriptor.delivery_mode,
                "source": updated_waiting.as_ref().map(|waiting| waiting.source.clone()),
                "resource": updated_waiting.as_ref().and_then(|waiting| waiting.resource.clone()),
                "trigger_count": updated_waiting.as_ref().map(|waiting| waiting.trigger_count),
                "origin": "callback",
                "delivery_surface": delivery_surface,
                "disposition": disposition,
                "admission_context": crate::types::AdmissionContext::ExternalTriggerCapability,
                "authority_class": crate::types::AuthorityClass::IntegrationSignal,
            }),
        ))?;

        Ok(CallbackDeliveryResult {
            agent_id,
            waiting_intent_id: updated_waiting.map(|waiting| waiting.id),
            external_trigger_id: updated_descriptor.external_trigger_id,
            scope: updated_descriptor.scope,
            delivery_mode: updated_descriptor.delivery_mode,
            disposition,
        })
    }

    pub async fn revoke_external_trigger(
        &self,
        external_trigger_id: &str,
    ) -> Result<ExternalTriggerRecord> {
        let descriptor = self
            .latest_external_triggers()
            .await?
            .into_iter()
            .find(|record| record.external_trigger_id == external_trigger_id)
            .ok_or_else(|| anyhow!("external trigger {} not found", external_trigger_id))?;
        if descriptor.status == ExternalTriggerStatus::Revoked {
            return Ok(descriptor);
        }

        let mut revoked = descriptor;
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(Utc::now());
        self.inner.storage.append_external_trigger(&revoked)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "external_trigger_revoked",
            serde_json::json!({
                "external_trigger_id": revoked.external_trigger_id,
                "agent_id": revoked.target_agent_id,
                "delivery_mode": revoked.delivery_mode,
            }),
        ))?;
        Ok(revoked)
    }

    pub async fn revoke_external_trigger_for_waiting_intent(
        &self,
        waiting_intent_id: &str,
    ) -> Result<ExternalTriggerRecord> {
        let descriptor = self
            .latest_external_triggers()
            .await?
            .into_iter()
            .find(|record| record.waiting_intent_id.as_deref() == Some(waiting_intent_id))
            .ok_or_else(|| {
                anyhow!(
                    "external trigger for waiting intent {} not found",
                    waiting_intent_id
                )
            })?;
        self.revoke_external_trigger(&descriptor.external_trigger_id)
            .await
    }

    async fn linked_active_waiting_intent(
        &self,
        descriptor: &ExternalTriggerRecord,
    ) -> Result<Option<WaitingIntentRecord>> {
        let Some(waiting_intent_id) = descriptor.waiting_intent_id.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .latest_waiting_intents()
            .await?
            .into_iter()
            .find(|record| {
                record.id == waiting_intent_id && record.status == WaitingIntentStatus::Active
            }))
    }
}

fn capability_from_record(descriptor: &ExternalTriggerRecord) -> Result<ExternalTriggerCapability> {
    let trigger_url = descriptor.trigger_url.clone().ok_or_else(|| {
        anyhow!(
            "external trigger {} has no trigger_url",
            descriptor.external_trigger_id
        )
    })?;
    Ok(ExternalTriggerCapability {
        external_trigger_id: descriptor.external_trigger_id.clone(),
        trigger_url,
        target_agent_id: descriptor.target_agent_id.clone(),
        delivery_mode: descriptor.delivery_mode.clone(),
        status: descriptor.status.clone(),
    })
}

fn callback_wake_reason(
    waiting: Option<&WaitingIntentRecord>,
    body: Option<&MessageBody>,
) -> String {
    match body {
        Some(MessageBody::Text { text }) if !text.trim().is_empty() => text.trim().to_string(),
        Some(MessageBody::Json { value }) => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            truncate_activation_text(&rendered)
        }
        Some(MessageBody::Brief { text, .. }) if !text.trim().is_empty() => text.trim().to_string(),
        _ => waiting
            .map(|waiting| format!("external trigger fired: {}", waiting.description))
            .unwrap_or_else(|| "external trigger fired".to_string()),
    }
}

fn truncate_activation_text(text: &str) -> String {
    if text.chars().count() <= 160 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}
