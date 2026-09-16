use anyhow::{anyhow, Result};
use std::future::Future;
use std::pin::Pin;

use super::RuntimeHandle;
use crate::runtime_error::{
    collect_runtime_error_source_chain, describe_runtime_error, RuntimeError, RuntimeErrorDomain,
};
use crate::types::{
    AdmissionContext, AgentCreateResult, AgentInvocationReceipt, AgentMessageCallerContext,
    AgentMessageDeliveryReceipt, AgentMessagePrincipalKind, AgentMessageSendRequest,
    AgentModelResolution, AgentSupervisionState, AuthorityClass, CreateAgentRequest,
    InvokeAgentRequest, InvokeAgentTarget, MessageDeliverySurface, MessageOrigin, TaskHandle,
    TaskRecord, TaskStatus,
};

pub(crate) struct AgentCreationService<'a> {
    runtime: &'a RuntimeHandle,
}

pub(crate) struct AgentInvocationService<'a> {
    runtime: &'a RuntimeHandle,
}

pub(crate) struct AgentMessagingService<'a> {
    runtime: &'a RuntimeHandle,
}

impl RuntimeHandle {
    pub(crate) fn agent_creation_service(&self) -> AgentCreationService<'_> {
        AgentCreationService { runtime: self }
    }

    pub(crate) fn agent_invocation_service(&self) -> AgentInvocationService<'_> {
        AgentInvocationService { runtime: self }
    }

    pub(crate) fn agent_messaging_service(&self) -> AgentMessagingService<'_> {
        AgentMessagingService { runtime: self }
    }
}

impl AgentCreationService<'_> {
    pub(crate) async fn create(&self, request: CreateAgentRequest) -> Result<AgentCreateResult> {
        let bridge = self
            .runtime
            .inner
            .host_bridge
            .clone()
            .ok_or_else(|| anyhow!("agent creation requires a host bridge"))?;
        bridge.create_agent(self.runtime.clone(), request).await
    }
}

impl AgentMessagingService<'_> {
    pub(crate) async fn send(
        &self,
        request: AgentMessageSendRequest,
        authority_class: AuthorityClass,
    ) -> Result<AgentMessageDeliveryReceipt> {
        let bridge = self
            .runtime
            .inner
            .host_bridge
            .clone()
            .ok_or_else(|| anyhow!("agent messaging requires a host bridge"))?;
        let caller_agent_id = self.runtime.agent_id().await?;
        let state = self.runtime.agent_state().await?;
        let principal_kind = bridge
            .canonical_relations_for_agent(&request.target_agent_id)
            .await?
            .and_then(|relations| relations.supervision)
            .filter(|supervision| {
                supervision.supervisor_agent_id == caller_agent_id
                    && matches!(
                        supervision.state,
                        AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
                    )
            })
            .map(|_| AgentMessagePrincipalKind::SupervisingParent)
            .unwrap_or(AgentMessagePrincipalKind::PeerAgent);
        let route = match principal_kind {
            AgentMessagePrincipalKind::SupervisingParent => "supervision_follow_up",
            AgentMessagePrincipalKind::PeerAgent => "agent_message",
            _ => unreachable!("agent messaging service only derives agent principals"),
        };
        let caller = AgentMessageCallerContext {
            caller_principal: format!("agent:{caller_agent_id}"),
            caller_agent_id: Some(caller_agent_id),
            principal_kind,
            route: route.into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: state.current_turn_id,
            current_task_id: None,
            current_work_item_id: state
                .current_turn_work_item_id
                .or(state.current_work_item_id),
        };
        let prepared = crate::runtime::AgentMessageDeliveryService::prepare(request, caller)?;
        Ok(bridge.deliver_agent_message(&prepared).await?.receipt)
    }
}

impl AgentInvocationService<'_> {
    pub(crate) fn invoke<'a>(
        &'a self,
        request: InvokeAgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentInvocationReceipt>> + Send + 'a>> {
        Box::pin(async move {
            let message = request.message;
            if message.trim().is_empty() {
                return Err(anyhow!("agent invocation requires a non-empty message"));
            }
            let bridge = self
                .runtime
                .inner
                .host_bridge
                .clone()
                .ok_or_else(|| anyhow!("agent invocation requires a host bridge"))?;
            let summary = super::tasks::spawn_agent_task_label(&message);
            match request.target {
                InvokeAgentTarget::ExistingAgent { agent_id } => {
                    let task = self
                        .runtime
                        .create_agent_message_wait_task(
                            summary,
                            message.clone(),
                            agent_id.clone(),
                            request.authority_class.clone(),
                        )
                        .await?;
                    let mut admitted = match bridge
                        .invoke_existing_agent(
                            &task,
                            &agent_id,
                            message,
                            request.authority_class.clone(),
                        )
                        .await
                    {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            self.runtime
                                .fail_agent_invocation_task(&task, &error)
                                .await?;
                            if describe_runtime_error(&error).code == "agent_target_unavailable" {
                                return Err(error);
                            }
                            let direct_cause = collect_runtime_error_source_chain(&error)
                                .last()
                                .cloned()
                                .unwrap_or_else(|| {
                                    "agent message wait admission failed".to_string()
                                });
                            return Err(error.context(
                                RuntimeError::new(
                                    RuntimeErrorDomain::Task,
                                    "agent_invocation_failed",
                                    format!("failed to invoke agent: {direct_cause}"),
                                )
                                .with_safe_context("task_id", &task.id)
                                .with_recovery_hint(
                                    "correct the target agent or message and retry",
                                ),
                            ));
                        }
                    };
                    let delivery_id = admitted.delivery_id.clone().ok_or_else(|| {
                        anyhow!("accepted agent message invocation is missing its delivery id")
                    })?;
                    let after_delivery_rowid = bridge.agent_message_delivery_rowid(&delivery_id)?;
                    admitted.task_detail["created_new_subagent"] = serde_json::json!(false);
                    admitted.task_detail["request_delivery_id"] = serde_json::json!(&delivery_id);
                    if let Some(detail) = admitted.task_detail.as_object_mut() {
                        detail.remove("delivery_id");
                    }
                    admitted.task_detail["message_wait_after_delivery_rowid"] =
                        serde_json::json!(after_delivery_rowid);
                    admitted.task_detail["business_completion"] = serde_json::json!(false);
                    let queued_task = self
                        .runtime
                        .start_agent_message_wait_monitor(
                            task,
                            request.authority_class,
                            agent_id.clone(),
                            delivery_id,
                            after_delivery_rowid,
                            admitted.task_detail,
                            false,
                        )
                        .await?;
                    Ok(AgentInvocationReceipt {
                        agent_id,
                        created: false,
                        task_handle: TaskHandle::from_task_record(&queued_task, None),
                    })
                }
                InvokeAgentTarget::NewSubagent {
                    template,
                    workspace_mode,
                    model_resolution,
                } => {
                    let model_resolution = required_model_resolution(model_resolution)?;
                    let task = self
                        .runtime
                        .create_agent_invocation_task(
                            summary,
                            message.clone(),
                            request.authority_class.clone(),
                            None,
                            true,
                            workspace_mode,
                        )
                        .await?;
                    let mut admitted = match bridge
                        .spawn_child_task(
                            self.runtime.clone(),
                            &task,
                            message,
                            request.authority_class.clone(),
                            workspace_mode.is_worktree(),
                            template,
                            model_resolution,
                        )
                        .await
                    {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            self.runtime
                                .fail_agent_invocation_task(&task, &error)
                                .await?;
                            let direct_cause = collect_runtime_error_source_chain(&error)
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "agent invocation admission failed".to_string());
                            return Err(error.context(
                                RuntimeError::new(
                                    RuntimeErrorDomain::Task,
                                    "agent_invocation_failed",
                                    format!("failed to invoke agent: {direct_cause}"),
                                )
                                .with_safe_context("task_id", &task.id)
                                .with_recovery_hint(
                                    "correct the template, model, or workspace configuration and retry",
                                ),
                            ));
                        }
                    };
                    admitted.task_detail["created_new_subagent"] = serde_json::json!(true);
                    let queued_task = self
                        .runtime
                        .start_agent_invocation_monitor(
                            task,
                            request.authority_class,
                            workspace_mode.is_worktree(),
                            admitted.child_agent_id.clone(),
                            admitted.child_turn_baseline,
                            admitted.delivery_id,
                            admitted.task_detail,
                        )
                        .await?;
                    Ok(AgentInvocationReceipt {
                        agent_id: admitted.child_agent_id,
                        created: true,
                        task_handle: TaskHandle::from_task_record(&queued_task, None),
                    })
                }
            }
        })
    }
}

pub(super) fn required_model_resolution(
    resolution: Option<AgentModelResolution>,
) -> Result<AgentModelResolution> {
    resolution.ok_or_else(|| anyhow!("new subagent invocation requires model resolution"))
}

pub(super) fn failed_invocation_task(task: &TaskRecord, error: &anyhow::Error) -> TaskRecord {
    let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
    detail["error"] = serde_json::json!(error.to_string());
    TaskRecord {
        status: TaskStatus::Failed,
        updated_at: chrono::Utc::now(),
        detail: Some(detail),
        ..task.clone()
    }
}
