use anyhow::{bail, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
#[cfg(test)]
use tokio::sync::Notify;

use super::RuntimeHandle;
use crate::types::{
    AgentMessageAdmissionEvidence, AgentMessageCallerContext, AgentMessageDeliveryOutcome,
    AgentMessageDeliveryReceipt, AgentMessageDeliveryRecord, AgentMessageDeliveryState,
    AgentMessageSendRequest, MessageEnvelope, MessageKind, Priority,
};

const DELIVERY_IDEMPOTENCY_NAMESPACE: &str = "agent_message_delivery";

#[cfg(test)]
static DELIVERY_CHECKPOINT_ENABLED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static DELIVERY_AT_CHECKPOINT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static DELIVERY_CHECKPOINT_AGENT_ID: Mutex<Option<String>> = Mutex::new(None);
#[cfg(test)]
static DELIVERY_REACHED_CHECKPOINT: Notify = Notify::const_new();
#[cfg(test)]
static DELIVERY_ALLOW_CONTINUE: Notify = Notify::const_new();

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
        let receipt = self
            .runtime
            .enqueue_delivery(prepared.message.clone(), &prepared.record)
            .await?;
        #[cfg(test)]
        wait_at_delivery_checkpoint(&prepared.record.target_agent_id).await;
        Ok(receipt)
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
            activation_id: None,
            turn_id: None,
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
                derived_grant: None,
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

pub(crate) fn isolate_legacy_cross_agent_execution_bindings(
    message: &mut MessageEnvelope,
    delivery: &AgentMessageDeliveryRecord,
) -> bool {
    let Some(caller_agent_id) = delivery.caller.caller_agent_id.as_deref() else {
        return false;
    };
    if caller_agent_id == message.agent_id
        || delivery.target_agent_id != message.agent_id
        || delivery.message_id.as_deref() != Some(message.id.as_str())
        || delivery.outcome != AgentMessageDeliveryOutcome::Accepted
        || delivery.state != AgentMessageDeliveryState::Queued
    {
        return false;
    }

    let caller = &delivery.caller;
    let has_source_binding = caller.current_turn_id.is_some()
        || caller.current_task_id.is_some()
        || caller.current_work_item_id.is_some();
    let bindings_match = message.turn_id == caller.current_turn_id
        && message.task_id == caller.current_task_id
        && message.work_item_id == caller.current_work_item_id;
    if !has_source_binding || !bindings_match {
        return false;
    }

    message.turn_id = None;
    message.task_id = None;
    message.work_item_id = None;
    true
}

#[cfg(test)]
async fn wait_at_delivery_checkpoint(agent_id: &str) {
    if !DELIVERY_CHECKPOINT_ENABLED.load(Ordering::SeqCst)
        || DELIVERY_CHECKPOINT_AGENT_ID.lock().unwrap().as_deref() != Some(agent_id)
    {
        return;
    }
    DELIVERY_AT_CHECKPOINT.store(true, Ordering::SeqCst);
    DELIVERY_REACHED_CHECKPOINT.notify_one();
    DELIVERY_ALLOW_CONTINUE.notified().await;
    DELIVERY_AT_CHECKPOINT.store(false, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn enable_delivery_checkpoint(agent_id: impl Into<String>) {
    *DELIVERY_CHECKPOINT_AGENT_ID.lock().unwrap() = Some(agent_id.into());
    DELIVERY_AT_CHECKPOINT.store(false, Ordering::SeqCst);
    DELIVERY_CHECKPOINT_ENABLED.store(true, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) async fn wait_for_delivery_checkpoint() {
    while !DELIVERY_AT_CHECKPOINT.load(Ordering::SeqCst) {
        DELIVERY_REACHED_CHECKPOINT.notified().await;
    }
}

#[cfg(test)]
pub(crate) fn release_delivery_checkpoint() {
    if !DELIVERY_CHECKPOINT_ENABLED.swap(false, Ordering::SeqCst) {
        return;
    }
    *DELIVERY_CHECKPOINT_AGENT_ID.lock().unwrap() = None;
    DELIVERY_ALLOW_CONTINUE.notify_one();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AdmissionContext, AgentMessagePrincipalKind, AuthorityClass, MessageBody,
        MessageDeliverySurface, MessageOrigin,
    };

    #[test]
    fn dispatched_legacy_delivery_keeps_execution_bindings_for_replay() {
        let caller = AgentMessageCallerContext {
            caller_principal: "runtime:agent-invocation".into(),
            caller_agent_id: Some("caller-agent".into()),
            principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
            route: "agent_invocation".into(),
            origin: MessageOrigin::Task {
                task_id: "task-caller".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: Some("turn-caller".into()),
            current_task_id: Some("task-caller".into()),
            current_work_item_id: Some("work-caller".into()),
        };
        let mut prepared = AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: "target-agent".into(),
                content: MessageBody::Text {
                    text: "legacy interrupted invocation".into(),
                },
                client_idempotency_key: "legacy-dispatched-bindings".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: None,
            },
            caller.clone(),
        )
        .unwrap();
        prepared.message.turn_id.clone_from(&caller.current_turn_id);
        prepared.message.task_id.clone_from(&caller.current_task_id);
        prepared
            .message
            .work_item_id
            .clone_from(&caller.current_work_item_id);
        prepared.record.outcome = AgentMessageDeliveryOutcome::Accepted;
        prepared.record.state = AgentMessageDeliveryState::Dispatched;

        assert!(!isolate_legacy_cross_agent_execution_bindings(
            &mut prepared.message,
            &prepared.record,
        ));
        assert_eq!(prepared.message.turn_id, caller.current_turn_id);
        assert_eq!(prepared.message.task_id, caller.current_task_id);
        assert_eq!(prepared.message.work_item_id, caller.current_work_item_id);
    }
}
