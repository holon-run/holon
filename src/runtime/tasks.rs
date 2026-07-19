use super::message_dispatch::message_text;
use super::waiting::WorkItemBlockerClearance;
use super::{task_state_reducer, *};
use crate::config::{ModelRef, ProviderId};
use crate::runtime_error::{
    sanitize_runtime_error_text, RuntimeError, RuntimeErrorContext, RuntimeErrorDomain,
};
use crate::tool::helpers::truncate_output_to_char_budget;
use crate::tool::ToolError;
use crate::types::{
    AgentProfilePreset, BriefKind, BriefRecord, ChildAgentWorkspaceMode, CommandTaskStatusSnapshot,
    FailureArtifact, FailureArtifactCategory, SpawnAgentModelRequest, SpawnAgentModelResolution,
    SpawnAgentModelResolutionStatus, SpawnAgentResult, TaskHandle, TaskInputResult, TaskKind,
    TaskListEntry, TaskOutputResult, TaskOutputRetrievalStatus, TaskOutputSnapshot,
    TaskStatusSnapshot, TodoItem, ToolArtifactRef, WaitConditionStatus, WorkItemContinuationFrame,
    WorkItemContinuationReturnPolicy, WorkItemDelegationRecord, WorkItemDelegationState,
    WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord, WorkItemState, CHILD_AGENT_TASK_KIND,
};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

const TASK_OUTPUT_POLL_INTERVAL_MS: u64 = 100;
const TASK_OUTPUT_MESSAGE_SCAN_LIMIT: usize = 200;
const TASK_OUTPUT_PREVIEW_CHAR_BUDGET: usize = 8_000;
const SPAWN_AGENT_TASK_LABEL_CHAR_BUDGET: usize = 120;

#[derive(Debug, Clone)]
struct TaskMessageSnapshot {
    state: TaskStatus,
    text: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemFocusTransitionWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemFocusTransition {
    pub previous_work_item_id: Option<String>,
    pub current_work_item_id: String,
    pub reason: Option<String>,
    pub previous_readiness: Option<WorkItemReadiness>,
    pub current_readiness: WorkItemReadiness,
    pub switch_kind: String,
    pub current_focus_mode: String,
    pub blocker_cleared: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_wait_condition_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkItemFocusTransitionWarning>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemContinuationSummary {
    pub frame_id: String,
    pub suspended_work_item_id: String,
    pub active_work_item_id: String,
    pub return_policy: WorkItemContinuationReturnPolicy,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PickedWorkItem {
    pub previous_work_item: Option<WorkItemRecord>,
    pub current_work_item: WorkItemRecord,
    pub transition: WorkItemFocusTransition,
    pub continuation_created: Option<WorkItemContinuationSummary>,
    pub continuation_resolved: Option<WorkItemContinuationSummary>,
}

#[derive(Debug, Clone)]
pub struct CompletedWorkItem {
    pub work_item: WorkItemRecord,
    pub continuation_resumed: Option<WorkItemContinuationSummary>,
}

fn child_agent_task_detail(workspace_mode: ChildAgentWorkspaceMode) -> serde_json::Value {
    serde_json::json!({
        "wait_policy": crate::types::TaskWaitPolicy::Background,
        "workspace_mode": workspace_mode,
    })
}

fn spawn_agent_task_label(initial_message: &str) -> String {
    let collapsed = initial_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let label = if collapsed.is_empty() {
        "delegated child agent task".to_string()
    } else {
        collapsed
    };
    crate::tool::helpers::truncate_text(&label, SPAWN_AGENT_TASK_LABEL_CHAR_BUDGET)
}

fn inherited_model_parameters(
    model: &crate::types::AgentModelState,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut parameters = serde_json::Map::new();
    if let Some(reasoning_effort) = model.override_reasoning_effort.clone() {
        parameters.insert("reasoning_effort".into(), reasoning_effort.into());
    }
    (!parameters.is_empty()).then_some(parameters)
}

fn inherited_spawn_model_resolution(
    model: &crate::types::AgentModelState,
) -> SpawnAgentModelResolution {
    SpawnAgentModelResolution {
        requested: None,
        resolved_provider: model.effective_model.provider.as_str().to_string(),
        resolved_model: model.effective_model.model.clone(),
        resolved_parameters: inherited_model_parameters(model),
        resolution_status: SpawnAgentModelResolutionStatus::Inherited,
        policy_notes: Vec::new(),
    }
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn task_with_status(
    task: &TaskRecord,
    status: TaskStatus,
    detail: Option<serde_json::Value>,
) -> TaskRecord {
    TaskRecord {
        id: task.id.clone(),
        agent_id: task.agent_id.clone(),
        kind: task.kind.clone(),
        status,
        created_at: task.created_at,
        updated_at: Utc::now(),
        parent_message_id: task.parent_message_id.clone(),
        work_item_id: task.work_item_id.clone(),
        summary: task.summary.clone(),
        detail,
        recovery: task.recovery.clone(),
    }
}

fn task_with_result_message(
    task: &TaskRecord,
    status: TaskStatus,
    mut detail: Option<serde_json::Value>,
    result_message: &MessageEnvelope,
) -> TaskRecord {
    if let (Some(detail), Some(parent_turn_id)) = (detail.as_mut(), result_message.turn_id.as_ref())
    {
        if let Some(detail) = detail.as_object_mut() {
            detail.insert(
                "parent_turn_id".to_string(),
                serde_json::json!(parent_turn_id),
            );
        }
    }
    let mut terminal_task = task_with_status(task, status, detail);
    terminal_task.parent_message_id = Some(result_message.id.clone());
    terminal_task
}

impl RuntimeHandle {
    pub(super) async fn task_work_item_binding(&self) -> Option<String> {
        let guard = self.inner.agent.lock().await;
        guard
            .state
            .current_turn_work_item_id
            .clone()
            .or_else(|| guard.state.current_work_item_id.clone())
    }

    pub(crate) fn supports_child_agent_spawning(&self) -> bool {
        self.inner.host_bridge.is_some()
    }

    pub(super) async fn ensure_background_tasks_allowed(&self, surface: &str) -> Result<()> {
        let state = self.agent_state().await?;
        crate::system::ensure_background_task_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &state.execution_profile,
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            surface,
        )
    }

    pub async fn schedule_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
        workspace_mode: ChildAgentWorkspaceMode,
    ) -> Result<TaskRecord> {
        match workspace_mode {
            ChildAgentWorkspaceMode::Inherit => {
                self.schedule_inherited_child_agent_task(summary, prompt, authority_class)
                    .await
            }
            ChildAgentWorkspaceMode::Worktree => {
                self.schedule_worktree_child_agent_task(summary, prompt, authority_class)
                    .await
            }
        }
    }

    async fn schedule_inherited_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let workspace_mode = ChildAgentWorkspaceMode::Inherit;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            authority_class: authority_class.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.clone()),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, authority_class, false, false)
                .await?;
            return Ok(task);
        }

        let runtime = self.clone();
        let task_record = task.clone();
        let task_id = task.id.clone();
        let handle = tokio::spawn(async move {
            let running_message = MessageEnvelope {
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": "running",
                    "task_summary": task_record.summary,
                    "task_detail": task_record.detail,
                    "task_recovery": task_record.recovery,
                    "work_item_id": task_record.work_item_id.clone(),
                })),
                ..MessageEnvelope::new(
                    agent_id.clone(),
                    MessageKind::TaskStatus,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    authority_class.clone(),
                    Priority::Background,
                    MessageBody::Text {
                        text: format!(
                            "child agent task started: {}",
                            task_record.summary.clone().unwrap_or_default()
                        ),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let _ = runtime.enqueue(running_message).await;

            let subagent_result = runtime
                .run_subagent_prompt(&agent_id, &prompt, &authority_class)
                .await;
            let (text, status) = match subagent_result {
                Ok(text) => (text, TaskStatus::Completed),
                Err(err) => (format!("child agent failed: {err:#}"), TaskStatus::Failed),
            };
            let status_label = task_status_label(&status);
            let mut task_detail = task_record
                .detail
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            task_detail["output_summary"] = serde_json::json!(text.clone());

            let result_message = MessageEnvelope {
                turn_id: Some(crate::ids::turn_id()),
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": status_label,
                    "task_summary": task_record.summary,
                    "task_detail": task_detail.clone(),
                    "task_recovery": task_record.recovery,
                    "work_item_id": task_record.work_item_id.clone(),
                })),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let terminal_task =
                task_with_result_message(&task_record, status, Some(task_detail), &result_message);
            if let Err(error) = runtime
                .persist_task_status_direct(&terminal_task, "task_status_updated")
                .await
            {
                tracing::warn!(
                    task_id = %terminal_task.id,
                    error = %error,
                    "failed to persist terminal task status before task result"
                );
            }
            let _ = runtime.enqueue(result_message).await;
            runtime
                .inner
                .task_handles
                .lock()
                .await
                .remove(&task_record.id);
        });
        self.inner
            .task_handles
            .lock()
            .await
            .insert(task_id, command_task::ManagedTaskHandle::Async(handle));

        Ok(task)
    }

    pub async fn spawn_agent(
        &self,
        initial_message: Option<String>,
        authority_class: AuthorityClass,
        preset: AgentProfilePreset,
        agent_id: Option<String>,
        worktree: bool,
        template: Option<String>,
        model_request: Option<SpawnAgentModelRequest>,
    ) -> Result<SpawnAgentResult> {
        if !self.supports_child_agent_spawning() {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "unsupported_runtime_capability",
                    "SpawnAgent is not available in this runtime",
                )
                .with_details(serde_json::json!({
                    "tool_name": crate::tool::names::SPAWN_AGENT,
                    "required_capability": "child_agent_spawning",
                }))
                .with_recovery_hint(
                    "run SpawnAgent from a host-managed runtime with child-agent support",
                ),
            ));
        }
        let model_resolution = self
            .resolve_spawn_agent_model_request(model_request)
            .await?;
        let bridge = self
            .inner
            .host_bridge
            .clone()
            .expect("spawn agent support should imply host bridge");

        match preset {
            AgentProfilePreset::PrivateChild => {
                let initial_message = initial_message
                    .ok_or_else(|| anyhow!("private_child spawn requires initial_message"))?;
                if initial_message.trim().is_empty() {
                    return Err(anyhow!(
                        "private_child spawn requires non-empty initial_message"
                    ));
                }
                let task_label = spawn_agent_task_label(&initial_message);
                let task = self
                    .create_child_supervision_task(
                        task_label,
                        initial_message.clone(),
                        authority_class.clone(),
                        worktree,
                    )
                    .await?;

                let spawned = match bridge
                    .spawn_child_task(
                        self.clone(),
                        &task,
                        initial_message,
                        authority_class.clone(),
                        worktree,
                        template.clone(),
                        model_resolution.clone(),
                    )
                    .await
                {
                    Ok(spawned) => spawned,
                    Err(err) => {
                        let failed_task = TaskRecord {
                            status: TaskStatus::Failed,
                            updated_at: Utc::now(),
                            ..task.clone()
                        };
                        self.persist_task_status_direct(&failed_task, "task_spawn_failed")
                            .await?;
                        return Err(err.context("failed to spawn child agent"));
                    }
                };

                let queued_task = TaskRecord {
                    updated_at: Utc::now(),
                    detail: Some(spawned.task_detail.clone()),
                    ..task.clone()
                };
                self.apply_task_transition(task_state_reducer::TaskTransition::new(
                    &queued_task,
                    "task_child_spawned",
                ))
                .await?;

                let runtime = self.clone();
                let task_record = queued_task.clone();
                let task_id = queued_task.id.clone();
                let child_agent_id = spawned.child_agent_id.clone();
                let child_turn_baseline = spawned.child_turn_baseline;
                let task_detail = spawned.task_detail.clone();
                let handle = tokio::spawn(async move {
                    let _ = runtime
                        .monitor_spawned_child_agent_task(
                            task_record.clone(),
                            authority_class,
                            worktree,
                            false,
                            child_agent_id,
                            child_turn_baseline,
                            task_detail,
                        )
                        .await;
                    runtime.inner.task_handles.lock().await.remove(&task_id);
                });
                self.inner.task_handles.lock().await.insert(
                    queued_task.id.clone(),
                    command_task::ManagedTaskHandle::Async(handle),
                );

                let child_supervision =
                    crate::types::ChildSupervisionProjection::from_task_record(&queued_task);

                Ok(SpawnAgentResult {
                    agent_id: spawned.child_agent_id.clone(),
                    child_agent_id: Some(spawned.child_agent_id.clone()),
                    task_handle: Some(TaskHandle::from_task_record(&queued_task, None)),
                    supervision_task_id: Some(queued_task.id.clone()),
                    child_supervision,
                    summary_text: Some(format!(
                        "delegated child {} started under supervision task {}",
                        spawned.child_agent_id, queued_task.id
                    )),
                    delegation_id: None,
                    parent_work_item_id: None,
                    child_work_item_id: None,
                    model_resolution: Some(model_resolution),
                })
            }
            AgentProfilePreset::PublicNamed => {
                let agent_id = agent_id
                    .ok_or_else(|| anyhow!("public_named spawn requires a stable agent id"))?;
                if worktree {
                    return Err(anyhow!(
                        "public_named spawn does not support workspace_mode=worktree"
                    ));
                }

                let spawned_agent_id = bridge
                    .spawn_public_named_agent(
                        self.clone(),
                        &agent_id,
                        initial_message,
                        authority_class,
                        template,
                        model_resolution.clone(),
                    )
                    .await?;

                Ok(SpawnAgentResult {
                    agent_id: spawned_agent_id.clone(),
                    child_agent_id: None,
                    task_handle: None,
                    supervision_task_id: None,
                    child_supervision: None,
                    summary_text: Some(format!(
                        "spawned public named agent {} without a supervising task handle",
                        spawned_agent_id
                    )),
                    delegation_id: None,
                    parent_work_item_id: None,
                    child_work_item_id: None,
                    model_resolution: Some(model_resolution),
                })
            }
        }
    }

    async fn resolve_spawn_agent_model_request(
        &self,
        request: Option<SpawnAgentModelRequest>,
    ) -> Result<SpawnAgentModelResolution> {
        let Some(request) = request else {
            let inherited = self.model_state_for(&self.agent_state().await?);
            return Ok(inherited_spawn_model_resolution(&inherited));
        };

        let provider = ProviderId::parse(&request.provider).map_err(|error| {
            ToolError::new(
                "invalid_tool_input",
                format!("SpawnAgent model.provider is invalid: {error}"),
            )
            .with_details(serde_json::json!({
                "field": "model.provider",
                "validation_error": error.to_string(),
            }))
            .with_recovery_hint("provide a lowercase provider id from ListModelProviders")
        })?;
        let requested_model = request.model.trim();
        if requested_model.is_empty() {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "invalid_tool_input",
                    "SpawnAgent model.model must not be empty",
                )
                .with_details(serde_json::json!({
                    "field": "model.model",
                    "validation_error": "must not be empty",
                }))
                .with_recovery_hint("provide a model id from ListProviderModels"),
            ));
        }
        let model_ref = ModelRef::new(provider, requested_model.to_string());
        let model_ref_string = model_ref.as_string();
        let availability =
            self.model_availability().await?.into_iter().find(|entry| {
                entry.policy.model_ref == model_ref || entry.model == model_ref_string
            });
        let Some(availability) = availability else {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "model_request_rejected",
                    format!("requested model {model_ref_string} is not in the model catalog"),
                )
                .with_details(serde_json::json!({
                    "requested_model": model_ref_string,
                    "resolution_status": "rejected",
                }))
                .with_recovery_hint(
                    "call ListModelProviders and ListProviderModels before requesting a model",
                ),
            ));
        };

        if !availability.available {
            let allow_fallback = request.allow_fallback.unwrap_or(true);
            let reason = availability
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "model_unavailable".to_string());
            let recovery_hint = if allow_fallback {
                "request an available/selectable model; fallback for explicit unavailable SpawnAgent requests is not used before child creation"
            } else {
                "request an available/selectable model, or omit model to inherit the parent model"
            };
            return Err(anyhow::Error::from(
                ToolError::new(
                    "model_request_rejected",
                    format!("requested model {model_ref_string} is unavailable: {reason}"),
                )
                .with_details(serde_json::json!({
                    "requested_model": model_ref_string,
                    "unavailable_reason": reason,
                    "allow_fallback": allow_fallback,
                    "resolution_status": "rejected",
                }))
                .with_recovery_hint(recovery_hint),
            ));
        }

        let mut resolved_parameters = serde_json::Map::new();
        let mut policy_notes = Vec::new();
        if let Some(reasoning_effort) = request.reasoning_effort.clone() {
            resolved_parameters.insert("reasoning_effort".into(), reasoning_effort.into());
        }
        if let Some(temperature) = request.temperature {
            resolved_parameters.insert("temperature".into(), temperature.into());
            policy_notes.push(
                "temperature is accepted for audit output; provider support is transport-specific"
                    .to_string(),
            );
        }
        if let Some(max_output_tokens) = request.max_output_tokens {
            resolved_parameters.insert("max_output_tokens".into(), max_output_tokens.into());
            policy_notes.push(
                "max_output_tokens is accepted for audit output; runtime policy may cap effective output"
                    .to_string(),
            );
        }

        Ok(SpawnAgentModelResolution {
            requested: Some(request),
            resolved_provider: availability.policy.model_ref.provider.as_str().to_string(),
            resolved_model: availability.policy.model_ref.model,
            resolved_parameters: (!resolved_parameters.is_empty()).then_some(resolved_parameters),
            resolution_status: SpawnAgentModelResolutionStatus::Accepted,
            policy_notes,
        })
    }

    async fn schedule_worktree_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        let workspace_mode = ChildAgentWorkspaceMode::Worktree;
        let state = self.agent_state().await?;
        crate::system::ensure_workspace_projection_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &state.execution_profile,
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            WorkspaceProjectionKind::GitWorktreeRoot,
            CHILD_AGENT_TASK_KIND,
        )?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            authority_class: authority_class.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.clone()),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, authority_class, true, false)
                .await?;
            return Ok(task);
        }

        let runtime = self.clone();
        let task_record = task.clone();
        let task_id = task.id.clone();
        let handle = tokio::spawn(async move {
            let subagent_result = runtime
                .run_subagent_prompt_in_dedicated_worktree(
                    &agent_id,
                    &prompt,
                    &authority_class,
                    &task_record.id,
                )
                .await;

            let (mut text, status, mut task_detail, worktree_path): (
                String,
                TaskStatus,
                serde_json::Value,
                Option<(PathBuf, String, Vec<String>)>,
            ) = match subagent_result {
                Ok(result) => {
                    let worktree_path = result.worktree_path.clone();
                    let worktree_branch = result.worktree_branch.clone();
                    let changed_files = result.changed_files.clone();

                    let worktree_metadata = serde_json::json!({
                        "worktree_path": result.worktree_path,
                        "worktree_branch": result.worktree_branch,
                        "changed_files": result.changed_files,
                    });
                    (
                        worktree::format_worktree_task_result(&result),
                        if result.failed {
                            TaskStatus::Failed
                        } else {
                            TaskStatus::Completed
                        },
                        {
                            let mut detail = task_record
                                .detail
                                .clone()
                                .unwrap_or_else(|| serde_json::json!({}));
                            detail["worktree"] = worktree_metadata;
                            detail
                        },
                        Some((worktree_path, worktree_branch, changed_files)),
                    )
                }
                Err(err) => (
                    format!("worktree child agent failed: {err:#}"),
                    TaskStatus::Failed,
                    task_record
                        .detail
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                    None,
                ),
            };

            if let Some((path, _, _)) = worktree_path {
                if let Ok(Some(cleanup)) = runtime
                    .cleanup_task_owned_worktree_in_detail(
                        &task_record.id,
                        &mut task_detail,
                        "terminal_task_result",
                    )
                    .await
                {
                    append_task_owned_worktree_cleanup_note(&mut text, &cleanup, &path);
                }
            }

            task_detail["output_summary"] = serde_json::json!(text.clone());
            let status_label = task_status_label(&status);
            let mut metadata = serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": status_label,
                "task_summary": task_record.summary,
                "task_detail": task_detail.clone(),
                "task_recovery": task_record.recovery,
                "work_item_id": task_record.work_item_id.clone(),
            });
            if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
                metadata["worktree"] = worktree;
            }
            let result_message = MessageEnvelope {
                turn_id: Some(crate::ids::turn_id()),
                metadata: Some(metadata),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let terminal_task =
                task_with_result_message(&task_record, status, Some(task_detail), &result_message);
            if let Err(error) = runtime
                .persist_task_status_direct(&terminal_task, "task_status_updated")
                .await
            {
                tracing::warn!(
                    task_id = %terminal_task.id,
                    error = %error,
                    "failed to persist terminal task status before task result"
                );
            }
            let _ = runtime.enqueue(result_message).await;
            runtime
                .inner
                .task_handles
                .lock()
                .await
                .remove(&task_record.id);
        });
        self.inner
            .task_handles
            .lock()
            .await
            .insert(task_id, command_task::ManagedTaskHandle::Async(handle));

        Ok(task)
    }

    async fn spawn_child_agent_task(
        &self,
        task_record: TaskRecord,
        prompt: String,
        authority_class: AuthorityClass,
        worktree: bool,
        recovered: bool,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Err(anyhow!("child agent runtime requires a host bridge"));
        };
        let agent_id = self.agent_id().await?;

        let existing_detail = task_record.detail.clone();
        let existing_child_id = detail_string(&existing_detail, "child_agent_id");
        let inherited_model_resolution =
            inherited_spawn_model_resolution(&self.model_state_for(&self.agent_state().await?));
        let runtime = self.clone();
        let task_id = task_record.id.clone();
        let task_id_for_cleanup = task_id.clone();
        let handle = tokio::spawn(async move {
            let spawned = async {
                if let Some(child_agent_id) = existing_child_id {
                    if bridge.reusable_agent_exists(&child_agent_id).await? {
                        let child_turn_baseline = match existing_detail
                            .as_ref()
                            .and_then(|detail| detail.get("child_turn_baseline"))
                            .and_then(|value| value.as_u64())
                        {
                            Some(value) => value,
                            None => bridge.child_turn_index(&child_agent_id).await?,
                        };
                        Ok::<(String, u64, serde_json::Value), anyhow::Error>((
                            child_agent_id,
                            child_turn_baseline,
                            existing_detail.unwrap_or_else(|| serde_json::json!({})),
                        ))
                    } else {
                        let spawned = bridge
                            .spawn_child_task(
                                runtime.clone(),
                                &task_record,
                                prompt,
                                authority_class.clone(),
                                worktree,
                                None,
                                inherited_model_resolution.clone(),
                            )
                            .await?;
                        Ok((
                            spawned.child_agent_id,
                            spawned.child_turn_baseline,
                            spawned.task_detail,
                        ))
                    }
                } else {
                    let spawned = bridge
                        .spawn_child_task(
                            runtime.clone(),
                            &task_record,
                            prompt,
                            authority_class.clone(),
                            worktree,
                            None,
                            inherited_model_resolution.clone(),
                        )
                        .await?;
                    Ok((
                        spawned.child_agent_id,
                        spawned.child_turn_baseline,
                        spawned.task_detail,
                    ))
                }
            }
            .await;

            let (child_agent_id, child_turn_baseline, task_detail) = match spawned {
                Ok(spawned) => spawned,
                Err(err) => {
                    let result_message = MessageEnvelope {
                        turn_id: Some(crate::ids::turn_id()),
                        metadata: Some(serde_json::json!({
                            "task_id": task_record.id,
                            "task_kind": task_record.kind,
                            "task_status": "failed",
                            "task_summary": task_record.summary,
                            "task_detail": task_record.detail,
                            "task_recovery": task_record.recovery,
                            "work_item_id": task_record.work_item_id.clone(),
                        })),
                        ..MessageEnvelope::new(
                            agent_id.clone(),
                            MessageKind::TaskResult,
                            MessageOrigin::Task {
                                task_id: task_record.id.clone(),
                            },
                            AuthorityClass::RuntimeInstruction,
                            Priority::Next,
                            MessageBody::Text {
                                text: format!("child agent failed: {err:#}"),
                            },
                        )
                        .with_admission(
                            MessageDeliverySurface::TaskRejoin,
                            AdmissionContext::RuntimeOwned,
                        )
                    };
                    let failed_task = task_with_result_message(
                        &task_record,
                        TaskStatus::Failed,
                        task_record.detail.clone(),
                        &result_message,
                    );
                    if let Err(error) = runtime
                        .persist_task_status_direct(&failed_task, "task_status_updated")
                        .await
                    {
                        tracing::warn!(
                            task_id = %failed_task.id,
                            error = %error,
                            "failed to persist terminal task status before task result"
                        );
                    }
                    let _ = runtime.enqueue(result_message).await;
                    runtime
                        .inner
                        .task_handles
                        .lock()
                        .await
                        .remove(&task_id_for_cleanup);
                    return;
                }
            };
            let _ = runtime
                .monitor_spawned_child_agent_task(
                    task_record,
                    authority_class,
                    worktree,
                    recovered,
                    child_agent_id,
                    child_turn_baseline,
                    task_detail,
                )
                .await;
            runtime
                .inner
                .task_handles
                .lock()
                .await
                .remove(&task_id_for_cleanup);
        });
        self.inner
            .task_handles
            .lock()
            .await
            .insert(task_id, command_task::ManagedTaskHandle::Async(handle));
        Ok(())
    }

    async fn create_child_supervision_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
        worktree: bool,
    ) -> Result<TaskRecord> {
        let workspace_mode = if worktree {
            ChildAgentWorkspaceMode::Worktree
        } else {
            ChildAgentWorkspaceMode::Inherit
        };
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        if worktree {
            let state = self.agent_state().await?;
            crate::system::ensure_workspace_projection_allowed(
                &crate::system::HostLocalBoundary::from_parts(
                    &state.execution_profile,
                    state
                        .active_workspace_entry
                        .as_ref()
                        .map(|entry| entry.projection_kind),
                    state
                        .active_workspace_entry
                        .as_ref()
                        .map(|entry| entry.access_mode),
                    state
                        .active_workspace_entry
                        .as_ref()
                        .map(|entry| entry.execution_root_id.clone()),
                ),
                WorkspaceProjectionKind::GitWorktreeRoot,
                CHILD_AGENT_TASK_KIND,
            )?;
        }

        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt,
            authority_class: authority_class.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: crate::ids::task_id(),
            agent_id,
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;
        Ok(task)
    }

    async fn monitor_spawned_child_agent_task(
        &self,
        task_record: TaskRecord,
        authority_class: AuthorityClass,
        worktree: bool,
        recovered: bool,
        child_agent_id: String,
        child_turn_baseline: u64,
        task_detail: serde_json::Value,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Err(anyhow!("child agent runtime requires a host bridge"));
        };
        let agent_id = self.agent_id().await?;

        let running_message = MessageEnvelope {
            metadata: Some(serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": "running",
                "task_summary": task_record.summary,
                "task_recovery": task_record.recovery,
                "work_item_id": task_record.work_item_id.clone(),
                "task_detail": task_detail.clone(),
            })),
            ..MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                authority_class.clone(),
                Priority::Background,
                MessageBody::Text {
                    text: if recovered {
                        format!(
                            "{} restarted after recovery: {}",
                            if worktree {
                                "worktree child agent"
                            } else {
                                "child agent"
                            },
                            task_record.summary.clone().unwrap_or_default()
                        )
                    } else {
                        format!(
                            "{} started: {}",
                            if worktree {
                                "worktree child agent"
                            } else {
                                "child agent"
                            },
                            task_record.summary.clone().unwrap_or_default()
                        )
                    },
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            )
        };
        let _ = self.enqueue(running_message).await;

        let task_detail_for_result = task_detail.clone();
        let result = bridge
            .await_child_terminal_result(&child_agent_id, child_turn_baseline, worktree)
            .await;
        let (mut text, status, mut task_detail) = match result {
            Ok(result) => (
                result.text,
                result.status,
                result.task_detail.unwrap_or(task_detail_for_result.clone()),
            ),
            Err(err) => (
                format!("child agent failed: {err:#}"),
                TaskStatus::Failed,
                task_detail_for_result.clone(),
            ),
        };
        if worktree && task_detail.get("worktree").is_none() {
            if let Some(worktree_detail) = task_detail_for_result.get("worktree").cloned() {
                task_detail["worktree"] = worktree_detail;
            }
        }
        if task_detail.get("workspace_mode").is_none() {
            if let Some(workspace_mode) = task_detail_for_result.get("workspace_mode").cloned() {
                task_detail["workspace_mode"] = workspace_mode;
            }
        }
        if task_detail.get("wait_policy").is_none() {
            if let Some(wait_policy) = task_detail_for_result.get("wait_policy").cloned() {
                task_detail["wait_policy"] = wait_policy;
            }
        }

        if worktree {
            if let Some(worktree) = task_detail.get("worktree").cloned() {
                let worktree_path = worktree
                    .get("worktree_path")
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from);
                let worktree_branch = worktree
                    .get("worktree_branch")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                let changed_files = worktree
                    .get("changed_files")
                    .and_then(|value| value.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| entry.as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                if let (Some(path), Some(branch)) = (worktree_path, worktree_branch) {
                    text = worktree::format_worktree_task_result(&WorktreeSubagentResult {
                        text,
                        worktree_path: path.clone(),
                        worktree_branch: branch.clone(),
                        changed_files: changed_files.clone(),
                        failed: status == TaskStatus::Failed,
                    });
                    if let Ok(Some(cleanup)) = self
                        .cleanup_task_owned_worktree_in_detail(
                            &task_record.id,
                            &mut task_detail,
                            "terminal_task_result",
                        )
                        .await
                    {
                        append_task_owned_worktree_cleanup_note(&mut text, &cleanup, &path);
                    }
                }
            }
        }

        let delegation = self
            .inner
            .storage
            .open_work_item_delegation_for_child(&child_agent_id)?;
        if let Some(delegation) = delegation.as_ref() {
            let completed = WorkItemDelegationRecord {
                state: WorkItemDelegationState::Completed,
                result_summary: Some(text.clone()),
                updated_at: Utc::now(),
                ..delegation.clone()
            };
            self.inner.storage.append_work_item_delegation(&completed)?;
            self.inner.storage.append_event(&AuditEvent::legacy(
                "work_item_delegation_completed",
                serde_json::to_value(&completed)?,
            ))?;
        }

        let status_label = task_status_label(&status);
        let mut metadata = serde_json::json!({
            "task_id": task_record.id,
            "task_kind": task_record.kind,
            "task_status": status_label,
            "task_summary": task_record.summary,
            "task_recovery": task_record.recovery,
            "work_item_id": task_record.work_item_id.clone(),
            "task_detail": task_detail.clone(),
        });
        if let Some(delegation) = delegation.as_ref() {
            metadata["delegation_id"] = serde_json::json!(delegation.delegation_id.clone());
            metadata["work_item_id"] = serde_json::json!(delegation.parent_work_item_id.clone());
            metadata["child_work_item_id"] =
                serde_json::json!(delegation.child_work_item_id.clone());
        }
        if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
            metadata["worktree"] = worktree;
        }
        task_detail["output_summary"] = serde_json::json!(text.clone());
        let result_message = MessageEnvelope {
            turn_id: Some(crate::ids::turn_id()),
            metadata: Some(metadata),
            ..MessageEnvelope::new(
                agent_id,
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text { text },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            )
        };
        let terminal_task =
            task_with_result_message(&task_record, status, Some(task_detail), &result_message);
        if let Err(error) = self
            .persist_task_status_direct(&terminal_task, "task_status_updated")
            .await
        {
            tracing::warn!(
                task_id = %terminal_task.id,
                error = %error,
                "failed to persist terminal task status before task result"
            );
        }
        let _ = self.enqueue(result_message).await;
        Ok(())
    }

    pub(crate) async fn interrupt_active_tasks(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<Vec<TaskRecord>> {
        self.persist_interrupted_tasks(tasks).await
    }

    pub(crate) async fn recover_supervised_child_tasks(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<(Vec<TaskRecord>, Vec<TaskRecord>)> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Ok((Vec::new(), tasks));
        };

        let mut reattached = Vec::new();
        let mut remaining = Vec::new();

        for task in tasks {
            let (prompt, authority_class, worktree) = match task.recovery.as_ref() {
                Some(TaskRecoverySpec::ChildAgentTask {
                    prompt,
                    authority_class,
                    workspace_mode,
                    ..
                }) => (
                    prompt.clone(),
                    authority_class.clone(),
                    workspace_mode.is_worktree(),
                ),
                Some(TaskRecoverySpec::SubagentTask {
                    prompt,
                    authority_class,
                    ..
                }) => (prompt.clone(), authority_class.clone(), false),
                Some(TaskRecoverySpec::WorktreeSubagentTask {
                    prompt,
                    authority_class,
                    ..
                }) => (prompt.clone(), authority_class.clone(), true),
                _ => {
                    remaining.push(task);
                    continue;
                }
            };

            let child_agent_id = detail_string(&task.detail, "child_agent_id");
            let Some(child_agent_id) = child_agent_id else {
                remaining.push(task);
                continue;
            };

            if !bridge.reusable_agent_exists(&child_agent_id).await? {
                remaining.push(task);
                continue;
            }

            match self
                .spawn_child_agent_task(task.clone(), prompt, authority_class, worktree, true)
                .await
            {
                Ok(()) => reattached.push(task),
                Err(error) => {
                    self.inner.storage.append_event(&AuditEvent::legacy(
                        "supervised_child_task_recovery_failed",
                        serde_json::json!({
                            "task_id": task.id,
                            "child_agent_id": child_agent_id,
                            "error": error.to_string(),
                        }),
                    ))?;
                    remaining.push(task);
                }
            }
        }

        Ok((reattached, remaining))
    }

    pub async fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks = self.inner.runtime_db.tasks().latest_all()?;
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub async fn latest_task_list_entries(&self) -> Result<Vec<TaskListEntry>> {
        let agent_id = self.agent_id().await?;
        self.latest_task_list_entries_for_agent(&agent_id, usize::MAX)
            .await
    }

    pub async fn latest_task_list_entries_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskListEntry>> {
        Ok(self
            .inner
            .runtime_db
            .tasks()
            .active_for_agent(agent_id, limit)?
            .into_iter()
            .map(|task| {
                let wait_policy = task.wait_policy();
                let command = CommandTaskStatusSnapshot::identity_from_task_record(&task);
                TaskListEntry {
                    id: task.id,
                    kind: task.kind.as_str().to_string(),
                    status: task.status,
                    summary: task.summary,
                    updated_at: task.updated_at,
                    wait_policy,
                    command,
                }
            })
            .collect())
    }

    async fn persist_task_status_direct(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.persist_task_transition(task, event_kind).await
    }

    async fn persist_interrupted_tasks(&self, tasks: Vec<TaskRecord>) -> Result<Vec<TaskRecord>> {
        let mut interrupted = Vec::new();
        for task in tasks {
            let prior_status = task_status_name(&task.status);
            let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(detail) = detail.as_object_mut() {
                detail.insert(
                    "status_before_restart".into(),
                    serde_json::json!(prior_status),
                );
                detail.insert("task_status".into(), serde_json::json!("interrupted"));
                detail.insert(
                    "interrupted_reason".into(),
                    serde_json::json!("runtime_restarted"),
                );
                detail.insert("interrupted_at".into(), to_json_value(&Utc::now()));
            }
            let interrupted_task = TaskRecord {
                id: task.id.clone(),
                agent_id: task.agent_id.clone(),
                kind: task.kind.clone(),
                status: TaskStatus::Interrupted,
                created_at: task.created_at,
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: task.work_item_id.clone(),
                summary: task.summary.clone(),
                detail: Some(detail),
                recovery: task.recovery.clone(),
            };
            self.persist_task_status_direct(&interrupted_task, "task_interrupted_on_restart")
                .await?;
            interrupted.push(interrupted_task);
        }
        Ok(interrupted)
    }

    pub(super) async fn interrupt_active_tasks_for_lifecycle_stop(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<Vec<TaskRecord>> {
        let mut interrupted = Vec::new();
        for task in tasks {
            {
                let mut handles = self.inner.task_handles.lock().await;
                match handles.remove(&task.id) {
                    Some(command_task::ManagedTaskHandle::Async(handle)) => {
                        handle.abort();
                    }
                    Some(command_task::ManagedTaskHandle::Command(mut handle)) => {
                        if let Some(cancel_tx) = handle.cancel_tx.take() {
                            let _ = cancel_tx.send(());
                        }
                        if let Some(force_stop_tx) = handle.force_stop_tx.take() {
                            let _ = force_stop_tx.send(());
                        }
                    }
                    None => {}
                }
            }

            let prior_status = task_status_name(&task.status);
            let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(detail_map) = detail.as_object_mut() {
                detail_map.insert("status_before_stop".into(), serde_json::json!(prior_status));
                detail_map.insert("task_status".into(), serde_json::json!("interrupted"));
                detail_map.insert(
                    "interrupted_reason".into(),
                    serde_json::json!("agent_stopped"),
                );
                detail_map.insert("interrupted_at".into(), to_json_value(&Utc::now()));
            }
            if task.is_worktree_child_agent_task() {
                let _ = self
                    .cleanup_task_owned_worktree_in_detail(
                        &task.id,
                        &mut detail,
                        "agent_lifecycle_stop",
                    )
                    .await;
            }

            let interrupted_task = TaskRecord {
                id: task.id.clone(),
                agent_id: task.agent_id.clone(),
                kind: task.kind,
                status: TaskStatus::Interrupted,
                created_at: task.created_at,
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: task.work_item_id.clone(),
                summary: task.summary.clone(),
                detail: Some(detail),
                recovery: task.recovery.clone(),
            };
            self.persist_task_status_direct(&interrupted_task, "task_interrupted_on_agent_stop")
                .await?;
            interrupted.push(interrupted_task);
        }
        Ok(interrupted)
    }

    pub async fn task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        self.inner.runtime_db.tasks().latest(task_id)
    }

    pub async fn task_status_snapshot(&self, task_id: &str) -> Result<TaskStatusSnapshot> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| task_not_found_error(task_id))?;
        let mut snapshot = TaskStatusSnapshot::from_task_record(&task);

        if task.is_child_agent_task()
            && matches!(
                task.status,
                TaskStatus::Queued
                    | TaskStatus::Running
                    | TaskStatus::Cancelling
                    | TaskStatus::Interrupted
            )
        {
            if let (Some(bridge), Some(child_agent_id)) = (
                self.inner.host_bridge.as_ref(),
                snapshot.child_agent_id.as_deref(),
            ) {
                if let Some(observability) = bridge.child_observability(child_agent_id).await? {
                    snapshot.child_observability = Some(observability);
                }
            }
        }
        if let Some(projection) = snapshot.child_supervision.take() {
            snapshot.child_supervision = Some(
                if let Ok(Some(delegation)) = self
                    .inner
                    .storage
                    .latest_work_item_delegation_for_child(&projection.child_agent_id)
                {
                    projection.with_work_item_delegation(&delegation)
                } else {
                    projection
                },
            );
        }

        Ok(snapshot)
    }

    pub async fn task_output(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<TaskOutputResult> {
        let started_at = tokio::time::Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        loop {
            let task = self
                .task_record(task_id)
                .await?
                .ok_or_else(|| task_not_found_error(task_id))?;
            let status = self.task_output_status(&task)?;
            let ready = task_output_ready(&task, &status);

            if !ready && block {
                let elapsed = started_at.elapsed();
                if elapsed < timeout {
                    let remaining = timeout.saturating_sub(elapsed);
                    let poll_interval =
                        Duration::from_millis(TASK_OUTPUT_POLL_INTERVAL_MS).min(remaining);
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
            }

            let retrieval_status = if ready {
                TaskOutputRetrievalStatus::Success
            } else {
                if block {
                    TaskOutputRetrievalStatus::Timeout
                } else {
                    TaskOutputRetrievalStatus::NotReady
                }
            };
            let snapshot = self.task_output_snapshot(task).await?;

            return Ok(TaskOutputResult {
                retrieval_status,
                task: snapshot,
            });
        }
    }

    async fn task_output_snapshot(&self, task: TaskRecord) -> Result<TaskOutputSnapshot> {
        let latest_message = self.latest_task_message(&task.id)?;
        let status = effective_task_output_status(&task.status, latest_message.as_ref());
        let summary = task.summary.clone();

        let (full_output, output_path, result_summary, exit_status) =
            if task.kind == TaskKind::CommandTask {
                let output_path = detail_string(&task.detail, "output_path");
                let output = self
                    .read_command_task_output(&task, output_path.as_deref())
                    .await?;
                let result_summary = detail_string(&task.detail, "output_summary")
                    .or_else(|| latest_message.as_ref().map(|message| message.text.clone()));
                let exit_status = task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("exit_status"))
                    .and_then(|value| value.as_i64())
                    .and_then(|value| i32::try_from(value).ok());
                (output, output_path, result_summary, exit_status)
            } else {
                let output = latest_message
                    .as_ref()
                    .map(|message| message.text.clone())
                    .or_else(|| detail_string(&task.detail, "output_summary"))
                    .unwrap_or_else(|| summary.clone().unwrap_or_default());
                let result_summary = if output.trim().is_empty() {
                    None
                } else {
                    Some(output.clone())
                };
                (output, None, result_summary, None)
            };
        let (output_preview, output_truncated) =
            truncate_output_to_char_budget(&full_output, TASK_OUTPUT_PREVIEW_CHAR_BUDGET);
        let (artifacts, output_artifact) = task_output_artifacts(output_path.as_deref());
        let failure_artifact = task_failure_artifact(
            &task,
            &status,
            full_output.as_str(),
            output_path.as_deref(),
            exit_status,
        );

        let child_supervision = crate::types::ChildSupervisionProjection::from_task_record(&task)
            .map(|projection| {
                if let Ok(Some(delegation)) = self
                    .inner
                    .storage
                    .latest_work_item_delegation_for_child(&projection.child_agent_id)
                {
                    projection.with_work_item_delegation(&delegation)
                } else {
                    projection
                }
            });

        let token_usage = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("token_usage"))
            .and_then(|value| serde_json::from_value(value.clone()).ok());

        Ok(TaskOutputSnapshot {
            task_id: task.id,
            kind: task.kind.as_str().to_string(),
            status,
            summary,
            output_preview,
            output_truncated,
            artifacts,
            output_artifact,
            result_summary,
            exit_status,
            failure_artifact,
            child_supervision,
            token_usage,
        })
    }

    fn latest_task_message(&self, task_id: &str) -> Result<Option<TaskMessageSnapshot>> {
        let recent_messages = self
            .inner
            .storage
            .read_recent_messages(TASK_OUTPUT_MESSAGE_SCAN_LIMIT)?;
        if let Some(snapshot) = latest_task_message_in(recent_messages, task_id) {
            return Ok(Some(snapshot));
        }
        Ok(latest_task_message_in(
            self.inner.storage.read_all_messages()?,
            task_id,
        ))
    }

    fn task_output_status(&self, task: &TaskRecord) -> Result<TaskStatus> {
        let latest_message = self.latest_task_message(&task.id)?;
        Ok(effective_task_output_status(
            &task.status,
            latest_message.as_ref(),
        ))
    }

    async fn read_command_task_output(
        &self,
        task: &TaskRecord,
        output_path: Option<&str>,
    ) -> Result<String> {
        let max_output_tokens = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("max_output_tokens"))
            .and_then(|value| value.as_u64())
            .map(|value| value as usize);
        let fallback = detail_string(&task.detail, "initial_output")
            .or_else(|| detail_string(&task.detail, "output_summary"))
            .unwrap_or_default();

        let Some(path) = output_path else {
            return Ok(crate::tool::helpers::truncate_output_for_tokens(
                &fallback,
                max_output_tokens,
            ));
        };

        match tokio::fs::read_to_string(path).await {
            Ok(content) if !content.is_empty() || fallback.is_empty() => Ok(
                crate::tool::helpers::truncate_output_for_tokens(&content, max_output_tokens),
            ),
            Ok(_) => Ok(crate::tool::helpers::truncate_output_for_tokens(
                &fallback,
                max_output_tokens,
            )),
            Err(_) => Ok(crate::tool::helpers::truncate_output_for_tokens(
                &fallback,
                max_output_tokens,
            )),
        }
    }

    pub async fn stop_task(
        &self,
        task_id: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskRecord> {
        let existing = self.task_record(task_id).await?;
        let is_command_task = existing
            .as_ref()
            .is_some_and(|task| task.kind == TaskKind::CommandTask);
        let mut force_stop_requested = false;
        let mut command_handle_missing = false;
        if is_command_task {
            let mut handles = self.inner.task_handles.lock().await;
            match handles.get_mut(task_id) {
                Some(command_task::ManagedTaskHandle::Command(handle)) => {
                    if let Some(cancel_tx) = handle.cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    } else if let Some(force_stop_tx) = handle.force_stop_tx.take() {
                        let _ = force_stop_tx.send(());
                        force_stop_requested = true;
                    } else {
                        force_stop_requested = true;
                    }
                }
                Some(command_task::ManagedTaskHandle::Async(_)) => {
                    return Err(RuntimeError::new(
                        RuntimeErrorDomain::Task,
                        "task_handle_type_mismatch",
                        format!("task {task_id} has an unexpected async handle"),
                    )
                    .with_safe_context("task_id", task_id)
                    .into());
                }
                None => {
                    command_handle_missing = true;
                    force_stop_requested = true;
                }
            }
            drop(handles);
        } else {
            let mut handles = self.inner.task_handles.lock().await;
            match handles.remove(task_id) {
                Some(handle) => {
                    drop(handles);
                    match handle {
                        command_task::ManagedTaskHandle::Async(handle) => {
                            handle.abort();
                        }
                        command_task::ManagedTaskHandle::Command(mut handle) => {
                            if let Some(cancel_tx) = handle.cancel_tx.take() {
                                let _ = cancel_tx.send(());
                            }
                            if let Some(force_stop_tx) = handle.force_stop_tx.take() {
                                let _ = force_stop_tx.send(());
                            }
                        }
                    }
                }
                None => {
                    drop(handles);
                    let can_cleanup_interrupted_child = existing.as_ref().is_some_and(|task| {
                        task.is_child_agent_task()
                            && matches!(task.status, TaskStatus::Interrupted)
                            && detail_string(&task.detail, "child_agent_id").is_some()
                    });
                    if !can_cleanup_interrupted_child {
                        return Err(RuntimeError::validation(
                            "task_not_running",
                            format!("task {task_id} is not currently running"),
                        )
                        .with_safe_context("task_id", task_id)
                        .into());
                    }
                }
            }
        }

        if let Some(child_agent_id) = existing
            .as_ref()
            .and_then(|task| detail_string(&task.detail, "child_agent_id"))
        {
            if let Some(bridge) = self.inner.host_bridge.as_ref() {
                let _ = bridge.stop_private_agent(&child_agent_id).await;
            }
        }

        let agent_id = self.agent_id().await?;
        let status = if is_command_task {
            if command_handle_missing {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Cancelling
            }
        } else {
            TaskStatus::Cancelled
        };
        let status_text = match status {
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Cancelled => "cancelled",
            _ => unreachable!("stop_task only emits cancelling or cancelled"),
        };
        let stopped_kind = existing.as_ref().map(|task| task.kind).ok_or_else(|| {
            RuntimeError::validation(
                "task_not_running",
                format!("task {task_id} is not currently running"),
            )
            .with_safe_context("task_id", task_id)
        })?;
        let mut detail = existing.as_ref().and_then(|task| task.detail.clone());
        if let Some(detail_map) = detail.as_mut().and_then(|value| value.as_object_mut()) {
            detail_map.insert("task_status".into(), serde_json::json!(status_text));
            if is_command_task {
                detail_map.insert("cancel_requested".into(), serde_json::json!(true));
                detail_map.insert("accepts_input".into(), serde_json::json!(false));
                detail_map.insert("input_target".into(), serde_json::json!(null));
            }
            if force_stop_requested {
                detail_map.insert("force_stop_requested".into(), serde_json::json!(true));
            }
        }
        if existing
            .as_ref()
            .is_some_and(|task| task.is_worktree_child_agent_task())
        {
            if let Some(detail) = detail.as_mut() {
                let _ = self
                    .cleanup_task_owned_worktree_in_detail(task_id, detail, "task_stop")
                    .await;
            }
        }

        let stopped = TaskRecord {
            id: task_id.to_string(),
            agent_id: agent_id.clone(),
            kind: stopped_kind,
            status,
            created_at: existing
                .as_ref()
                .map(|task| task.created_at)
                .unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: existing.as_ref().and_then(|task| task.work_item_id.clone()),
            summary: existing
                .as_ref()
                .and_then(|task| task.summary.clone())
                .or_else(|| Some(format!("task {status_text}"))),
            detail,
            recovery: existing.as_ref().and_then(|task| task.recovery.clone()),
        };
        self.persist_task_status_direct(&stopped, "task_status_updated")
            .await?;
        if stopped.kind != TaskKind::CommandTask {
            return self
                .finish_stopped_task(agent_id, stopped, authority_class)
                .await;
        }
        Ok(stopped)
    }

    pub async fn task_input(&self, task_id: &str, input: &str) -> Result<TaskInputResult> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| task_not_found_error(task_id))?;
        let snapshot = TaskStatusSnapshot::from_task_record(&task);
        let command = snapshot.command.clone();
        if matches!(
            task.status,
            TaskStatus::Cancelling
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: command.and_then(|value| value.input_target),
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }
        if task.kind != TaskKind::CommandTask {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task does not support input delivery".into()),
            });
        }
        self.deliver_command_task_input(&task, snapshot, command, input)
            .await
    }

    pub async fn task_input_with_trust(
        &self,
        task_id: &str,
        input: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskInputResult> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| task_not_found_error(task_id))?;
        let snapshot = TaskStatusSnapshot::from_task_record(&task);
        let command = snapshot.command.clone();
        if matches!(
            task.status,
            TaskStatus::Cancelling
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: command.clone().and_then(|value| value.input_target),
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }
        if task.kind == TaskKind::CommandTask {
            return self
                .deliver_command_task_input(&task, snapshot, command, input)
                .await;
        }
        if task.is_child_agent_task() {
            return self
                .deliver_child_task_input(&task, snapshot, input, authority_class)
                .await;
        }
        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: false,
            input_target: None,
            bytes_written: None,
            summary_text: Some("task does not support input delivery".into()),
        })
    }

    async fn deliver_command_task_input(
        &self,
        task: &TaskRecord,
        snapshot: TaskStatusSnapshot,
        command: Option<CommandTaskStatusSnapshot>,
        input: &str,
    ) -> Result<TaskInputResult> {
        if command.as_ref().and_then(|value| value.accepts_input) != Some(true) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }

        let input_tx = {
            let handles = self.inner.task_handles.lock().await;
            match handles.get(&task.id) {
                Some(command_task::ManagedTaskHandle::Command(handle)) => handle.input_tx.clone(),
                Some(command_task::ManagedTaskHandle::Async(_)) => {
                    return Ok(TaskInputResult {
                        task: snapshot,
                        accepted_input: false,
                        input_target: None,
                        bytes_written: None,
                        summary_text: Some("task does not support input delivery".into()),
                    });
                }
                None => {
                    return Ok(TaskInputResult {
                        task: snapshot,
                        accepted_input: false,
                        input_target: command.and_then(|value| value.input_target),
                        bytes_written: None,
                        summary_text: Some("task is not currently accepting input".into()),
                    });
                }
            }
        };

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        input_tx
            .send(command_task::CommandTaskInputRequest {
                text: input.to_string(),
                response_tx,
            })
            .await
            .map_err(|_| {
                RuntimeError::validation(
                    "task_not_running",
                    format!("task {} is not currently running", task.id),
                )
                .with_safe_context("task_id", &task.id)
            })?;
        let bytes_written = response_rx
            .await
            .map_err(|_| {
                RuntimeError::new(
                    RuntimeErrorDomain::Task,
                    "task_input_interrupted",
                    format!("task {} input delivery was interrupted", task.id),
                )
                .with_safe_context("task_id", &task.id)
                .with_retryable(true)
            })?
            .map_err(|_| {
                RuntimeError::new(
                    RuntimeErrorDomain::Task,
                    "task_input_failed",
                    format!("task {} input delivery failed", task.id),
                )
                .with_safe_context("task_id", &task.id)
            })?;

        let input_target = command
            .as_ref()
            .and_then(|value| value.input_target.clone())
            .unwrap_or_else(|| "stdin".into());
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_input_delivered",
            serde_json::json!({
                "task_id": task.id,
                "task_kind": task.kind,
                "bytes_written": bytes_written,
                "input_target": input_target,
            }),
        ))?;

        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: true,
            input_target: Some(input_target),
            bytes_written: Some(bytes_written),
            summary_text: Some(format!("delivered input to task {}", task.id)),
        })
    }

    async fn deliver_child_task_input(
        &self,
        task: &TaskRecord,
        snapshot: TaskStatusSnapshot,
        input: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskInputResult> {
        let Some(child_agent_id) = detail_string(&task.detail, "child_agent_id") else {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task does not support input delivery".into()),
            });
        };
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        };

        let parent_agent_id = self.agent_id().await?;
        let delivered = bridge
            .deliver_child_followup(
                &parent_agent_id,
                &task.id,
                &child_agent_id,
                input,
                authority_class.clone(),
            )
            .await?;
        if !delivered {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }

        let bytes_written = input.len() as u64;
        let input_target = "child_followup";
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_input_delivered",
            serde_json::json!({
                "task_id": task.id,
                "task_kind": task.kind,
                "child_agent_id": child_agent_id,
                "bytes_written": bytes_written,
                "input_target": input_target,
            }),
        ))?;

        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: true,
            input_target: Some(input_target.into()),
            bytes_written: Some(bytes_written),
            summary_text: Some(format!(
                "delivered parent follow-up to child {} via supervision task {}",
                child_agent_id, task.id
            )),
        })
    }

    async fn finish_stopped_task(
        &self,
        agent_id: String,
        stopped: TaskRecord,
        _authority_class: &AuthorityClass,
    ) -> Result<TaskRecord> {
        if stopped.kind != TaskKind::CommandTask {
            let message = MessageEnvelope {
                metadata: Some(serde_json::json!({
                    "task_id": stopped.id,
                    "task_kind": stopped.kind,
                    "task_status": "cancelled",
                    "task_summary": stopped.summary,
                    "task_detail": stopped.detail,
                    "task_recovery": stopped.recovery,
                })),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: stopped.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text {
                        text: "task cancelled by operator".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            self.enqueue(message).await?;
        }
        Ok(stopped)
    }

    pub async fn create_work_item(
        &self,
        objective: String,
        plan_status: Option<WorkItemPlanStatus>,
        plan: Option<String>,
        todo_list: Vec<TodoItem>,
    ) -> Result<WorkItemRecord> {
        let agent_id = self.agent_id().await?;
        let mut record = WorkItemRecord::new(agent_id.clone(), objective, WorkItemState::Open);
        if let Some(plan_status) = plan_status {
            record.plan_status = plan_status;
        }
        record.plan_artifact = Some(crate::work_item_plan::ensure_plan_artifact(
            self.agent_home().as_path(),
            &record,
            plan.as_deref(),
        )?);
        record.todo_list = todo_list;
        record.workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id)
            .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string());
        let commit = self.inner.runtime_db.transitions().commit_work_item(
            &crate::runtime_db::transitions::WorkItemTransitionCommand {
                agent_id,
                mutation: crate::runtime_db::transitions::WorkItemMutation::Insert {
                    record: record.clone(),
                },
                agent_state: None,
                audit_events: vec![self.work_item_written_event("created", &record, Value::Null)],
                index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                notify_scheduler: true,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(record)
    }

    pub async fn pick_work_item(
        &self,
        work_item_id: String,
    ) -> Result<(Option<WorkItemRecord>, WorkItemRecord)> {
        let picked = self.pick_work_item_with_reason(work_item_id, None).await?;
        Ok((picked.previous_work_item, picked.current_work_item))
    }

    pub async fn pick_work_item_with_reason(
        &self,
        work_item_id: String,
        reason: Option<String>,
    ) -> Result<PickedWorkItem> {
        self.pick_work_item_with_reason_and_clear_blocker(work_item_id, reason, false)
            .await
    }

    pub async fn pick_work_item_with_reason_and_clear_blocker(
        &self,
        work_item_id: String,
        reason: Option<String>,
        clear_blocker: bool,
    ) -> Result<PickedWorkItem> {
        let agent_id = self.agent_id().await?;
        let state = self.agent_state().await?;
        let current_id = state.current_work_item_id.clone();
        let previous = match current_id.as_deref() {
            Some(id) => self.inner.runtime_db.work_items().latest(id)?,
            None => None,
        };
        let mut record = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if record.state == WorkItemState::Completed {
            return Err(RuntimeError::validation(
                "work_item_completed",
                format!("cannot pick completed work item {work_item_id}"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into());
        }
        let normalized_reason = reason.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
        if clear_blocker && normalized_reason.is_none() {
            return Err(anyhow!(
                "PickWorkItem clear_blocker requires a non-empty reason"
            ));
        }
        let blocker_clearance = if clear_blocker {
            self.clear_work_item_blocker_for_pick(
                &agent_id,
                record,
                normalized_reason
                    .as_deref()
                    .expect("clear_blocker reason validated"),
            )
            .await?
        } else {
            WorkItemBlockerClearance::unchanged(record)
        };
        let WorkItemBlockerClearance {
            work_item,
            expected_revision,
            wait_conditions,
            mut audit_events,
            index_changes,
            blocker_cleared,
            cancelled_wait_condition_ids,
        } = blocker_clearance;
        record = work_item;
        let switching = current_id.as_deref().is_some_and(|id| id != record.id);
        let work_queue = self.inner.storage.work_queue_prompt_projection()?;
        let previous_readiness = previous.as_ref().map(|record| {
            work_queue
                .items
                .iter()
                .find(|item| item.work_item.id == record.id)
                .map(|item| item.readiness)
                .unwrap_or_else(|| record.readiness())
        });
        let current_readiness = if blocker_cleared {
            record.readiness()
        } else {
            work_queue
                .items
                .iter()
                .find(|item| item.work_item.id == record.id)
                .map(|item| item.readiness)
                .unwrap_or_else(|| record.readiness())
        };
        let mut warnings = Vec::new();
        let mut continuation_created = None;
        let mut continuation_resolved = None;
        let mut continuation_records = Vec::new();
        let target_yielded_frame = self
            .inner
            .storage
            .latest_active_work_item_continuation_for_suspended(&agent_id, &record.id)?;
        let target_was_yielded = target_yielded_frame.is_some();
        if let Some(frame) = target_yielded_frame {
            let resolved = frame.resume("explicit_pick");
            continuation_resolved = Some(continuation_summary(&resolved, "explicit_pick"));
            audit_events.push(AuditEvent::legacy(
                "work_item_continuation_resumed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "continuation": continuation_summary(&resolved, "explicit_pick"),
                }),
            ));
            continuation_records.push(resolved);
        }
        if let Some(id) = current_id.as_deref() {
            if let Some(frame) = self
                .inner
                .storage
                .latest_active_work_item_continuation_for_suspended(&agent_id, id)?
            {
                let cancelled = frame.cancel("current_focus_reselected");
                audit_events.push(AuditEvent::legacy(
                    "work_item_continuation_cancelled",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "continuation": continuation_summary(&cancelled, "current_focus_reselected"),
                    }),
                ));
                continuation_records.push(cancelled);
            }
        }
        let yield_current = switching
            && !target_was_yielded
            && previous_readiness == Some(WorkItemReadiness::Runnable)
            && previous
                .as_ref()
                .is_some_and(|record| record.state == WorkItemState::Open)
            && record.state == WorkItemState::Open;
        if yield_current {
            if let Some(existing) = self
                .inner
                .storage
                .latest_active_work_item_continuation_for_active(&agent_id, &record.id)?
            {
                return Err(anyhow!(
                    "cannot yield to work item {} because continuation {} already uses it as active work item",
                    record.id,
                    existing.id
                ));
            }
            if let Some(previous) = previous.as_ref() {
                let frame = WorkItemContinuationFrame::new_on_completed(
                    agent_id.clone(),
                    previous.id.clone(),
                    record.id.clone(),
                    state.current_turn_id.clone(),
                );
                continuation_created = Some(continuation_summary(&frame, "pick_work_item"));
                audit_events.push(AuditEvent::legacy(
                    "work_item_continuation_created",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "continuation": continuation_summary(&frame, "pick_work_item"),
                    }),
                ));
                continuation_records.push(frame);
            }
        } else if switching
            && previous_readiness == Some(WorkItemReadiness::Runnable)
            && normalized_reason.is_none()
            && !target_was_yielded
        {
            warnings.push(WorkItemFocusTransitionWarning {
                code: "missing_pick_reason_for_runnable_focus_switch".into(),
                message: "PickWorkItem switched away from a runnable current work item without a reason; include reason on future explicit focus overrides.".into(),
            });
        }
        let switch_kind = if !switching {
            "same_work_item"
        } else if continuation_created.is_some() {
            "yield_current"
        } else if continuation_resolved.is_some() {
            "explicit_yield_return"
        } else if previous_readiness == Some(WorkItemReadiness::Runnable) {
            "explicit_focus_override"
        } else {
            "explicit_focus_pick"
        }
        .to_string();
        let current_focus_mode = if current_readiness == WorkItemReadiness::Runnable {
            "runnable"
        } else {
            "inspection"
        }
        .to_string();
        let transition = WorkItemFocusTransition {
            previous_work_item_id: current_id.clone(),
            current_work_item_id: record.id.clone(),
            reason: normalized_reason,
            previous_readiness,
            current_readiness,
            switch_kind,
            current_focus_mode,
            blocker_cleared,
            cancelled_wait_condition_ids,
            warnings,
        };
        audit_events.push(AuditEvent::legacy(
            "work_item_picked",
            serde_json::json!({
                "agent_id": agent_id,
                "previous_work_item_id": transition.previous_work_item_id.clone(),
                "current_work_item_id": transition.current_work_item_id.clone(),
                "reason": transition.reason.clone(),
                "previous_readiness": transition.previous_readiness,
                "current_readiness": transition.current_readiness,
                "switch_kind": transition.switch_kind.clone(),
                "current_focus_mode": transition.current_focus_mode.clone(),
                "blocker_cleared": transition.blocker_cleared,
                "cancelled_wait_condition_ids": transition.cancelled_wait_condition_ids.clone(),
                "warnings": transition.warnings.clone(),
                "continuation_created": continuation_created.clone(),
                "continuation_resolved": continuation_resolved.clone(),
            }),
        ));
        let mut next_state = state.clone();
        next_state.current_work_item_id = Some(record.id.clone());
        next_state.current_turn_work_item_id = Some(record.id.clone());
        let work_items = expected_revision
            .map(|expected_revision| {
                vec![crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision,
                }]
            })
            .unwrap_or_default();
        let commit = self.inner.runtime_db.transitions().commit_work_item_focus(
            &crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
                agent_id: agent_id.clone(),
                work_items,
                wait_conditions,
                continuations: continuation_records,
                agent_state: crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(state)),
                    record: Box::new(next_state),
                },
                audit_events,
                index_changes,
                notify_scheduler: true,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(PickedWorkItem {
            previous_work_item: previous,
            current_work_item: record,
            transition,
            continuation_created,
            continuation_resolved,
        })
    }

    pub async fn update_work_item_fields(
        &self,
        work_item_id: String,
        objective: Option<String>,
        plan_status: Option<WorkItemPlanStatus>,
        _plan: Option<Option<String>>,
        todo_list: Option<Vec<TodoItem>>,
        blocked_by: Option<Option<String>>,
    ) -> Result<WorkItemRecord> {
        self.update_work_item_fields_with_recheck(
            work_item_id,
            objective,
            plan_status,
            _plan,
            todo_list,
            blocked_by,
            None,
        )
        .await
    }

    pub async fn update_work_item_fields_with_recheck(
        &self,
        work_item_id: String,
        objective: Option<String>,
        plan_status: Option<WorkItemPlanStatus>,
        _plan: Option<Option<String>>,
        todo_list: Option<Vec<TodoItem>>,
        blocked_by: Option<Option<String>>,
        recheck_after_ms: Option<u64>,
    ) -> Result<WorkItemRecord> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state == WorkItemState::Completed {
            return Err(RuntimeError::validation(
                "work_item_completed",
                format!("cannot update completed work item {work_item_id}"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into());
        }
        let mut record = existing.clone();
        let mut wrote_item = false;
        let previous_objective = record.objective.clone();
        let focus_release_reason = blocked_by
            .as_ref()
            .is_some_and(Option::is_some)
            .then_some("work_item_blocked");
        if let Some(objective) = objective {
            record.objective = objective;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        if let Some(plan_status) = plan_status {
            record.plan_status = plan_status;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        if let Some(todo_list) = todo_list {
            record.todo_list = todo_list;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        if let Some(blocked_by) = blocked_by {
            let now = self.now();
            record.blocked_by = blocked_by;
            match record.blocked_by {
                Some(_) => {
                    let recheck_after_ms = recheck_after_ms.unwrap_or(60 * 60 * 1000);
                    let recheck_after_ms = i64::try_from(recheck_after_ms).unwrap_or(i64::MAX);
                    let recheck_after = chrono::Duration::try_milliseconds(recheck_after_ms)
                        .unwrap_or(chrono::Duration::MAX);
                    record.recheck_at = Some(now + recheck_after);
                    record.recheck_consumed_at = None;
                }
                None => {
                    record.recheck_at = None;
                    record.recheck_consumed_at = None;
                }
            }
            record.updated_at = now;
            wrote_item = true;
        }
        if wrote_item {
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut record,
            )?;
            record.revision = existing.revision + 1;
            let mut audit_events = Vec::new();
            if plan_artifact_changed && record.plan_artifact != existing.plan_artifact {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                    audit_events.push(event);
                }
            }
            audit_events.push(self.work_item_written_event(
                "updated",
                &record,
                serde_json::json!({
                    "previous_objective_preview": crate::types::truncate_audit_preview(
                        &previous_objective,
                        600
                    ),
                    "objective_changed": previous_objective != record.objective,
                }),
            ));
            let mut state = self.agent_state().await?;
            let expected_state = state.clone();
            let mut agent_state = None;
            if let Some(reason) = focus_release_reason {
                let release_current =
                    state.current_work_item_id.as_deref() == Some(record.id.as_str());
                let release_turn =
                    state.current_turn_work_item_id.as_deref() == Some(record.id.as_str());
                if release_current {
                    state.current_work_item_id = None;
                }
                if release_turn {
                    state.current_turn_work_item_id = None;
                }
                if release_current || release_turn {
                    audit_events.push(AuditEvent::legacy(
                        "work_item_focus_released",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "work_item_id": record.id.as_str(),
                            "reason": reason,
                            "readiness": record.readiness(),
                            "revision": record.revision,
                        }),
                    ));
                    agent_state = Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_state)),
                        record: Box::new(state),
                    });
                }
            }
            let commit = self.inner.runtime_db.transitions().commit_work_item(
                &crate::runtime_db::transitions::WorkItemTransitionCommand {
                    agent_id,
                    mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                        record: record.clone(),
                        expected_revision: existing.revision,
                    },
                    agent_state,
                    audit_events,
                    index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                },
            )?;
            self.apply_transition_commit(commit).await;
        }
        Ok(record)
    }

    pub async fn consume_work_item_recheck(
        &self,
        work_item_id: &str,
    ) -> Result<Option<WorkItemRecord>> {
        let agent_id = self.agent_id().await?;
        let existing = match self.validate_owned_work_item(&agent_id, work_item_id) {
            Ok(record) => record,
            Err(_) => return Ok(None),
        };
        if existing.state != WorkItemState::Open || existing.blocked_by.is_none() {
            return Ok(None);
        }
        let Some(recheck_at) = existing.recheck_at else {
            return Ok(None);
        };
        if existing
            .recheck_consumed_at
            .is_some_and(|consumed_at| consumed_at >= recheck_at)
        {
            return Ok(Some(existing));
        }

        let consumed_at = self.now();
        let mut record = WorkItemRecord {
            revision: existing.revision + 1,
            recheck_consumed_at: Some(consumed_at),
            updated_at: consumed_at,
            ..existing
        };
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut record,
        )?;
        let mut audit_events = Vec::new();
        if plan_artifact_changed {
            if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                audit_events.push(event);
            }
        }
        audit_events.push(AuditEvent::legacy(
            "work_item_recheck_consumed",
            serde_json::json!({
                "work_item_id": record.id.clone(),
                "revision": record.revision,
                "recheck_at": record.recheck_at,
                "recheck_consumed_at": record.recheck_consumed_at,
            }),
        ));
        let commit = self.inner.runtime_db.transitions().commit_work_item(
            &crate::runtime_db::transitions::WorkItemTransitionCommand {
                agent_id,
                mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision: existing.revision,
                },
                agent_state: None,
                audit_events,
                index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                notify_scheduler: true,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(Some(record))
    }

    pub async fn complete_work_item(
        &self,
        work_item_id: String,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemRecord> {
        Ok(self
            .complete_work_item_with_continuation(work_item_id, warnings)
            .await?
            .work_item)
    }

    pub async fn complete_work_item_with_continuation(
        &self,
        work_item_id: String,
        warnings: Vec<serde_json::Value>,
    ) -> Result<CompletedWorkItem> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state == WorkItemState::Completed {
            return Ok(CompletedWorkItem {
                work_item: existing,
                continuation_resumed: None,
            });
        }
        let mut record = WorkItemRecord {
            revision: existing.revision + 1,
            state: WorkItemState::Completed,
            blocked_by: None,
            recheck_at: None,
            recheck_consumed_at: None,
            result_brief_id: existing.result_brief_id.clone(),
            result_summary: existing.result_summary.clone(),
            updated_at: Utc::now(),
            ..existing
        };
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut record,
        )?;
        let now = Utc::now();
        let active_waits = self
            .inner
            .storage
            .raw_active_wait_conditions_for_agent(&agent_id)?
            .into_iter()
            .filter(|condition| condition.work_item_id.as_deref() == Some(record.id.as_str()))
            .collect::<Vec<_>>();
        let mut wait_conditions = Vec::with_capacity(active_waits.len());
        let mut cancelled_wait_condition_ids = Vec::with_capacity(active_waits.len());
        for condition in active_waits {
            let mut cancelled = condition.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at = now;
            cancelled.cancelled_at = Some(now);
            cancelled_wait_condition_ids.push(condition.id);
            wait_conditions.push(cancelled);
        }
        let mut audit_events = Vec::new();
        if plan_artifact_changed {
            if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                audit_events.push(event);
            }
        }
        let mut state = self.agent_state().await?;
        let expected_state = state.clone();
        let release_current = state.current_work_item_id.as_deref() == Some(record.id.as_str());
        let release_turn = state.current_turn_work_item_id.as_deref() == Some(record.id.as_str());
        let mut continuation_records = Vec::new();
        let mut continuation_resumed = None;
        if release_current {
            state.current_work_item_id = None;
        }
        if release_turn {
            state.current_turn_work_item_id = None;
        }
        if release_current || release_turn {
            audit_events.push(AuditEvent::legacy(
                "work_item_focus_released",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id.as_str(),
                    "reason": "work_item_completed",
                    "readiness": record.readiness(),
                    "revision": record.revision,
                }),
            ));
        }
        // Pick atomically cancels conflicting active frames, so this lookup has
        // at most one resumable continuation for the completed WorkItem.
        if let Some(frame) = self
            .inner
            .storage
            .latest_active_work_item_continuation_for_active(&agent_id, &record.id)?
        {
            let suspended = self
                .inner
                .runtime_db
                .work_items()
                .latest(&frame.suspended_work_item_id)?;
            match suspended {
                Some(suspended)
                    if suspended.agent_id == agent_id && suspended.state == WorkItemState::Open =>
                {
                    let resumed = frame.resume("active_work_item_completed");
                    let summary = continuation_summary(&resumed, "active_work_item_completed");
                    state.current_work_item_id = Some(suspended.id.clone());
                    state.current_turn_work_item_id = Some(suspended.id.clone());
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_resumed",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "continuation": summary,
                            "completed_work_item_id": record.id,
                            "resumed_work_item_id": suspended.id,
                        }),
                    ));
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_scheduler_evidence",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "reason": "continuation_resumed",
                            "work_item_id": suspended.id,
                            "completed_work_item_id": record.id,
                            "continuation_frame_id": summary.frame_id,
                        }),
                    ));
                    continuation_resumed = Some(summary);
                    continuation_records.push(resumed);
                }
                suspended => {
                    let reason = if suspended.is_some() {
                        "suspended_work_item_not_open"
                    } else {
                        "suspended_work_item_missing"
                    };
                    let cancelled = frame.cancel(reason);
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_cancelled",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "continuation": continuation_summary(&cancelled, reason),
                            "suspended_work_item_state": suspended.map(|record| record.state),
                        }),
                    ));
                    continuation_records.push(cancelled);
                }
            }
        }
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id,
                    "reason": "work_item_completed",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        audit_events.push(self.work_item_written_event(
            "completed",
            &record,
            serde_json::json!({
                "warning_count": warnings.len(),
                "continuation_resumed": continuation_resumed,
            }),
        ));
        let commit = self.inner.runtime_db.transitions().commit_work_item_focus(
            &crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
                agent_id: agent_id.clone(),
                work_items: vec![crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision: existing.revision,
                }],
                wait_conditions,
                continuations: continuation_records,
                agent_state: crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(expected_state)),
                    record: Box::new(state),
                },
                audit_events,
                index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                notify_scheduler: true,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(CompletedWorkItem {
            work_item: record,
            continuation_resumed,
        })
    }

    pub async fn promote_work_item_completion_report(
        &self,
        work_item_id: String,
        report_text: String,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemRecord> {
        Ok(self
            .promote_work_item_completion_report_with_metadata(
                work_item_id,
                report_text,
                source_turn_index,
                source_round,
                warnings,
            )
            .await?
            .into_record())
    }

    pub(super) async fn promote_work_item_completion_report_with_metadata(
        &self,
        work_item_id: String,
        report_text: String,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemCompletionReportPromotionOutcome> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state != WorkItemState::Completed {
            return Err(anyhow!(
                "cannot promote completion report for open work item {}",
                work_item_id
            ));
        }
        let report_text = report_text.trim();
        if report_text.is_empty() {
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(
                existing,
            ));
        }
        if let Some(result_brief_id) = existing.result_brief_id.as_deref() {
            if let Some(brief) = self.inner.storage.read_brief_by_id(result_brief_id)? {
                if brief.text.trim() == report_text {
                    return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(
                        existing,
                    ));
                }
            }
        }
        let current_turn_id = {
            let guard = self.inner.agent.lock().await;
            guard.state.current_turn_id.clone()
        };
        let mut brief =
            BriefRecord::new(agent_id.clone(), BriefKind::Result, report_text, None, None);
        brief.work_item_id = Some(existing.id.clone());
        brief.workspace_id = existing.workspace_id.clone();
        brief.turn_index = source_turn_index;
        brief.turn_id = current_turn_id;
        self.persist_brief(&brief).await?;
        let record = WorkItemRecord {
            revision: existing.revision + 1,
            result_brief_id: Some(brief.id.clone()),
            updated_at: Utc::now(),
            ..existing
        };
        let commit = self.inner.runtime_db.transitions().commit_work_item(
            &crate::runtime_db::transitions::WorkItemTransitionCommand {
                agent_id: agent_id.clone(),
                mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision: record.revision - 1,
                },
                agent_state: None,
                audit_events: vec![AuditEvent::legacy(
                    "work_item_completion_report_promoted",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": record.id.clone(),
                        "revision": record.revision,
                        "source_turn_index": source_turn_index,
                        "source_round": source_round,
                        "text_preview": crate::tool::helpers::truncate_text(report_text, 600),
                        "warnings": warnings.clone(),
                        "warning_count": warnings.len(),
                        "brief_id": brief.id.clone(),
                    }),
                )],
                index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                notify_scheduler: false,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(WorkItemCompletionReportPromotionOutcome::Promoted(
            WorkItemCompletionReportPromotion {
                record,
                brief_id: brief.id,
            },
        ))
    }

    pub async fn record_work_item_completion_warning(
        &self,
        work_item_id: String,
        kind: &str,
        message: &str,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "work_item_completion_warning",
            serde_json::json!({
                "agent_id": agent_id,
                "work_item_id": work_item_id,
                "kind": kind,
                "message": message,
                "source_turn_index": source_turn_index,
                "source_round": source_round,
            }),
        ))?;
        Ok(())
    }

    pub(super) fn validate_owned_work_item(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<WorkItemRecord> {
        let record = self
            .inner
            .runtime_db
            .work_items()
            .latest(work_item_id)?
            .ok_or_else(|| {
                RuntimeError::not_found(
                    "work_item_not_found",
                    format!("unknown work item {work_item_id}"),
                )
                .with_safe_context("work_item_id", work_item_id)
            })?;
        if record.agent_id != agent_id {
            return Err(RuntimeError::policy(
                "work_item_access_denied",
                format!("work item {work_item_id} belongs to another agent"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_safe_context("agent_id", agent_id)
            .into());
        }
        Ok(record)
    }
}

fn task_not_found_error(task_id: &str) -> RuntimeError {
    RuntimeError::not_found("task_not_found", format!("task {task_id} not found"))
        .with_safe_context("task_id", task_id)
}

fn continuation_summary(
    frame: &WorkItemContinuationFrame,
    reason: impl Into<String>,
) -> WorkItemContinuationSummary {
    WorkItemContinuationSummary {
        frame_id: frame.id.clone(),
        suspended_work_item_id: frame.suspended_work_item_id.clone(),
        active_work_item_id: frame.active_work_item_id.clone(),
        return_policy: frame.return_policy,
        reason: reason.into(),
    }
}

fn task_status_name(state: &TaskStatus) -> &'static str {
    match state {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn append_task_owned_worktree_cleanup_note(
    text: &mut String,
    cleanup: &worktree::TaskOwnedWorktreeCleanup,
    worktree_path: &PathBuf,
) {
    match cleanup.status {
        worktree::TaskOwnedWorktreeCleanupStatus::Cleaned => {
            text.push_str("\nWorktree cleanup: auto-removed clean task-owned artifact.");
        }
        worktree::TaskOwnedWorktreeCleanupStatus::AlreadyRemoved => {
            text.push_str("\nWorktree cleanup: already removed.");
        }
        worktree::TaskOwnedWorktreeCleanupStatus::Retained => {
            if cleanup.changed_files.is_empty() {
                text.push_str(
                    "\nWorktree retained for review: cleanup skipped after artifact metadata mismatch.",
                );
            } else {
                text.push_str(&format!(
                    "\nWorktree retained for review: {} changes detected. Use 'git worktree remove {}' when done.",
                    cleanup.changed_files.len(),
                    worktree_path.display()
                ));
            }
        }
        worktree::TaskOwnedWorktreeCleanupStatus::Failed => {
            text.push_str("\nWorktree cleanup: failed; artifact retained for manual inspection.");
        }
    }
}

pub(super) fn task_from_message(message: &MessageEnvelope, agent_id: &str) -> Result<TaskRecord> {
    let metadata = message.metadata.as_ref();
    let task_id = metadata
        .and_then(|value| value.get("task_id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task message missing metadata.task_id"))?
        .to_string();
    let task_kind = metadata
        .and_then(|value| value.get("task_kind"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task message missing metadata.task_kind"))
        .and_then(|kind| {
            serde_json::from_value::<TaskKind>(serde_json::Value::String(kind.to_string()))
                .with_context(|| format!("unknown task kind {kind:?}"))
        })?;
    let task_status = metadata
        .and_then(|value| value.get("task_status"))
        .and_then(|value| value.as_str());
    let status = match task_status.unwrap_or(match message.kind {
        MessageKind::TaskStatus => "running",
        MessageKind::TaskResult => "completed",
        _ => "queued",
    }) {
        "running" => TaskStatus::Running,
        "cancelling" => TaskStatus::Cancelling,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "interrupted" => TaskStatus::Interrupted,
        _ => TaskStatus::Queued,
    };
    let summary = metadata
        .and_then(|value| value.get("task_summary"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| Some(message_text(&message.body)));

    let mut detail = metadata.and_then(|value| value.get("task_detail")).cloned();
    if let Some(parent_turn_id) = message.turn_id.as_ref() {
        let detail = detail.get_or_insert_with(|| serde_json::json!({}));
        if let Some(detail) = detail.as_object_mut() {
            detail
                .entry("parent_turn_id")
                .or_insert_with(|| serde_json::json!(parent_turn_id));
        }
    }
    if message.correlation_id.is_some() || message.causation_id.is_some() {
        let detail = detail.get_or_insert_with(|| serde_json::json!({}));
        if let Some(detail) = detail.as_object_mut() {
            if let Some(correlation_id) = message.correlation_id.as_ref() {
                detail
                    .entry("correlation_id")
                    .or_insert_with(|| serde_json::json!(correlation_id));
            }
            if let Some(causation_id) = message.causation_id.as_ref() {
                detail
                    .entry("causation_id")
                    .or_insert_with(|| serde_json::json!(causation_id));
            }
        }
    }

    Ok(TaskRecord {
        id: task_id,
        agent_id: agent_id.to_string(),
        kind: task_kind,
        status,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: metadata
            .and_then(|value| value.get("work_item_id"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .or_else(|| message.work_item_id.clone()),
        summary,
        detail,
        recovery: metadata
            .and_then(|value| value.get("task_recovery"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?,
    })
}

fn detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn latest_task_message_in(
    messages: Vec<MessageEnvelope>,
    task_id: &str,
) -> Option<TaskMessageSnapshot> {
    let mut status_candidate = None;

    for message in messages.into_iter().rev() {
        if !matches!(&message.origin, MessageOrigin::Task { .. }) {
            continue;
        }
        let metadata = match &message.metadata {
            Some(metadata)
                if metadata.get("task_id").and_then(|value| value.as_str()) == Some(task_id) =>
            {
                metadata
            }
            _ => continue,
        };

        let snapshot = TaskMessageSnapshot {
            state: task_status_from_message(&message, metadata),
            text: render_task_message_body(&message.body),
        };

        if message.kind == MessageKind::TaskResult {
            return Some(snapshot);
        }
        if status_candidate.is_none() {
            status_candidate = Some(snapshot);
        }
    }

    status_candidate
}

fn effective_task_output_status(
    task_status: &TaskStatus,
    latest_message: Option<&TaskMessageSnapshot>,
) -> TaskStatus {
    if is_terminal_task_status(task_status) || matches!(task_status, TaskStatus::Cancelling) {
        return task_status.clone();
    }

    match latest_message {
        Some(message) => message.state.clone(),
        None => task_status.clone(),
    }
}

fn task_output_ready(task: &TaskRecord, state: &TaskStatus) -> bool {
    if matches!(
        state,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    ) {
        return false;
    }

    if task.kind != TaskKind::CommandTask {
        return true;
    }

    if task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("terminal_snapshot_ready"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if is_terminal_task_status(&task.status)
        && task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("output_path"))
            .and_then(|value| value.as_str())
            .is_some()
    {
        return true;
    }

    task.detail.as_ref().is_some_and(|detail| {
        detail
            .get("exit_status")
            .is_some_and(|value| !value.is_null())
            || detail
                .get("error")
                .and_then(|value| value.as_str())
                .is_some()
            || detail_string(&task.detail, "output_summary").is_some()
            || detail_string(&task.detail, "initial_output").is_some()
    })
}

fn task_failure_artifact(
    task: &TaskRecord,
    state: &TaskStatus,
    output: &str,
    output_path: Option<&str>,
    exit_status: Option<i32>,
) -> Option<FailureArtifact> {
    if !matches!(
        state,
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
    ) {
        return None;
    }

    let mut metadata = BTreeMap::new();
    if let Some(cmd) = detail_string(&task.detail, "cmd") {
        metadata.insert("command".into(), cmd);
    }
    if let Some(path) = output_path {
        metadata.insert("output_path".into(), path.to_string());
    } else if let Some(path) = detail_string(&task.detail, "output_path") {
        metadata.insert("output_path".into(), path);
    }
    let has_error = detail_string(&task.detail, "error").is_some();
    let source_chain = detail_string(&task.detail, "error")
        .map(|error| sanitize_runtime_error_text(&error))
        .filter(|error| !error.is_empty())
        .into_iter()
        .collect();
    if has_error {
        metadata.insert("error_present".into(), "true".into());
    }
    if let Some(task_status) = detail_string(&task.detail, "task_status") {
        metadata.insert("task_status".into(), task_status);
    }

    let (kind, summary, exit_status) = if task.kind == TaskKind::CommandTask {
        let kind = if let Some(code) = exit_status {
            if code != 0 {
                "command_task_exit_nonzero"
            } else if matches!(state, TaskStatus::Interrupted) {
                "command_task_interrupted"
            } else {
                "command_task_failed"
            }
        } else if matches!(state, TaskStatus::Interrupted) {
            "command_task_interrupted"
        } else if has_error {
            "command_task_error"
        } else if output.is_empty() {
            "command_task_failed"
        } else {
            "command_task_output"
        };

        let summary = if matches!(state, TaskStatus::Interrupted) {
            "command task interrupted by runtime restart".to_string()
        } else if let Some(code) = exit_status {
            format!("command task exited with status {code}")
        } else if has_error && metadata.contains_key("output_path") {
            "command task failed; inspect output_path for details".to_string()
        } else if has_error {
            "command task failed before producing output".to_string()
        } else if !output.is_empty() && metadata.contains_key("output_path") {
            "command task failed; inspect output_path for details".to_string()
        } else if !output.is_empty() {
            "command task failed and produced output".to_string()
        } else {
            task.summary
                .as_deref()
                .map(ToString::to_string)
                .unwrap_or_else(|| task.kind.as_str().to_string())
        };

        (kind, summary, exit_status)
    } else {
        let kind = match state {
            TaskStatus::Cancelled => "task_cancelled",
            TaskStatus::Interrupted => "task_interrupted",
            _ => "task_failed",
        };
        let summary = task
            .summary
            .as_deref()
            .map(ToString::to_string)
            .unwrap_or_else(|| task.kind.as_str().to_string());
        (kind, summary, None)
    };

    Some(FailureArtifact {
        category: FailureArtifactCategory::Task,
        kind: kind.to_string(),
        summary,
        domain: Some(RuntimeErrorDomain::Task),
        retryable: Some(false),
        recovery_hint: None,
        provider: None,
        model_ref: None,
        status: None,
        task_id: Some(task.id.clone()),
        exit_status,
        source_chain,
        context: Box::new(RuntimeErrorContext {
            message_id: task.parent_message_id.clone(),
            turn_id: detail_string(&task.detail, "parent_turn_id"),
            work_item_id: task.work_item_id.clone(),
            task_id: Some(task.id.clone()),
            correlation_id: detail_string(&task.detail, "correlation_id"),
            causation_id: detail_string(&task.detail, "causation_id"),
            ..RuntimeErrorContext::default()
        }),
        metadata,
    })
}

fn task_output_artifacts(output_path: Option<&str>) -> (Vec<ToolArtifactRef>, Option<usize>) {
    let Some(path) = output_path else {
        return (Vec::new(), None);
    };
    (
        vec![ToolArtifactRef {
            path: path.to_string(),
        }],
        Some(0),
    )
}

fn is_terminal_task_status(state: &TaskStatus) -> bool {
    matches!(
        state,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

fn render_task_message_body(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn task_status_from_message(message: &MessageEnvelope, metadata: &serde_json::Value) -> TaskStatus {
    match metadata
        .get("task_status")
        .and_then(|value| value.as_str())
        .unwrap_or(match message.kind {
            MessageKind::TaskStatus => "running",
            MessageKind::TaskResult => "completed",
            _ => "queued",
        }) {
        "running" => TaskStatus::Running,
        "cancelling" => TaskStatus::Cancelling,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "interrupted" => TaskStatus::Interrupted,
        _ => TaskStatus::Queued,
    }
}
