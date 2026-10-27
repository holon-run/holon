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
    AgentMessageRoutingContext, AgentMessageSendRequest, MessageEnvelope, MessageKind, Priority,
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
        mut caller: AgentMessageCallerContext,
    ) -> Result<PreparedAgentMessageDelivery> {
        if request.target_agent_id.trim().is_empty() {
            bail!("agent message delivery requires a target agent");
        }
        if request.client_idempotency_key.trim().is_empty() {
            bail!("agent message delivery requires an idempotency key");
        }
        if let Some(selector) = request.reply_context_id.as_deref() {
            if selector.is_empty()
                || caller
                    .inherited_routing_context
                    .as_ref()
                    .is_none_or(|route| route.message_id != selector)
                || caller.routing_activation_id.is_none()
            {
                bail!("reply_context_id must select a live admitted agent message");
            }
        }

        let inherited_routing_context = caller.inherited_routing_context.clone().filter(|route| {
            caller.caller_agent_id.as_deref() == Some(route.recipient_agent_id.as_str())
                && (request.forward
                    || route.reply_to_agent_id.as_deref() == Some(request.target_agent_id.as_str())
                    || route.sender_agent_id.as_deref() == Some(request.target_agent_id.as_str()))
        });
        if request.forward && inherited_routing_context.is_none() {
            bail!("forwarding requires a unique or explicitly selected admitted agent message");
        }
        if caller.routing_activation_id.is_none() {
            caller
                .inherited_routing_context
                .clone_from(&inherited_routing_context);
        }
        let priority = request
            .requested_priority
            .clone()
            .unwrap_or(Priority::Normal);
        let inherited = inherited_routing_context.as_ref();
        let forwarded = inherited.filter(|route| {
            request.forward
                && route.reply_to_agent_id.as_deref() != Some(request.target_agent_id.as_str())
                && route.sender_agent_id.as_deref() != Some(request.target_agent_id.as_str())
        });
        let correlation_id = request
            .correlation_id
            .clone()
            .or_else(|| inherited.and_then(|route| route.correlation_id.clone()));
        let causation_id = request
            .causation_id
            .clone()
            .or_else(|| inherited.map(|route| route.message_id.clone()));
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
                correlation_id: correlation_id.as_deref(),
                causation_id: causation_id.as_deref(),
                priority: &priority,
                forward: request.forward,
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
        message.correlation_id.clone_from(&correlation_id);
        message.causation_id.clone_from(&causation_id);
        let sender_agent_id = caller.caller_agent_id.clone();
        message.routing_context = Some(AgentMessageRoutingContext {
            message_id: message.id.clone(),
            sender_agent_id: sender_agent_id.clone(),
            recipient_agent_id: request.target_agent_id.clone(),
            reply_to_agent_id: forwarded
                .and_then(|route| route.reply_to_agent_id.clone())
                .or_else(|| sender_agent_id.clone()),
            correlation_id: correlation_id.clone(),
            in_reply_to_message_id: inherited.map(|route| route.message_id.clone()),
            original_sender_agent_id: forwarded
                .and_then(|route| route.original_sender_agent_id.clone())
                .or_else(|| forwarded.and_then(|route| route.sender_agent_id.clone())),
            original_reply_to_agent_id: forwarded
                .and_then(|route| route.original_reply_to_agent_id.clone())
                .or_else(|| forwarded.and_then(|route| route.reply_to_agent_id.clone())),
        });
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
            expected_incarnation: None,
            message_id: Some(message.id.clone()),
            activation_id: None,
            turn_id: None,
            correlation_id,
            causation_id,
            routing_context: message.routing_context.clone(),
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
    let released = DELIVERY_ALLOW_CONTINUE.notified();
    tokio::pin!(released);
    released.as_mut().enable();
    DELIVERY_AT_CHECKPOINT.store(true, Ordering::SeqCst);
    DELIVERY_REACHED_CHECKPOINT.notify_one();
    released.await;
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
    // A recovery test may release without any delivery reaching the checkpoint.
    // Wake registered arrivals without leaving a permit for the next test.
    DELIVERY_ALLOW_CONTINUE.notify_waiters();
}

#[derive(Serialize)]
struct DeliverySemanticRequest<'a> {
    target_agent_id: &'a str,
    content: &'a crate::types::MessageBody,
    correlation_id: Option<&'a str>,
    causation_id: Option<&'a str>,
    priority: &'a Priority,
    forward: bool,
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

    fn caller(
        inherited_routing_context: Option<AgentMessageRoutingContext>,
    ) -> AgentMessageCallerContext {
        AgentMessageCallerContext {
            caller_principal: "runtime:agent-message".into(),
            caller_agent_id: Some("caller-agent".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: Some("turn-caller".into()),
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context,
            routing_activation_id: None,
        }
    }

    fn request(target_agent_id: &str, key: &str) -> AgentMessageSendRequest {
        AgentMessageSendRequest {
            target_agent_id: target_agent_id.into(),
            content: MessageBody::Text {
                text: "message body".into(),
            },
            client_idempotency_key: key.into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: None,
            forward: false,
            reply_context_id: None,
        }
    }

    async fn start_reply_selector_turn(runtime: &RuntimeHandle) -> Result<MessageEnvelope> {
        use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
        let RunLoopPoll::Message(scheduled) =
            SchedulerDecisionExecutor::new(runtime).poll().await?
        else {
            bail!("expected scheduled agent message");
        };
        runtime
            .begin_interactive_turn_with_provenance(
                Some(&scheduled.message),
                None,
                None,
                scheduled.dispatch_plan.execution_admission_provenance,
            )
            .await?;
        runtime.record_incoming_transcript_entry(&scheduled.message)?;
        Ok(scheduled.message)
    }

    #[tokio::test]
    async fn issue_3484_real_delivery_survives_two_recoveries_and_processes_new_input() -> Result<()>
    {
        use crate::domain::execution_protocol::ExecutionAttemptState;
        use crate::provider::{
            AgentProvider, ConversationMessage, ProviderTurnRequest, ProviderTurnResponse,
        };
        use crate::runtime::tests::support::{
            context_config, host_backed_test_runtime, CountingProvider,
        };
        use crate::types::{BriefKind, QueueEntryStatus};
        use anyhow::Context;
        use std::sync::Arc;
        use std::time::Duration;

        struct DeliveryCaptureProvider {
            inner: CountingProvider,
            requests: tokio::sync::Mutex<Vec<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl AgentProvider for DeliveryCaptureProvider {
            async fn complete_turn(
                &self,
                request: ProviderTurnRequest,
            ) -> Result<ProviderTurnResponse> {
                let current_inputs = request
                    .conversation
                    .iter()
                    .filter_map(|message| match message {
                        ConversationMessage::UserBlocks(blocks) => Some(blocks),
                        _ => None,
                    })
                    .flatten()
                    .filter(|block| block.text.starts_with("## current_input\n"))
                    .map(|block| block.text.clone())
                    .collect();
                self.requests.lock().await.push(current_inputs);
                self.inner.complete_turn(request).await
            }
        }

        let (home, _host, sender) = host_backed_test_runtime().await;
        let sender_id = sender.agent_id().await?;
        let workspace = home.path().join("settlement-acceptance-workspace");
        std::fs::create_dir_all(&workspace)?;
        let state_root = home.path().join("settlement-acceptance-state");
        let provider = Arc::new(DeliveryCaptureProvider {
            inner: CountingProvider {
                calls: tokio::sync::Mutex::new(0),
                reply: "delivery completed",
            },
            requests: tokio::sync::Mutex::new(Vec::new()),
        });
        let new_target = || {
            RuntimeHandle::new(
                "settlement-acceptance-target",
                state_root.clone(),
                workspace.clone(),
                "http://127.0.0.1:7878".into(),
                provider.clone(),
                "fixture".into(),
                context_config(),
            )
        };
        let prepare = |key: &str| {
            let mut source = caller(None);
            source.caller_principal = format!("agent:{sender_id}");
            source.caller_agent_id = Some(sender_id.clone());
            source.current_turn_id = None;
            let mut input = request("settlement-acceptance-target", key);
            input.content = MessageBody::Text {
                text: format!("issue-3484-{key}-delivery-marker"),
            };
            input.requested_priority = Some(Priority::Normal);
            AgentMessageDeliveryService::prepare(input, source)
        };
        let mut target = new_target()?;
        let mut ids = Vec::new();
        for key in ["anchor", "follower"] {
            ids.push(
                target
                    .agent_message_delivery_service()
                    .deliver(&prepare(key)?)
                    .await?
                    .message_id
                    .context("accepted delivery")?,
            );
        }
        let originals = ids
            .iter()
            .map(|id| {
                target
                    .storage()
                    .read_message_by_id(id)?
                    .context("source evidence")
            })
            .collect::<Result<Vec<_>>>()?;
        assert!(originals.iter().all(|message| message.turn_id.is_some()));
        let mut predecessor = None;
        for interrupted_claim in 0..2 {
            if interrupted_claim > 0 {
                drop(target);
                target = new_target()?;
                target.recover_scheduler_bootstrap_claims().await?;
            }
            let anchor = start_reply_selector_turn(&target).await?;
            assert_eq!(anchor.id, ids[0]);
            assert!(target.attach_idle_sender_inputs(&anchor, 24 * 1024).await?);
            let execution = target
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("settlement-acceptance-target")?
                .context("canonical execution")?;
            let delivery = target
                .inner
                .runtime_db
                .agent_message_deliveries()
                .latest_for_message(&ids[0])?
                .context("bound anchor delivery")?;
            let activation = delivery.activation_id.context("bound activation")?;
            let attempt = &execution.attempts[&activation];
            assert_eq!(attempt.state, ExecutionAttemptState::Open);
            assert_eq!(attempt.recovery_of_attempt_id, predecessor);
            assert_eq!(attempt.turn_id, originals[0].turn_id);
            assert_eq!(delivery.turn_id, attempt.turn_id);
            assert_eq!(attempt.source_message_id.as_ref(), Some(&ids[0]));
            if let Some(previous) = &predecessor {
                assert_eq!(
                    execution.attempts[previous].state,
                    ExecutionAttemptState::Interrupted
                );
            }
            predecessor = Some(activation);
            assert_eq!(provider.inner.call_count().await, 0);
        }
        drop(target);
        target = new_target()?;
        target.recover_scheduler_bootstrap_claims().await?;

        // Only ordinary runtime recovery and real producer ingress are used;
        // no durable message, attempt, or delivery evidence is rewritten.
        let mut runner = tokio::spawn(target.clone().run());
        let delivered = tokio::time::timeout(Duration::from_secs(10), async {
            let mut processed_ids = vec![ids[0].clone()];
            for key in [None, Some("after-recovery")] {
                if let Some(key) = key {
                    processed_ids.push(
                        target
                            .agent_message_delivery_service()
                            .deliver(&prepare(key)?)
                            .await?
                            .message_id
                            .context("accepted subsequent input")?,
                    );
                }
                let id = processed_ids.last().unwrap();
                loop {
                    assert!(
                        !runner.is_finished(),
                        "runtime exited before delivery: {:?}",
                        (&mut runner).await
                    );
                    let processed = target
                        .inner
                        .runtime_db
                        .queue_entries()
                        .latest(id)?
                        .is_some_and(|entry| entry.status == QueueEntryStatus::Processed);
                    let published = target
                        .storage()
                        .read_recent_briefs(10)?
                        .iter()
                        .any(|brief| brief.related_message_id.as_deref() == Some(id.as_str()));
                    if processed && published {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            Ok::<_, anyhow::Error>(processed_ids)
        })
        .await;
        runner.abort();
        let _ = runner.await;
        let processed_ids = delivered.context("recovery must not strand later input")??;
        assert_eq!(provider.inner.call_count().await, 2);
        {
            let requests = provider.requests.lock().await;
            assert_eq!(requests.len(), 2);
            for current_inputs in requests.iter() {
                assert_eq!(
                    current_inputs.len(),
                    1,
                    "current input block must be unique"
                );
            }
            let recovered_input = &requests[0][0];
            for key in ["anchor", "follower"] {
                assert_eq!(
                    recovered_input
                        .matches(&format!("issue-3484-{key}-delivery-marker"))
                        .count(),
                    1,
                    "{key} body must reach the provider as current input exactly once"
                );
            }
            for id in &ids {
                assert_eq!(
                    recovered_input
                        .matches(&format!("  source_ref: message:{id}\n"))
                        .count(),
                    1,
                    "recovered source must be admitted exactly once"
                );
            }
            assert!(!recovered_input.contains("issue-3484-after-recovery-delivery-marker"));
            let later_input = &requests[1][0];
            assert_eq!(
                later_input
                    .matches("issue-3484-after-recovery-delivery-marker")
                    .count(),
                1
            );
            assert_eq!(
                later_input
                    .matches(&format!("  source_ref: message:{}\n", processed_ids[1]))
                    .count(),
                1
            );
        }
        assert_eq!(target.agent_state().await?.pending, 0);
        assert!(target.storage().read_recent_tool_executions(10)?.is_empty());
        let execution = target
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("settlement-acceptance-target")?
            .context("settled execution")?;
        let deliveries = target.inner.runtime_db.agent_message_deliveries();
        let anchor = deliveries.latest_for_message(&ids[0])?.unwrap();
        let final_attempt = &execution.attempts[anchor.activation_id.as_ref().unwrap()];
        assert_eq!(final_attempt.recovery_of_attempt_id, predecessor);
        assert_eq!(
            execution.attempts[predecessor.as_ref().unwrap()].state,
            ExecutionAttemptState::Interrupted
        );
        assert_eq!(final_attempt.turn_id, originals[0].turn_id);
        assert_eq!(execution.attempts.len(), 4);
        for (id, original) in ids.iter().zip(&originals) {
            assert_eq!(
                serde_json::to_value(target.storage().read_message_by_id(id)?.unwrap())?,
                serde_json::to_value(original)?
            );
            assert_eq!(
                deliveries.latest_for_message(id)?.unwrap().state,
                AgentMessageDeliveryState::Consumed
            );
        }
        let follower = deliveries.latest_for_message(&ids[1])?.unwrap();
        assert!(follower.activation_id.is_none());
        assert!(follower.turn_id.is_none());
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(&ids[1])?
                .unwrap()
                .status,
            QueueEntryStatus::Interjected
        );
        assert!(!execution
            .attempts
            .values()
            .any(|attempt| { attempt.source_message_id.as_ref() == Some(&ids[1]) }));
        let briefs = target.storage().read_recent_briefs(10)?;
        assert_eq!(briefs.len(), 2);
        let turns = target.storage().read_recent_turns(10)?;
        for id in processed_ids {
            let brief = briefs
                .iter()
                .find(|brief| brief.related_message_id.as_deref() == Some(id.as_str()))
                .context("Result brief for processed input")?;
            assert_eq!(brief.kind, BriefKind::Result);
            let turn = turns
                .iter()
                .find(|turn| {
                    turn.input_message_ids.contains(&id)
                        && turn.terminal.as_ref().is_some_and(|terminal| {
                            terminal.kind == crate::types::TurnTerminalKind::Completed
                        })
                })
                .context("Completed turn for processed input")?;
            if id == ids[0] {
                assert!(turn.input_message_ids.contains(&ids[1]));
            }
            assert_eq!(turn.produced_brief_ids, vec![brief.id.clone()]);
        }
        Ok(())
    }

    #[tokio::test]
    async fn operator_authorized_agent_message_anchor_can_reply() -> Result<()> {
        real_agent_message_can_reply(false).await
    }

    #[tokio::test]
    async fn runtime_authorized_agent_message_live_follower_can_reply() -> Result<()> {
        real_agent_message_can_reply(true).await
    }

    async fn real_agent_message_can_reply(live_follower: bool) -> Result<()> {
        use crate::runtime::tests::support::{
            tempdir, AgentProvider, AppConfig, ProviderTurnRequest, ProviderTurnResponse,
            RuntimeHost,
        };
        use crate::types::{CreateAgentRequest, QueueEntryStatus};
        use anyhow::Context;
        use std::sync::Arc;
        use std::time::Duration;

        struct ReplySelectorProvider {
            started: Notify,
        }

        #[async_trait::async_trait]
        impl AgentProvider for ReplySelectorProvider {
            async fn complete_turn(
                &self,
                _request: ProviderTurnRequest,
            ) -> Result<ProviderTurnResponse> {
                self.started.notify_one();
                std::future::pending().await
            }
        }

        let home = tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        let provider = Arc::new(ReplySelectorProvider {
            started: Notify::new(),
        });
        let host = RuntimeHost::new_with_provider(config, provider.clone())?;
        let sender = host.default_runtime().await?;
        let sender_id = sender.agent_id().await?;
        sender
            .enqueue(
                MessageEnvelope::new(
                    sender_id.clone(),
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: Some("reply-test-operator".into()),
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "contact the peer agent".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::HttpControlPrompt,
                    AdmissionContext::ControlAuthenticated,
                ),
            )
            .await?;
        // The host owns scheduling; park each admitted turn in the provider
        // instead of racing its run loop with a second scheduler poll.
        tokio::time::timeout(Duration::from_secs(2), provider.started.notified()).await?;
        let sender_state = sender.agent_state().await?;
        let operator_input = sender
            .storage()
            .read_message_by_id(
                &sender_state
                    .current_execution_binding
                    .context("admitted operator turn")?
                    .source_message_id,
            )?
            .context("durable operator input")?;
        sender
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "reply-peer".into(),
                name: None,
                template: None,
                initial_message: None,
                authority_class: operator_input.authority_class.clone(),
                model_resolution: None,
                lineage_parent_agent_id: None,
                inherit_parent_runtime: false,
            })
            .await?;
        let target = host.get_or_create_agent("reply-peer").await?;
        let first = sender
            .agent_messaging_service()
            .send(
                request("reply-peer", "anchor"),
                if live_follower {
                    AuthorityClass::RuntimeInstruction
                } else {
                    operator_input.authority_class.clone()
                },
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(2), provider.started.notified()).await?;
        let target_state = target.agent_state().await?;
        let anchor = target
            .storage()
            .read_message_by_id(
                &target_state
                    .current_execution_binding
                    .as_ref()
                    .context("admitted agent message")?
                    .source_message_id,
            )?
            .context("durable anchor")?;
        assert_eq!(first.message_id.as_deref(), Some(anchor.id.as_str()));
        let selected_id = if live_follower {
            let turn = target
                .storage()
                .read_turn_by_id(
                    target_state
                        .current_turn_id
                        .as_deref()
                        .context("live turn")?,
                )?
                .context("admitted turn")?;
            assert_eq!(
                turn.idle_input_batch
                    .context("persisted anchor admission")?
                    .anchor_message_id,
                anchor.id
            );
            let queued = sender
                .agent_messaging_service()
                .send(
                    request("reply-peer", "operator-follower"),
                    operator_input.authority_class,
                )
                .await?;
            let queued_id = queued.message_id.context("accepted operator input")?;
            assert!(target
                .attach_live_inputs("reply-peer", 2, 24 * 1024, false)
                .await?
                .is_empty());
            assert_eq!(
                target
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&queued_id)?
                    .unwrap()
                    .status,
                QueueEntryStatus::Queued
            );
            let follower = sender
                .agent_messaging_service()
                .send(
                    request("reply-peer", "live-follower"),
                    AuthorityClass::RuntimeInstruction,
                )
                .await?;
            let id = follower.message_id.context("accepted live input")?;
            assert!(!target
                .attach_live_inputs("reply-peer", 2, 24 * 1024, false)
                .await?
                .is_empty());
            assert_eq!(
                target
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&id)?
                    .unwrap()
                    .status,
                QueueEntryStatus::Interjected
            );
            id
        } else {
            anchor.id
        };
        let input = target.storage().read_message_by_id(&selected_id)?.unwrap();
        let expected_authority = if live_follower {
            AuthorityClass::RuntimeInstruction
        } else {
            AuthorityClass::OperatorInstruction
        };
        assert_eq!(input.authority_class, expected_authority);
        assert_eq!(
            input.admission_context,
            Some(AdmissionContext::RuntimeOwned)
        );
        assert_eq!(input.kind, MessageKind::InternalFollowup);
        let mut reply = request(&sender_id, "reply");
        reply.reply_context_id = Some(selected_id.clone());
        let receipt = target
            .agent_messaging_service()
            .send(reply, input.authority_class)
            .await?;
        assert_eq!(receipt.outcome, AgentMessageDeliveryOutcome::Accepted);
        let reply = sender
            .storage()
            .read_message_by_id(receipt.message_id.as_deref().context("accepted reply")?)?
            .context("durable reply")?;
        let route = reply.routing_context.context("reply routing context")?;
        assert_eq!(route.sender_agent_id.as_deref(), Some("reply-peer"));
        assert_eq!(route.recipient_agent_id, sender_id);
        assert_eq!(route.in_reply_to_message_id, Some(selected_id.clone()));
        assert_eq!(reply.causation_id, Some(selected_id));
        assert_eq!(reply.authority_class, expected_authority);
        host.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn reply_selector_routes_real_p1_followers_and_their_interrupted_replay() -> Result<()> {
        real_p1_reply_selector_followers_and_interrupted_replay(false).await
    }

    #[tokio::test]
    async fn reply_selector_routes_real_p1_null_ingress_followers_and_interrupted_replay(
    ) -> Result<()> {
        real_p1_reply_selector_followers_and_interrupted_replay(true).await
    }

    async fn real_p1_reply_selector_followers_and_interrupted_replay(
        null_ingress_turn: bool,
    ) -> Result<()> {
        use crate::runtime::tests::support::{
            context_config, host_backed_test_runtime, TimelineProvider,
        };
        use crate::types::QueueEntryStatus;
        use anyhow::Context;
        use std::sync::Arc;

        let (home, _host, sender) = host_backed_test_runtime().await;
        let sender_id = sender.agent_id().await?;
        let workspace = home.path().join("reply-selector-workspace");
        std::fs::create_dir_all(&workspace)?;
        let state_root = home.path().join("reply-selector-state");
        let new_target = || {
            RuntimeHandle::new(
                "reply-selector-target",
                state_root.clone(),
                workspace.clone(),
                "http://127.0.0.1:7878".into(),
                Arc::new(TimelineProvider),
                "fixture".into(),
                context_config(),
            )
        };
        let mut target = new_target()?;
        let mut ids = Vec::new();
        for index in 0..2 {
            let mut source = caller(None);
            source.caller_principal = format!("agent:{sender_id}");
            source.caller_agent_id = Some(sender_id.clone());
            source.current_turn_id = None;
            let mut input = request("reply-selector-target", &format!("input-{index}"));
            input.requested_priority = Some(Priority::Normal);
            input.correlation_id = Some(format!("correlation-{index}"));
            let prepared = AgentMessageDeliveryService::prepare(input, source)?;
            ids.push(
                target
                    .agent_message_delivery_service()
                    .deliver(&prepared)
                    .await?
                    .message_id
                    .context("accepted message")?,
            );
        }
        let original_follower = target
            .storage()
            .read_message_by_id(&ids[1])?
            .context("durable follower")?;
        assert!(original_follower.turn_id.is_some());
        if null_ingress_turn {
            // Restore a persisted pre-assignment input before real admission;
            // never assign or rewrite the canonical attempt's turn.
            let mut guard = target.inner.agent.lock().await;
            guard
                .queue
                .pop_next_matching(|message| message.id == ids[0])
                .context("queued pre-assignment anchor")?;
            let mut message = target
                .storage()
                .read_message_by_id(&ids[0])?
                .context("persisted pre-assignment anchor")?;
            message.turn_id = None;
            target.inner.runtime_db.transaction(|tx| {
                crate::runtime_db::evidence::insert_message_evidence_tx(tx, &message)
            })?;
            let message = target
                .storage()
                .read_message_by_id(&message.id)?
                .context("persisted pre-assignment anchor")?;
            guard.queue.push_front(message);
        }
        let original_anchor = target
            .storage()
            .read_message_by_id(&ids[0])?
            .context("durable original anchor")?;
        assert_eq!(original_anchor.turn_id.is_none(), null_ingress_turn);
        let mut original_activation_id = None;
        let mut original_turn_id = None;

        for replay in [false, true] {
            if replay {
                drop(target);
                target = new_target()?;
                target.recover_scheduler_bootstrap_claims().await?;
            }
            let anchor = start_reply_selector_turn(&target).await?;
            assert_eq!(anchor.id, ids[0]);
            assert_eq!(anchor.turn_id, original_anchor.turn_id);
            if replay {
                assert_eq!(
                    anchor.source_refs.get("replay_source_turn_id"),
                    original_turn_id.as_ref()
                );
                assert_eq!(
                    anchor.source_refs.get("replay_reason").map(String::as_str),
                    Some("interrupted_queue_claim_reentry")
                );
            } else {
                assert!(!anchor.source_refs.contains_key("replay_source_turn_id"));
                assert!(!anchor.source_refs.contains_key("replay_reason"));
            }
            assert!(target.attach_idle_sender_inputs(&anchor, 24 * 1024).await?);
            let state = target.inner.agent.lock().await.state.clone();
            let turn = target
                .storage()
                .read_turn_by_id(state.current_turn_id.as_deref().context("live turn")?)?
                .context("live turn record")?;
            assert_ne!(
                original_follower.turn_id.as_deref(),
                Some(turn.turn_id.as_str())
            );
            assert_eq!(turn.input_message_ids, ids);
            assert_eq!(turn.replay.is_some(), replay);
            let execution = target
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("reply-selector-target")?
                .context("canonical execution")?;
            let binding = state
                .current_execution_binding
                .as_ref()
                .context("current execution binding")?;
            assert_eq!(binding.turn_id, turn.turn_id);
            assert_eq!(binding.source_message_id, ids[0]);
            let attempt = &execution.attempts[binding.activation_id.as_ref().unwrap()];
            assert_eq!(attempt.agent_id, turn.agent_id);
            assert_eq!(attempt.source_message_id.as_ref(), Some(&ids[0]));
            assert!(attempt.run_id.is_none());
            assert_eq!(attempt.turn_id, anchor.turn_id);
            if let Some(replay) = &turn.replay {
                assert_eq!(attempt.recovery_of_attempt_id, original_activation_id);
                assert_eq!(replay.source_message_id, ids[0]);
                assert_eq!(
                    original_turn_id.as_deref(),
                    Some(replay.source_turn_id.as_str())
                );
                assert_ne!(attempt.turn_id.as_deref(), Some(turn.turn_id.as_str()));
                let prior = &execution.attempts[attempt.recovery_of_attempt_id.as_ref().unwrap()];
                assert_eq!(
                    prior.state,
                    crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
                );
                assert_eq!(prior.turn_id, original_anchor.turn_id);
                assert_eq!(prior.turn_id.is_none(), null_ingress_turn);
                assert_eq!(attempt.turn_id, prior.turn_id);
                assert_eq!(attempt.source_message_id, prior.source_message_id);
                assert_eq!(attempt.binding, prior.binding);
                let source = target
                    .storage()
                    .read_turn_by_id(&replay.source_turn_id)?
                    .context("original admission turn")?;
                assert_ne!(source.turn_id, turn.turn_id);
                assert_eq!(source.agent_id, turn.agent_id);
                assert_eq!(source.input_message_ids, ids);
                assert_eq!(
                    source
                        .trigger
                        .as_ref()
                        .and_then(|trigger| trigger.message_id.as_deref()),
                    Some(ids[0].as_str())
                );
                let source_batch = source
                    .idle_input_batch
                    .as_ref()
                    .context("independent source admission snapshot")?;
                assert_eq!(source_batch.anchor_message_id, ids[0]);
                assert_eq!(
                    source_batch.activation_id.as_deref(),
                    Some(prior.attempt_id.as_str())
                );
                assert_eq!(
                    source_batch
                        .items
                        .iter()
                        .map(|item| &item.message_id)
                        .collect::<Vec<_>>(),
                    ids.iter().collect::<Vec<_>>()
                );
                for item in &source_batch.items {
                    assert_eq!(item.source_ref, format!("message:{}", item.message_id));
                }
            } else {
                assert_eq!(attempt.turn_id, original_anchor.turn_id);
                assert!(attempt.recovery_of_attempt_id.is_none());
                original_activation_id = Some(attempt.attempt_id.clone());
                original_turn_id = Some(turn.turn_id.clone());
            }
            assert_eq!(
                target
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&ids[1])?
                    .context("follower queue entry")?
                    .status,
                QueueEntryStatus::Interjected
            );
            assert_eq!(
                serde_json::to_value(target.storage().read_message_by_id(&ids[1])?.unwrap())?,
                serde_json::to_value(&original_follower)?
            );
            let deliveries = target.inner.runtime_db.agent_message_deliveries();
            let follower_delivery = deliveries
                .latest_for_message(&ids[1])?
                .context("canonical follower delivery")?;
            assert_eq!(
                follower_delivery.outcome,
                AgentMessageDeliveryOutcome::Accepted
            );
            assert!(follower_delivery.activation_id.is_none());
            assert!(follower_delivery.turn_id.is_none());
            assert_eq!(
                follower_delivery.routing_context,
                original_follower.routing_context
            );
            assert!(deliveries
                .resolve_reply_context(
                    "reply-selector-target",
                    state.current_turn_id.as_deref(),
                    None,
                    &sender_id,
                    false,
                )
                .is_err());
            for (index, id) in ids.iter().enumerate() {
                let selected = deliveries
                    .resolve_reply_context(
                        "reply-selector-target",
                        state.current_turn_id.as_deref(),
                        Some(id),
                        &sender_id,
                        false,
                    )?
                    .context("selected admitted input")?;
                assert_eq!(selected.routing_context.message_id, *id);
                assert_eq!(
                    selected.routing_context.correlation_id,
                    Some(format!("correlation-{index}"))
                );
                assert_eq!(
                    selected.activation_id,
                    state
                        .current_execution_binding
                        .as_ref()
                        .unwrap()
                        .activation_id
                        .as_ref()
                        .unwrap()
                        .as_str()
                );
                let mut reply_caller = caller(Some(selected.routing_context.clone()));
                reply_caller.caller_agent_id = Some("reply-selector-target".into());
                reply_caller.current_turn_id = Some(selected.turn_id);
                reply_caller.routing_activation_id = Some(selected.activation_id);
                let mut reply = request(&sender_id, &format!("reply-{replay}-{index}"));
                reply.reply_context_id = Some(id.clone());
                let prepared = AgentMessageDeliveryService::prepare(reply, reply_caller)?;
                assert_eq!(
                    prepared.message.correlation_id,
                    selected.routing_context.correlation_id
                );
                assert_eq!(prepared.message.causation_id.as_deref(), Some(id.as_str()));
                assert_eq!(
                    prepared
                        .message
                        .routing_context
                        .unwrap()
                        .in_reply_to_message_id
                        .as_deref(),
                    Some(id.as_str())
                );
            }
            let assert_follower_rejected = |defect: &str| {
                assert!(
                    deliveries
                        .resolve_reply_context(
                            "reply-selector-target",
                            state.current_turn_id.as_deref(),
                            Some(&ids[1]),
                            &sender_id,
                            false,
                        )
                        .is_err(),
                    "{defect}"
                );
            };
            if replay {
                let source = target
                    .storage()
                    .read_turn_by_id(&turn.replay.as_ref().unwrap().source_turn_id)?
                    .context("replay source fixture")?;
                for defect in [
                    "missing_source_batch",
                    "wrong_source_ref",
                    "missing_source_admission",
                ] {
                    let mut invalid = source.clone();
                    match defect {
                        "missing_source_batch" => invalid.idle_input_batch = None,
                        "wrong_source_ref" => {
                            invalid.idle_input_batch.as_mut().unwrap().items[1].source_ref =
                                "message:foreign".into();
                        }
                        _ => {
                            invalid.input_message_ids.retain(|id| id != &ids[1]);
                        }
                    }
                    target.inner.runtime_db.turn_records().upsert(&invalid)?;
                    assert_follower_rejected(defect);
                }
                if null_ingress_turn {
                    let mut invalid = source.clone();
                    invalid.idle_input_batch.as_mut().unwrap().activation_id =
                        Some(attempt.attempt_id.clone());
                    target.inner.runtime_db.turn_records().upsert(&invalid)?;
                    assert_follower_rejected("source_batch_bound_to_successor");
                }
                target.inner.runtime_db.turn_records().upsert(&source)?;
                let mut invalid = execution.clone();
                invalid
                    .attempts
                    .get_mut(&attempt.attempt_id)
                    .unwrap()
                    .recovery_of_attempt_id = None;
                target.inner.runtime_db.transaction(|tx| {
                    crate::runtime_db::transitions::persist_state_tx(tx, &invalid)
                })?;
                assert_follower_rejected("missing_canonical_predecessor");
                target.inner.runtime_db.transaction(|tx| {
                    crate::runtime_db::transitions::persist_state_tx(tx, &execution)
                })?;
                // The same accepted, unbound ingress ID becomes foreign once a
                // durable turn exists; neither snapshot may authorize it now.
                let foreign = crate::types::TurnRecord::new(
                    "reply-selector-target",
                    original_follower.turn_id.as_deref().unwrap(),
                    3,
                );
                target.inner.runtime_db.turn_records().upsert(&foreign)?;
                assert_follower_rejected("foreign_bound_ingress_turn");
            } else {
                for defect in ["input_ids_only", "wrong_source_ref"] {
                    let mut invalid = turn.clone();
                    if defect == "input_ids_only" {
                        invalid.idle_input_batch = None;
                    } else {
                        invalid.idle_input_batch.as_mut().unwrap().items[1].source_ref =
                            "message:foreign".into();
                    }
                    target.inner.runtime_db.turn_records().upsert(&invalid)?;
                    assert_follower_rejected(defect);
                }
                target.inner.runtime_db.turn_records().upsert(&turn)?;
            }
        }
        Ok(())
    }

    #[test]
    fn direct_delivery_populates_trusted_reply_context() {
        let prepared = AgentMessageDeliveryService::prepare(
            request("target-agent", "direct-routing"),
            caller(None),
        )
        .unwrap();
        let routing = prepared.message.routing_context.as_ref().unwrap();

        assert_eq!(routing.sender_agent_id.as_deref(), Some("caller-agent"));
        assert_eq!(routing.recipient_agent_id, "target-agent");
        assert_eq!(routing.reply_to_agent_id.as_deref(), Some("caller-agent"));
        assert_eq!(routing.correlation_id, None);
        assert_eq!(routing.in_reply_to_message_id, None);
        assert_eq!(routing.original_sender_agent_id, None);
        assert_eq!(routing.original_reply_to_agent_id, None);
        assert_eq!(prepared.record.routing_context.as_ref(), Some(routing));
    }

    #[test]
    fn forwarded_delivery_preserves_original_sender_and_reply_target() {
        let inherited = AgentMessageRoutingContext {
            message_id: "incoming-message".into(),
            sender_agent_id: Some("origin-agent".into()),
            recipient_agent_id: "caller-agent".into(),
            reply_to_agent_id: Some("origin-agent".into()),
            correlation_id: Some("correlation-1".into()),
            in_reply_to_message_id: None,
            original_sender_agent_id: None,
            original_reply_to_agent_id: None,
        };
        let mut request = request("final-agent", "forwarded-routing");
        request.forward = true;
        let prepared =
            AgentMessageDeliveryService::prepare(request, caller(Some(inherited))).unwrap();
        let routing = prepared.message.routing_context.as_ref().unwrap();

        assert_eq!(routing.sender_agent_id.as_deref(), Some("caller-agent"));
        assert_eq!(routing.recipient_agent_id, "final-agent");
        assert_eq!(routing.reply_to_agent_id.as_deref(), Some("origin-agent"));
        assert_eq!(routing.correlation_id.as_deref(), Some("correlation-1"));
        assert_eq!(
            routing.in_reply_to_message_id.as_deref(),
            Some("incoming-message")
        );
        assert_eq!(
            routing.original_sender_agent_id.as_deref(),
            Some("origin-agent")
        );
        assert_eq!(
            routing.original_reply_to_agent_id.as_deref(),
            Some("origin-agent")
        );
        assert_eq!(prepared.record.routing_context.as_ref(), Some(routing));

        let lifecycle = crate::types::MessageLifecycleAuditEvent::from_message(&prepared.message);
        assert_eq!(lifecycle.routing_context.as_ref(), Some(routing));
    }

    #[test]
    fn direct_response_targets_the_immediate_sender_instead_of_itself() {
        let inherited = AgentMessageRoutingContext {
            message_id: "incoming-message".into(),
            sender_agent_id: Some("origin-agent".into()),
            recipient_agent_id: "caller-agent".into(),
            reply_to_agent_id: Some("origin-agent".into()),
            correlation_id: Some("correlation-1".into()),
            in_reply_to_message_id: None,
            original_sender_agent_id: None,
            original_reply_to_agent_id: None,
        };
        let prepared = AgentMessageDeliveryService::prepare(
            request("origin-agent", "direct-response"),
            caller(Some(inherited)),
        )
        .unwrap();
        let routing = prepared.message.routing_context.as_ref().unwrap();

        assert_eq!(routing.reply_to_agent_id.as_deref(), Some("caller-agent"));
        assert_eq!(
            routing.in_reply_to_message_id.as_deref(),
            Some("incoming-message")
        );
        assert_eq!(routing.original_sender_agent_id, None);
        assert_eq!(routing.original_reply_to_agent_id, None);
    }

    #[test]
    fn direct_response_chain_targets_the_immediate_sender() {
        let mut first_caller = caller(None);
        first_caller.caller_agent_id = Some("agent-a".into());
        let first =
            AgentMessageDeliveryService::prepare(request("agent-b", "chain-a-to-b"), first_caller)
                .unwrap();
        let first_route = first.message.routing_context.clone().unwrap();

        let mut second_caller = caller(Some(first_route));
        second_caller.caller_agent_id = Some("agent-b".into());
        let second =
            AgentMessageDeliveryService::prepare(request("agent-a", "chain-b-to-a"), second_caller)
                .unwrap();
        let second_route = second.message.routing_context.unwrap();

        assert_eq!(second_route.sender_agent_id.as_deref(), Some("agent-b"));
        assert_eq!(second_route.recipient_agent_id, "agent-a");
        assert_eq!(second_route.reply_to_agent_id.as_deref(), Some("agent-b"));
        assert_eq!(
            second_route.in_reply_to_message_id.as_deref(),
            Some(first.message.id.as_str())
        );
    }

    #[test]
    fn unrelated_send_drops_inherited_route_without_forward_intent() {
        let inherited = AgentMessageRoutingContext {
            message_id: "incoming-message".into(),
            sender_agent_id: Some("origin-agent".into()),
            recipient_agent_id: "caller-agent".into(),
            reply_to_agent_id: Some("origin-agent".into()),
            correlation_id: Some("correlation-1".into()),
            in_reply_to_message_id: None,
            original_sender_agent_id: None,
            original_reply_to_agent_id: None,
        };
        let prepared = AgentMessageDeliveryService::prepare(
            request("unrelated-agent", "new-notification"),
            caller(Some(inherited)),
        )
        .unwrap();
        let routing = prepared.message.routing_context.as_ref().unwrap();

        assert_eq!(routing.reply_to_agent_id.as_deref(), Some("caller-agent"));
        assert_eq!(routing.correlation_id, None);
        assert_eq!(routing.in_reply_to_message_id, None);
        assert_eq!(routing.original_sender_agent_id, None);
        assert_eq!(routing.original_reply_to_agent_id, None);
    }

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
            inherited_routing_context: None,
            routing_activation_id: None,
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
                forward: false,
                reply_context_id: None,
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
