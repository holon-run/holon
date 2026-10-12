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
        let _admission = self.execution_admission_lease()?;
        let delivery_mode = CallbackDeliveryMode::WakeHint;
        let mut mutation = self.begin_mutation().await?;
        let agent_id = mutation.state.id.clone();
        let now = Utc::now();
        let external_trigger_id = crate::ids::external_trigger_id();
        let token = generate_callback_token();
        let mut result = None;
        let commit = mutation
            .commit_transition(crate::ids::audit_event_id(), || {
                let (descriptor, revoked, created) = self
                    .inner
                    .runtime_db
                    .external_triggers()
                    .ensure_default_for_agent(
                        &agent_id,
                        delivery_mode.clone(),
                        now,
                        external_trigger_id,
                        token,
                    )?;
                let mut commit = TransitionCommit {
                    applied: created,
                    ..Default::default()
                };
                if created {
                    commit.effects.external_triggers.extend(revoked);
                    commit.effects.external_triggers.push(descriptor.clone());
                }
                result = Some((descriptor, created));
                Ok(commit)
            })
            .await?;
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let (descriptor, created) = result.expect("external ingress commit has a result");
        if !created {
            return capability_from_record(&self.inner.callback_base_url, &descriptor);
        }
        self.inner.storage.append_event(&AuditEvent::legacy(
            "external_trigger_created",
            serde_json::json!({
                "external_trigger_id": &descriptor.external_trigger_id,
                "agent_id": &agent_id,
                "scope": &descriptor.scope,
                "delivery_mode": &descriptor.delivery_mode,
            }),
        ))?;

        capability_from_record(&self.inner.callback_base_url, &descriptor)
    }

    pub async fn deliver_callback(
        &self,
        descriptor_id: &str,
        payload: CallbackDeliveryPayload,
    ) -> Result<CallbackDeliveryResult> {
        let _admission = self.execution_admission_lease()?;
        let descriptor = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(descriptor_id)?
            .ok_or_else(|| anyhow!("external trigger {} not found", descriptor_id))?;
        if descriptor.status != ExternalTriggerStatus::Active {
            return Err(anyhow!("external trigger is not active"));
        }

        let agent_id = self.agent_state().await?.id;
        anyhow::ensure!(
            descriptor.target_agent_id == agent_id,
            "external trigger does not belong to this agent"
        );
        let now = Utc::now();
        let correlation_id = payload.correlation_id.clone();
        let causation_id = payload.causation_id.clone();

        let disposition = {
            let disposition = self
                .submit_wake_hint(WakeHint {
                    agent_id: agent_id.clone(),
                    reason: callback_wake_reason(payload.body.as_ref()),
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

        // Wake submission owns its own reservation. Reacquire only for accounting
        // and preserve a revocation that completed while wake admission awaited.
        let mut mutation = self.begin_mutation().await?;
        let mut updated_descriptor = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(descriptor_id)?
            .ok_or_else(|| anyhow!("external trigger {} not found", descriptor_id))?;
        anyhow::ensure!(
            updated_descriptor.target_agent_id == mutation.state.id,
            "external trigger does not belong to this agent"
        );
        updated_descriptor.last_delivered_at = Some(now);
        updated_descriptor.delivery_count += 1;
        let commit = mutation
            .commit_transition(crate::ids::audit_event_id(), || {
                self.inner
                    .runtime_db
                    .external_triggers()
                    .upsert(&updated_descriptor)?;
                Ok(TransitionCommit {
                    applied: true,
                    effects: crate::runtime_db::transitions::PostCommitEffects {
                        external_triggers: vec![updated_descriptor.clone()],
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
            .await?;
        drop(mutation);
        self.apply_transition_commit(commit).await;

        let updated_descriptor_id = updated_descriptor.external_trigger_id.clone();
        let descriptor_delivery_mode = updated_descriptor.delivery_mode.clone();
        let deprecated_enqueue_message_mapped_to_wake_hint =
            descriptor_delivery_mode == CallbackDeliveryMode::EnqueueMessage;
        self.inner.storage.append_event(&AuditEvent::legacy(
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
        let mut mutation = self.begin_mutation().await?;
        let descriptor = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(external_trigger_id)?
            .ok_or_else(|| anyhow!("external trigger {} not found", external_trigger_id))?;
        anyhow::ensure!(
            descriptor.target_agent_id == mutation.state.id,
            "external trigger does not belong to this agent"
        );
        if descriptor.status == ExternalTriggerStatus::Revoked {
            return Ok(descriptor);
        }

        let mut revoked = descriptor;
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(Utc::now());
        let commit = mutation
            .commit_transition(crate::ids::audit_event_id(), || {
                self.inner.runtime_db.external_triggers().upsert(&revoked)?;
                Ok(TransitionCommit {
                    applied: true,
                    effects: crate::runtime_db::transitions::PostCommitEffects {
                        external_triggers: vec![revoked.clone()],
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
            .await?;
        drop(mutation);
        self.apply_transition_commit(commit).await;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "external_trigger_revoked",
            serde_json::json!({
                "external_trigger_id": revoked.external_trigger_id,
                "agent_id": revoked.target_agent_id,
                "delivery_mode": revoked.delivery_mode,
            }),
        ))?;
        Ok(revoked)
    }

    /// Atomically revoke the current default external trigger for the agent and
    /// provision a fresh one with a new token. Returns the new capability.
    pub async fn reset_external_trigger(&self) -> Result<ExternalTriggerCapability> {
        let delivery_mode = CallbackDeliveryMode::WakeHint;
        let mut mutation = self.begin_mutation().await?;
        let agent_id = mutation.state.id.clone();
        let now = Utc::now();

        let external_trigger_id = crate::ids::external_trigger_id();
        let token = generate_callback_token();
        let mut result = None;
        let commit = mutation
            .commit_transition(crate::ids::audit_event_id(), || {
                let (descriptor, revoked) = self
                    .inner
                    .runtime_db
                    .external_triggers()
                    .reset_default_for_agent(
                        &agent_id,
                        delivery_mode.clone(),
                        now,
                        external_trigger_id,
                        token,
                    )?;
                let mut commit = TransitionCommit {
                    applied: true,
                    ..Default::default()
                };
                commit.effects.external_triggers.extend(revoked.clone());
                commit.effects.external_triggers.push(descriptor.clone());
                result = Some((descriptor, revoked));
                Ok(commit)
            })
            .await?;
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let (descriptor, revoked) = result.expect("external ingress reset has a result");

        if let Some(revoked) = revoked {
            self.inner.storage.append_event(&AuditEvent::legacy(
                "external_trigger_revoked",
                serde_json::json!({
                    "external_trigger_id": revoked.external_trigger_id,
                    "agent_id": &agent_id,
                    "delivery_mode": revoked.delivery_mode,
                    "reason": "reset_callback",
                }),
            ))?;
        }

        self.inner.storage.append_event(&AuditEvent::legacy(
            "external_trigger_created",
            serde_json::json!({
                "external_trigger_id": &descriptor.external_trigger_id,
                "agent_id": &agent_id,
                "scope": &descriptor.scope,
                "delivery_mode": &delivery_mode,
                "reason": "reset_callback",
            }),
        ))?;

        capability_from_record(&self.inner.callback_base_url, &descriptor)
    }
}

fn capability_from_record(
    callback_base_url: &str,
    descriptor: &ExternalTriggerRecord,
) -> Result<ExternalTriggerCapability> {
    let token = descriptor.token.as_deref().ok_or_else(|| {
        anyhow!(
            "external trigger {} has no token",
            descriptor.external_trigger_id
        )
    })?;
    let trigger_url = build_callback_url(callback_base_url, &descriptor.delivery_mode, token);
    Ok(ExternalTriggerCapability {
        external_trigger_id: descriptor.external_trigger_id.clone(),
        trigger_url,
        target_agent_id: descriptor.target_agent_id.clone(),
        delivery_mode: descriptor.delivery_mode.clone(),
        status: descriptor.status.clone(),
    })
}

fn callback_wake_reason(body: Option<&MessageBody>) -> String {
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

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use crate::runtime::tests::support::context_config;
    use tempfile::tempdir;

    #[tokio::test]
    async fn foreign_external_trigger_rejected_without_wake_or_descriptor_mutation() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let (descriptor, _, _) = runtime
            .inner
            .runtime_db
            .external_triggers()
            .ensure_default_for_agent(
                "foreign",
                CallbackDeliveryMode::WakeHint,
                runtime.now(),
                crate::ids::external_trigger_id(),
                generate_callback_token(),
            )
            .unwrap();
        let previous_state = runtime.agent_state().await.unwrap();
        assert!(runtime
            .revoke_external_trigger(&descriptor.external_trigger_id)
            .await
            .is_err());
        assert!(runtime
            .deliver_callback(
                &descriptor.external_trigger_id,
                CallbackDeliveryPayload {
                    body: None,
                    content_type: None,
                    correlation_id: None,
                    causation_id: None,
                },
            )
            .await
            .is_err());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .external_triggers()
                .latest(&descriptor.external_trigger_id)
                .unwrap()
                .unwrap(),
            descriptor
        );
        assert_eq!(runtime.agent_state().await.unwrap(), previous_state);
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert!(runtime
            .storage()
            .read_recent_messages(10)
            .unwrap()
            .is_empty());
    }
}
