use anyhow::{bail, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::RuntimeHandle;
use crate::types::{
    AgentMessageAdmissionEvidence, AgentMessageCallerContext, AgentMessageDeliveryOutcome,
    AgentMessageDeliveryReceipt, AgentMessageDeliveryRecord, AgentMessageDeliveryState,
    AgentMessageSendRequest, MessageEnvelope, MessageKind, Priority,
};

const DELIVERY_IDEMPOTENCY_NAMESPACE: &str = "agent_message_delivery";

pub(crate) struct AgentMessageDeliveryService<'a> {
    runtime: &'a RuntimeHandle,
}

pub(crate) struct PreparedAgentMessageDelivery {
    pub(crate) message: MessageEnvelope,
    pub(crate) record: AgentMessageDeliveryRecord,
}

impl RuntimeHandle {
    pub(crate) fn agent_message_delivery_service(&self) -> AgentMessageDeliveryService<'_> {
        AgentMessageDeliveryService { runtime: self }
    }
}

impl AgentMessageDeliveryService<'_> {
    pub(crate) async fn deliver(
        &self,
        prepared: &PreparedAgentMessageDelivery,
    ) -> Result<AgentMessageDeliveryReceipt> {
        self.runtime
            .enqueue_delivery(prepared.message.clone(), &prepared.record)
            .await
    }

    pub(crate) fn prepare(
        request: AgentMessageSendRequest,
        caller: AgentMessageCallerContext,
    ) -> Result<PreparedAgentMessageDelivery> {
        if request.target_agent_id.trim().is_empty() {
            bail!("agent message delivery requires a target agent");
        }
        if request.client_idempotency_key.trim().is_empty() {
            bail!("agent message delivery requires an idempotency key");
        }

        let priority = request
            .requested_priority
            .clone()
            .unwrap_or(Priority::Normal);
        let idempotency_scope = format!(
            "{DELIVERY_IDEMPOTENCY_NAMESPACE}:{}:{}",
            caller.caller_principal, request.target_agent_id
        );
        let idempotency_key_digest = digest(
            b"holon.agent-message-delivery.idempotency-key.v1",
            &request.client_idempotency_key,
        )?;
        let request_digest = digest(
            b"holon.agent-message-delivery.request.v1",
            &DeliverySemanticRequest {
                target_agent_id: &request.target_agent_id,
                content: &request.content,
                correlation_id: request.correlation_id.as_deref(),
                causation_id: request.causation_id.as_deref(),
                priority: &priority,
                caller: &caller,
            },
        )?;
        let delivery_id =
            crate::ids::agent_message_delivery_id(&idempotency_scope, &idempotency_key_digest);
        let now = Utc::now();
        let mut message = MessageEnvelope::new(
            request.target_agent_id.clone(),
            MessageKind::InternalFollowup,
            caller.origin.clone(),
            caller.authority_class,
            priority,
            request.content,
        )
        .with_admission(caller.delivery_surface, caller.admission_context);
        message.turn_id.clone_from(&caller.current_turn_id);
        message.task_id.clone_from(&caller.current_task_id);
        message
            .work_item_id
            .clone_from(&caller.current_work_item_id);
        message.correlation_id.clone_from(&request.correlation_id);
        message.causation_id.clone_from(&request.causation_id);
        message.metadata = Some(serde_json::json!({
            "agent_message_delivery": {
                "delivery_id": delivery_id,
                "caller_principal": caller.caller_principal,
                "caller_agent_id": caller.caller_agent_id,
                "principal_kind": caller.principal_kind,
                "route": caller.route,
            }
        }));
        let principal_id = caller
            .caller_agent_id
            .clone()
            .unwrap_or_else(|| caller.caller_principal.clone());
        let record = AgentMessageDeliveryRecord {
            delivery_id,
            target_agent_id: request.target_agent_id,
            message_id: Some(message.id.clone()),
            correlation_id: request.correlation_id,
            causation_id: request.causation_id,
            idempotency_scope,
            idempotency_key_digest,
            request_digest,
            caller: caller.clone(),
            outcome: AgentMessageDeliveryOutcome::Rejected,
            state: AgentMessageDeliveryState::Rejected,
            state_version: 1,
            admission_evidence: AgentMessageAdmissionEvidence {
                identity_revision: None,
                identity_status: None,
                runtime_status: None,
                message_policy_revision: None,
                principal_kind: caller.principal_kind,
                principal_id: Some(principal_id),
                route: caller.route,
                matched_rule_index: None,
            },
            rejection_code: None,
            retryable: false,
            diagnostic: None,
            accepted_at: None,
            terminal_at: None,
            created_at: now,
            updated_at: now,
        };
        Ok(PreparedAgentMessageDelivery { message, record })
    }
}

#[derive(Serialize)]
struct DeliverySemanticRequest<'a> {
    target_agent_id: &'a str,
    content: &'a crate::types::MessageBody,
    correlation_id: Option<&'a str>,
    causation_id: Option<&'a str>,
    priority: &'a Priority,
    caller: &'a AgentMessageCallerContext,
}

fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(b"\0");
    hasher.update(serde_json::to_vec(value)?);
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
