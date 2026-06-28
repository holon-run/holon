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
        _delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        self.ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await
    }

    pub async fn ensure_default_external_ingress(
        &self,
        _delivery_mode: CallbackDeliveryMode,
    ) -> Result<ExternalTriggerCapability> {
        let delivery_mode = CallbackDeliveryMode::WakeHint;
        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        if let Some(descriptor) = self
            .inner
            .runtime_db
            .external_triggers()
            .active_default_for_agent(&agent_id)?
        {
            if descriptor.trigger_url.is_some() {
                return capability_from_record(&descriptor);
            }
            let mut revoked = descriptor;
            revoked.status = ExternalTriggerStatus::Revoked;
            revoked.revoked_at = Some(now);
            self.inner.runtime_db.external_triggers().upsert(&revoked)?;
            self.cache_external_trigger_projection(&revoked).await;
        }

        let external_trigger_id = crate::ids::external_trigger_id();
        let token = generate_callback_token();
        let trigger_url = build_callback_url(&self.inner.callback_base_url, &delivery_mode, &token);
        let descriptor = ExternalTriggerRecord {
            external_trigger_id: external_trigger_id.clone(),
            target_agent_id: agent_id.clone(),
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

        self.inner
            .runtime_db
            .external_triggers()
            .upsert(&descriptor)?;
        self.cache_external_trigger_projection(&descriptor).await;
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
            target_agent_id: agent_id,
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
            .inner
            .runtime_db
            .external_triggers()
            .latest(descriptor_id)?
            .ok_or_else(|| anyhow!("external trigger {} not found", descriptor_id))?;
        if descriptor.status != ExternalTriggerStatus::Active {
            return Err(anyhow!("external trigger is not active"));
        }

        let agent_id = self.agent_id().await?;
        let now = Utc::now();
        let correlation_id = payload.correlation_id.clone();
        let causation_id = payload.causation_id.clone();

        let disposition = {
            let disposition = self
                .submit_wake_hint(WakeHint {
                    agent_id: agent_id.clone(),
                    reason: callback_wake_reason(None, payload.body.as_ref()),
                    description: None,
                    source: None,
                    scope: Some(descriptor.scope.clone()),
                    external_trigger_id: Some(descriptor.external_trigger_id.clone()),
                    resource: None,
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
        };

        let mut updated_descriptor = descriptor;
        updated_descriptor.last_delivered_at = Some(now);
        updated_descriptor.delivery_count += 1;
        self.inner
            .runtime_db
            .external_triggers()
            .upsert(&updated_descriptor)?;
        self.cache_external_trigger_projection(&updated_descriptor)
            .await;

        let updated_descriptor_id = updated_descriptor.external_trigger_id.clone();
        let descriptor_delivery_mode = updated_descriptor.delivery_mode.clone();
        let deprecated_enqueue_message_mapped_to_wake_hint =
            descriptor_delivery_mode == CallbackDeliveryMode::EnqueueMessage;
        self.inner.storage.append_event(&AuditEvent::new(
            "callback_delivered",
            serde_json::json!({
                "agent_id": agent_id,
                "external_trigger_id": updated_descriptor_id,
                "scope": updated_descriptor.scope,
                "delivery_mode": CallbackDeliveryMode::WakeHint,
                "descriptor_delivery_mode": descriptor_delivery_mode,
                "deprecated_enqueue_message_mapped_to_wake_hint": deprecated_enqueue_message_mapped_to_wake_hint,
                "source": serde_json::Value::Null,
                "resource": serde_json::Value::Null,
                "origin": "callback",
                "delivery_surface": crate::types::MessageDeliverySurface::HttpCallbackWake,
                "disposition": disposition,
                "admission_context": crate::types::AdmissionContext::ExternalTriggerCapability,
                "authority_class": crate::types::AuthorityClass::IntegrationSignal,
            }),
        ))?;

        Ok(CallbackDeliveryResult {
            agent_id,
            external_trigger_id: updated_descriptor.external_trigger_id,
            scope: updated_descriptor.scope,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            disposition,
        })
    }

    pub async fn revoke_external_trigger(
        &self,
        external_trigger_id: &str,
    ) -> Result<ExternalTriggerRecord> {
        let descriptor = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(external_trigger_id)?
            .ok_or_else(|| anyhow!("external trigger {} not found", external_trigger_id))?;
        if descriptor.status == ExternalTriggerStatus::Revoked {
            return Ok(descriptor);
        }

        let mut revoked = descriptor;
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(Utc::now());
        self.inner.runtime_db.external_triggers().upsert(&revoked)?;
        self.cache_external_trigger_projection(&revoked).await;
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

fn callback_wake_reason(_waiting: Option<&()>, body: Option<&MessageBody>) -> String {
    match body {
        Some(MessageBody::Text { text }) if !text.trim().is_empty() => text.trim().to_string(),
        Some(MessageBody::Json { value }) => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            truncate_activation_text(&rendered)
        }
        Some(MessageBody::Brief { text, .. }) if !text.trim().is_empty() => text.trim().to_string(),
        _ => "external trigger fired".to_string(),
    }
}

fn truncate_activation_text(text: &str) -> String {
    if text.chars().count() <= 160 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}
