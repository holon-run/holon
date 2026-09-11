use anyhow::{anyhow, Result};
use std::future::Future;
use std::pin::Pin;

use super::RuntimeHandle;
use crate::runtime_error::{
    collect_runtime_error_source_chain, describe_runtime_error, RuntimeError, RuntimeErrorDomain,
};
use crate::types::{
    AgentCreateResult, AgentInvocationReceipt, AgentModelResolution, ChildAgentWorkspaceMode,
    CreateAgentRequest, InvokeAgentRequest, InvokeAgentTarget, TaskHandle, TaskRecord, TaskStatus,
};

pub(crate) struct AgentCreationService<'a> {
    runtime: &'a RuntimeHandle,
}

pub(crate) struct AgentInvocationService<'a> {
    runtime: &'a RuntimeHandle,
}

impl RuntimeHandle {
    pub(crate) fn agent_creation_service(&self) -> AgentCreationService<'_> {
        AgentCreationService { runtime: self }
    }

    pub(crate) fn agent_invocation_service(&self) -> AgentInvocationService<'_> {
        AgentInvocationService { runtime: self }
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
            let (target_agent_id, created_new_subagent, workspace_mode, model_resolution) =
                match &request.target {
                    InvokeAgentTarget::ExistingAgent { agent_id } => (
                        Some(agent_id.clone()),
                        false,
                        ChildAgentWorkspaceMode::Inherit,
                        None,
                    ),
                    InvokeAgentTarget::NewSubagent {
                        workspace_mode,
                        model_resolution,
                        ..
                    } => (
                        None,
                        true,
                        *workspace_mode,
                        Some(required_model_resolution(model_resolution.clone())?),
                    ),
                };
            let summary = super::tasks::spawn_agent_task_label(&message);
            let task = self
                .runtime
                .create_agent_invocation_task(
                    summary,
                    message.clone(),
                    request.authority_class,
                    target_agent_id,
                    created_new_subagent,
                    workspace_mode,
                )
                .await?;

            let admitted = match request.target {
                InvokeAgentTarget::ExistingAgent { agent_id } => {
                    bridge
                        .invoke_existing_agent(&task, &agent_id, message, request.authority_class)
                        .await
                }
                InvokeAgentTarget::NewSubagent {
                    template,
                    workspace_mode,
                    ..
                } => {
                    bridge
                        .spawn_child_task(
                            self.runtime.clone(),
                            &task,
                            message,
                            request.authority_class,
                            workspace_mode.is_worktree(),
                            template,
                            model_resolution.expect(
                                "new subagent model resolution was validated before task creation",
                            ),
                        )
                        .await
                }
            };
            let mut admitted = match admitted {
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
                        .unwrap_or_else(|| "agent invocation admission failed".to_string());
                    return Err(error.context(
                        RuntimeError::new(
                            RuntimeErrorDomain::Task,
                            "agent_invocation_failed",
                            format!("failed to invoke agent: {direct_cause}"),
                        )
                        .with_safe_context("task_id", &task.id)
                        .with_recovery_hint(
                            "correct the target agent, template, model, or workspace configuration and retry",
                        ),
                    ));
                }
            };
            admitted.task_detail["created_new_subagent"] = serde_json::json!(created_new_subagent);
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
                created: created_new_subagent,
                task_handle: TaskHandle::from_task_record(&queued_task, None),
            })
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
