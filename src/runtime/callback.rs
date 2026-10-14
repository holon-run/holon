use super::*;

use crate::callbacks::{build_callback_url, generate_callback_token};
use crate::ingress::WakeHint;
use crate::runtime_db::transitions::{
    ExternalTriggerMutationOutcome, PreparedExternalTriggerTransition, PreparedInputTransition,
};

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
        let mut mutation = self.begin_mutation().await?;
        let agent_id = mutation.state.id.clone();
        let now = Utc::now();
        let external_trigger_id = crate::ids::external_trigger_id();
        let token = generate_callback_token();
        let request = mutation.scheduling_writer_request()?;
        let observed = self
            .inner
            .runtime_db
            .external_triggers()
            .observe_default_for_writer(
                &mutation.input_evidence_scope(),
                &request,
                &self.inner.input_evidence_budget,
            )?;
        let command = PreparedExternalTriggerTransition::ensure(
            observed,
            external_trigger_id,
            token,
            now,
            None,
        );
        let prepared = PreparedInputTransition::external_trigger(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            now,
            command,
        )?;
        let (commit, outcome) = mutation
            .commit_prepared_external_trigger_transition(prepared)
            .await?;
        let _ack_material = commit.effects.scheduling_material_budget_views.clone();
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let ExternalTriggerMutationOutcome::Ensured {
            record: descriptor,
            created,
            ..
        } = outcome
        else {
            anyhow::bail!("ensure ingress returned another original outcome");
        };
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
        let (descriptor_scope, descriptor_id, agent_id) = {
            // Freeze original qualified admission while holding its owner; the
            // wake phase acquires its own reservation after this block ends.
            let mutation = self.begin_mutation().await?;
            let request = mutation.scheduling_writer_request()?;
            let observed = self
                .inner
                .runtime_db
                .external_triggers()
                .observe_active_for_admission(
                    &mutation.input_evidence_scope(),
                    descriptor_id,
                    &request,
                    &self.inner.input_evidence_budget,
                )?;
            let descriptor = observed
                .record()
                .ok_or_else(|| anyhow!("external trigger {} not found", descriptor_id))?;
            anyhow::ensure!(
                descriptor.status == ExternalTriggerStatus::Active,
                "external trigger is not active"
            );
            anyhow::ensure!(
                descriptor.target_agent_id == mutation.state.id,
                "external trigger does not belong to this agent"
            );
            (
                descriptor.scope.clone(),
                descriptor.external_trigger_id.clone(),
                mutation.state.id.clone(),
            )
        };
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
                    scope: Some(descriptor_scope),
                    external_trigger_id: Some(descriptor_id.clone()),
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
        let request = mutation.scheduling_writer_request()?;
        let observed = self
            .inner
            .runtime_db
            .external_triggers()
            .observe_existing_for_cleanup(
                &mutation.input_evidence_scope(),
                &descriptor_id,
                &request,
                &self.inner.input_evidence_budget,
            )?;
        let command = PreparedExternalTriggerTransition::account(observed, now, disposition, None)?;
        let prepared = PreparedInputTransition::external_trigger(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            now,
            command,
        )?;
        let (commit, outcome) = mutation
            .commit_prepared_external_trigger_transition(prepared)
            .await?;
        let _ack_material = commit.effects.scheduling_material_budget_views.clone();
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let ExternalTriggerMutationOutcome::Accounted {
            record: updated_descriptor,
            disposition,
        } = outcome
        else {
            anyhow::bail!("callback accounting returned another original outcome");
        };

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
        let request = mutation.scheduling_writer_request()?;
        let observed = self
            .inner
            .runtime_db
            .external_triggers()
            .observe_existing_for_cleanup(
                &mutation.input_evidence_scope(),
                external_trigger_id,
                &request,
                &self.inner.input_evidence_budget,
            )?;
        let now = Utc::now();
        let command = PreparedExternalTriggerTransition::revoke(observed, now, None)?;
        let prepared = PreparedInputTransition::external_trigger(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            now,
            command,
        )?;
        let (commit, outcome) = mutation
            .commit_prepared_external_trigger_transition(prepared)
            .await?;
        let _ack_material = commit.effects.scheduling_material_budget_views.clone();
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let ExternalTriggerMutationOutcome::Revoked {
            record: revoked,
            changed,
        } = outcome
        else {
            anyhow::bail!("revoke ingress returned another original outcome");
        };
        if !changed {
            return Ok(revoked);
        }
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
        let request = mutation.scheduling_writer_request()?;
        let observed = self
            .inner
            .runtime_db
            .external_triggers()
            .observe_default_for_writer(
                &mutation.input_evidence_scope(),
                &request,
                &self.inner.input_evidence_budget,
            )?;
        let command = PreparedExternalTriggerTransition::reset(
            observed,
            external_trigger_id,
            token,
            now,
            None,
        );
        let prepared = PreparedInputTransition::external_trigger(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            now,
            command,
        )?;
        let (commit, outcome) = mutation
            .commit_prepared_external_trigger_transition(prepared)
            .await?;
        let _ack_material = commit.effects.scheduling_material_budget_views.clone();
        drop(mutation);
        self.apply_transition_commit(commit).await;
        let ExternalTriggerMutationOutcome::Reset {
            record: descriptor,
            revoked,
        } = outcome
        else {
            anyhow::bail!("reset ingress returned another original outcome");
        };

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
    #[tokio::test]
    async fn actual_trigger_producers_ack_scheduling_without_new_dispatch_or_message() -> Result<()>
    {
        let home = tempdir()?;
        let workspace = tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )?;
        let before = runtime.agent_state().await?;
        let first = runtime
            .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
        let noop = runtime
            .ensure_default_external_ingress(CallbackDeliveryMode::EnqueueMessage)
            .await?;
        assert_eq!(first, noop);
        let reset = runtime.reset_external_trigger().await?;
        assert_ne!(first.external_trigger_id, reset.external_trigger_id);
        assert_ne!(first.trigger_url, reset.trigger_url);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .external_triggers()
                .latest(&first.external_trigger_id)?
                .unwrap()
                .status,
            ExternalTriggerStatus::Revoked
        );
        let revoked = runtime
            .revoke_external_trigger(&reset.external_trigger_id)
            .await?;
        assert_eq!(revoked.status, ExternalTriggerStatus::Revoked);
        let after = runtime.agent_state().await?;
        assert_eq!(before.current_run_id, after.current_run_id);
        assert_eq!(before.current_turn_id, after.current_turn_id);
        assert_eq!(
            before.current_execution_binding,
            after.current_execution_binding
        );
        assert_eq!(before.turn_index, after.turn_index);
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert!(runtime.storage().read_recent_messages(10)?.is_empty());
        let (receipts, starts, aliases): (usize, usize, usize) =
            runtime.inner.runtime_db.transaction(|tx| {
                Ok(tx.query_row(
            "SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE family='external_trigger'),
                (SELECT COUNT(*) FROM dispatch_memory_checkpoints),
                (SELECT COUNT(*) FROM scheduling_domain_records WHERE family='external_trigger')",
            [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)
            })?;
        assert_eq!(receipts, 4);
        assert_eq!(starts, 0);
        assert_eq!(aliases, 2);
        Ok(())
    }
}
