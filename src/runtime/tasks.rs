use super::message_dispatch::message_text;
use super::*;
use crate::runtime::task_state_reducer::has_blocking_active_tasks;
use crate::tool::helpers::truncate_output_to_char_budget;
use crate::tool::ToolError;
use crate::types::{
    AgentProfilePreset, ChildAgentWorkspaceMode, CommandTaskStatusSnapshot, FailureArtifact,
    FailureArtifactCategory, SpawnAgentResult, TaskHandle, TaskInputResult, TaskKind,
    TaskListEntry, TaskOutputResult, TaskOutputRetrievalStatus, TaskOutputSnapshot,
    TaskStatusSnapshot, ToolArtifactRef, WorkItemRecord, WorkItemStatus, WorkPlanItem,
    WorkPlanSnapshot, CHILD_AGENT_TASK_KIND,
};
use std::collections::BTreeMap;

const TASK_OUTPUT_POLL_INTERVAL_MS: u64 = 100;
const TASK_OUTPUT_MESSAGE_SCAN_LIMIT: usize = 200;
const TASK_OUTPUT_PREVIEW_CHAR_BUDGET: usize = 8_000;

#[derive(Debug, Clone)]
struct TaskMessageSnapshot {
    status: TaskStatus,
    text: String,
}

fn child_agent_task_detail(workspace_mode: ChildAgentWorkspaceMode) -> serde_json::Value {
    serde_json::json!({
        "wait_policy": crate::types::TaskWaitPolicy::Blocking,
        "workspace_mode": workspace_mode,
    })
}

impl RuntimeHandle {
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
        trust: TrustLevel,
        workspace_mode: ChildAgentWorkspaceMode,
    ) -> Result<TaskRecord> {
        match workspace_mode {
            ChildAgentWorkspaceMode::Inherit => {
                self.schedule_inherited_child_agent_task(summary, prompt, trust)
                    .await
            }
            ChildAgentWorkspaceMode::Worktree => {
                self.schedule_worktree_child_agent_task(summary, prompt, trust)
                    .await
            }
        }
    }

    async fn schedule_inherited_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        trust: TrustLevel,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        let agent_id = self.agent_id().await?;
        let workspace_mode = ChildAgentWorkspaceMode::Inherit;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            trust: trust.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some(summary.clone()),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.inner.storage.append_task(&task)?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("task_created", to_json_value(&task)))?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !guard.state.active_task_ids.contains(&task.id) {
                guard.state.active_task_ids.push(task.id.clone());
            }
            if task.is_blocking()
                && !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                )
            {
                guard.state.status = AgentStatus::AwaitingTask;
            }
            self.inner.storage.write_agent(&guard.state)?;
        }

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, trust, false, false)
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
                })),
                ..MessageEnvelope::new(
                    agent_id.clone(),
                    MessageKind::TaskStatus,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    trust.clone(),
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
                .run_subagent_prompt(&agent_id, &prompt, &trust)
                .await;
            let (text, status) = match subagent_result {
                Ok(text) => (text, "completed"),
                Err(err) => (format!("child agent failed: {err:#}"), "failed"),
            };

            let result_message = MessageEnvelope {
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": status,
                    "task_summary": task_record.summary,
                    "task_detail": task_record.detail,
                    "task_recovery": task_record.recovery,
                })),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    trust,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
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
        summary: String,
        prompt: String,
        trust: TrustLevel,
        preset: AgentProfilePreset,
        agent_id: Option<String>,
        worktree: bool,
        template: Option<String>,
    ) -> Result<SpawnAgentResult> {
        if !self.supports_child_agent_spawning() {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "unsupported_runtime_capability",
                    "SpawnAgent is not available in this runtime",
                )
                .with_details(serde_json::json!({
                    "tool_name": "SpawnAgent",
                    "required_capability": "child_agent_spawning",
                }))
                .with_recovery_hint(
                    "run SpawnAgent from a host-managed runtime with child-agent support",
                ),
            ));
        }
        let bridge = self
            .inner
            .host_bridge
            .clone()
            .expect("spawn agent support should imply host bridge");

        match preset {
            AgentProfilePreset::PrivateChild => {
                let task = self
                    .create_child_supervision_task(summary, prompt.clone(), trust.clone(), worktree)
                    .await?;

                let spawned = match bridge
                    .spawn_child_task(
                        self.clone(),
                        &task,
                        prompt,
                        trust.clone(),
                        worktree,
                        template.clone(),
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
                self.inner.storage.append_task(&queued_task)?;
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_child_spawned",
                    to_json_value(&queued_task),
                ))?;

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
                            trust,
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

                Ok(SpawnAgentResult {
                    agent_id: spawned.child_agent_id.clone(),
                    task_handle: Some(TaskHandle::from_task_record(&queued_task, None)),
                    summary_text: Some(format!(
                        "spawned private child agent {} with supervising task handle",
                        spawned.child_agent_id
                    )),
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
                    .spawn_public_named_agent(self.clone(), &agent_id, prompt, trust, template)
                    .await?;

                Ok(SpawnAgentResult {
                    agent_id: spawned_agent_id.clone(),
                    task_handle: None,
                    summary_text: Some(format!(
                        "spawned public named agent {} without a supervising task handle",
                        spawned_agent_id
                    )),
                })
            }
        }
    }

    async fn schedule_worktree_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        trust: TrustLevel,
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
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            trust: trust.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some(summary.clone()),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.inner.storage.append_task(&task)?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("task_created", to_json_value(&task)))?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !guard.state.active_task_ids.contains(&task.id) {
                guard.state.active_task_ids.push(task.id.clone());
            }
            if task.is_blocking()
                && !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                )
            {
                guard.state.status = AgentStatus::AwaitingTask;
            }
            self.inner.storage.write_agent(&guard.state)?;
        }

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, trust, true, false)
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
                    &trust,
                    &task_record.id,
                )
                .await;

            let (mut text, status, mut task_detail, worktree_path): (
                String,
                &str,
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
                        if result.failed { "failed" } else { "completed" },
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
                    "failed",
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

            let mut metadata = serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": status,
                "task_summary": task_record.summary,
                "task_detail": task_detail,
                "task_recovery": task_record.recovery,
            });
            if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
                metadata["worktree"] = worktree;
            }
            let result_message = MessageEnvelope {
                metadata: Some(metadata),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    trust,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
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
        trust: TrustLevel,
        worktree: bool,
        recovered: bool,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Err(anyhow!("child agent runtime requires a host bridge"));
        };
        let agent_id = self.agent_id().await?;

        let existing_detail = task_record.detail.clone();
        let existing_child_id = detail_string(&existing_detail, "child_agent_id");
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
                                trust.clone(),
                                worktree,
                                None,
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
                            trust.clone(),
                            worktree,
                            None,
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
                        metadata: Some(serde_json::json!({
                            "task_id": task_record.id,
                            "task_kind": task_record.kind,
                            "task_status": "failed",
                            "task_summary": task_record.summary,
                            "task_detail": task_record.detail,
                            "task_recovery": task_record.recovery,
                        })),
                        ..MessageEnvelope::new(
                            agent_id.clone(),
                            MessageKind::TaskResult,
                            MessageOrigin::Task {
                                task_id: task_record.id.clone(),
                            },
                            trust.clone(),
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
                    trust,
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
        trust: TrustLevel,
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
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt,
            trust: trust.clone(),
            workspace_mode,
        };
        let task = TaskRecord {
            id: Uuid::new_v4().to_string(),
            agent_id,
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some(summary),
            detail: Some(child_agent_task_detail(workspace_mode)),
            recovery: Some(recovery),
        };
        self.inner.storage.append_task(&task)?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("task_created", to_json_value(&task)))?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !guard.state.active_task_ids.contains(&task.id) {
                guard.state.active_task_ids.push(task.id.clone());
            }
            if task.is_blocking()
                && !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                )
            {
                guard.state.status = AgentStatus::AwaitingTask;
            }
            self.inner.storage.write_agent(&guard.state)?;
        }
        Ok(task)
    }

    async fn monitor_spawned_child_agent_task(
        &self,
        task_record: TaskRecord,
        trust: TrustLevel,
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
                "task_detail": task_detail,
            })),
            ..MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                trust.clone(),
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
                match result.status {
                    TaskStatus::Completed => "completed",
                    TaskStatus::Failed => "failed",
                    TaskStatus::Cancelled => "cancelled",
                    TaskStatus::Interrupted => "interrupted",
                    TaskStatus::Cancelling => "cancelling",
                    TaskStatus::Running => "running",
                    TaskStatus::Queued => "queued",
                },
                result.task_detail.unwrap_or(task_detail_for_result.clone()),
            ),
            Err(err) => (
                format!("child agent failed: {err:#}"),
                "failed",
                task_detail_for_result,
            ),
        };

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
                        failed: status == "failed",
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

        let mut metadata = serde_json::json!({
            "task_id": task_record.id,
            "task_kind": task_record.kind,
            "task_status": status,
            "task_summary": task_record.summary,
            "task_recovery": task_record.recovery,
            "task_detail": task_detail,
        });
        if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
            metadata["worktree"] = worktree;
        }
        let result_message = MessageEnvelope {
            metadata: Some(metadata),
            ..MessageEnvelope::new(
                agent_id,
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                trust,
                Priority::Next,
                MessageBody::Text { text },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            )
        };
        let _ = self.enqueue(result_message).await;
        Ok(())
    }

    pub async fn schedule_timer(
        &self,
        duration_ms: u64,
        interval_ms: Option<u64>,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let created_at = Utc::now();
        let timer = TimerRecord {
            id: Uuid::new_v4().to_string(),
            agent_id: self.agent_id().await?,
            created_at,
            duration_ms,
            interval_ms,
            repeat: interval_ms.is_some(),
            status: TimerStatus::Active,
            summary,
            next_fire_at: Some(advance_time(created_at, duration_ms)?),
            last_fired_at: None,
            fire_count: 0,
        };
        self.inner.storage.append_timer(&timer)?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("timer_created", to_json_value(&timer)))?;
        self.spawn_timer_loop(timer.clone());

        Ok(timer)
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
            let (prompt, trust, worktree) = match task.recovery.as_ref() {
                Some(TaskRecoverySpec::ChildAgentTask {
                    prompt,
                    trust,
                    workspace_mode,
                    ..
                }) => (prompt.clone(), trust.clone(), workspace_mode.is_worktree()),
                Some(TaskRecoverySpec::SubagentTask { prompt, trust, .. }) => {
                    (prompt.clone(), trust.clone(), false)
                }
                Some(TaskRecoverySpec::WorktreeSubagentTask { prompt, trust, .. }) => {
                    (prompt.clone(), trust.clone(), true)
                }
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
                .spawn_child_agent_task(task.clone(), prompt, trust, worktree, true)
                .await
            {
                Ok(()) => reattached.push(task),
                Err(error) => {
                    self.inner.storage.append_event(&AuditEvent::new(
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

    pub(crate) async fn recover_active_timers(&self, timers: Vec<TimerRecord>) -> Result<()> {
        for timer in timers {
            self.recover_timer(timer).await?;
        }
        Ok(())
    }

    fn spawn_timer_loop(&self, timer: TimerRecord) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut timer = timer;
            loop {
                let Some(next_fire_at) = timer.next_fire_at else {
                    break;
                };
                let now = Utc::now();
                if next_fire_at > now {
                    let wait = (next_fire_at - now)
                        .to_std()
                        .unwrap_or_else(|_| Duration::from_millis(0));
                    tokio::time::sleep(wait).await;
                }
                if let Err(err) = runtime.fire_timer_record(&mut timer).await {
                    let _ = runtime.inner.storage.append_event(&AuditEvent::new(
                        "timer_fire_failed",
                        serde_json::json!({
                            "timer_id": timer.id,
                            "error": err.to_string(),
                        }),
                    ));
                    break;
                }
                if timer.status != TimerStatus::Active {
                    break;
                }
            }
        });
    }

    pub async fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks = self.inner.storage.latest_task_records()?;
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub async fn latest_task_list_entries(&self) -> Result<Vec<TaskListEntry>> {
        Ok(self
            .latest_task_records()
            .await?
            .into_iter()
            .map(|task| {
                let wait_policy = task.wait_policy();
                TaskListEntry {
                    id: task.id,
                    kind: task.kind.as_str().to_string(),
                    status: task.status,
                    summary: task.summary,
                    updated_at: task.updated_at,
                    wait_policy,
                }
            })
            .collect())
    }

    async fn persist_task_status_direct(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.inner.storage.append_task(task)?;
        {
            let mut guard = self.inner.agent.lock().await;
            if is_terminal_task_status(&task.status) {
                guard.state.active_task_ids.retain(|id| id != &task.id);
                if !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                ) {
                    guard.state.status = if has_blocking_active_tasks(
                        &self.inner.storage,
                        &guard.state.active_task_ids,
                    )? {
                        AgentStatus::AwaitingTask
                    } else {
                        AgentStatus::AwakeIdle
                    };
                }
            } else {
                if !guard.state.active_task_ids.contains(&task.id) {
                    guard.state.active_task_ids.push(task.id.clone());
                }
                if task.is_blocking()
                    && !matches!(
                        guard.state.status,
                        AgentStatus::Paused | AgentStatus::Stopped
                    )
                {
                    guard.state.status = AgentStatus::AwaitingTask;
                }
            }
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner
            .storage
            .append_event(&AuditEvent::new(event_kind, to_json_value(task)))?;
        Ok(())
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

    pub async fn task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        Ok(self
            .inner
            .storage
            .latest_task_records()?
            .into_iter()
            .find(|task| task.id == task_id))
    }

    pub async fn task_status_snapshot(&self, task_id: &str) -> Result<TaskStatusSnapshot> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {} not found", task_id))?;
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
                .ok_or_else(|| anyhow!("task {} not found", task_id))?;
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

    async fn recover_timer(&self, timer: TimerRecord) -> Result<()> {
        let timer = normalize_recovered_timer(timer);
        let now = Utc::now();
        if timer
            .next_fire_at
            .is_some_and(|next_fire_at| next_fire_at <= now)
        {
            let mut overdue = timer.clone();
            self.fire_timer_record(&mut overdue).await?;
            if overdue.status == TimerStatus::Active {
                self.spawn_timer_loop(overdue);
            }
        } else {
            self.spawn_timer_loop(timer);
        }
        Ok(())
    }

    async fn fire_timer_record(&self, timer: &mut TimerRecord) -> Result<()> {
        let message = MessageEnvelope {
            metadata: Some(serde_json::json!({ "timer_id": timer.id })),
            ..MessageEnvelope::new(
                timer.agent_id.clone(),
                MessageKind::TimerTick,
                MessageOrigin::Timer {
                    timer_id: timer.id.clone(),
                },
                TrustLevel::TrustedSystem,
                Priority::Next,
                MessageBody::Text {
                    text: timer
                        .summary
                        .clone()
                        .unwrap_or_else(|| format!("timer {} fired", timer.id)),
                },
            )
            .with_admission(
                MessageDeliverySurface::TimerScheduler,
                AdmissionContext::RuntimeOwned,
            )
        };
        self.enqueue(message).await?;

        let fired_at = Utc::now();
        timer.last_fired_at = Some(fired_at);
        timer.fire_count += 1;
        if let Some(interval_ms) = timer.interval_ms {
            timer.status = TimerStatus::Active;
            timer.next_fire_at = Some(advance_time(fired_at, interval_ms)?);
        } else {
            timer.status = TimerStatus::Completed;
            timer.next_fire_at = None;
        }
        self.inner.storage.append_timer(timer)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "timer_fired",
            serde_json::json!({
                "timer_id": timer.id,
                "status": timer.status,
                "fire_count": timer.fire_count,
                "next_fire_at": timer.next_fire_at,
            }),
        ))?;
        Ok(())
    }

    pub async fn stop_task(&self, task_id: &str, trust: &TrustLevel) -> Result<TaskRecord> {
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
                    return Err(anyhow!("task {} has an unexpected async handle", task_id));
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
                        return Err(anyhow!("task {} is not currently running", task_id));
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
        let stopped_kind = existing
            .as_ref()
            .map(|task| task.kind)
            .ok_or_else(|| anyhow!("task {} is not currently running", task_id))?;
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
            return self.finish_stopped_task(agent_id, stopped, trust).await;
        }
        Ok(stopped)
    }

    pub async fn task_input(&self, task_id: &str, input: &str) -> Result<TaskInputResult> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {} not found", task_id))?;
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
        trust: &TrustLevel,
    ) -> Result<TaskInputResult> {
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {} not found", task_id))?;
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
                .deliver_child_task_input(&task, snapshot, input, trust)
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
            .map_err(|_| anyhow!("task {} is not currently running", task.id))?;
        let bytes_written = response_rx
            .await
            .map_err(|_| anyhow!("task {} input delivery was interrupted", task.id))?
            .map_err(|error| anyhow!("task {} input delivery failed: {}", task.id, error))?;

        let input_target = command
            .as_ref()
            .and_then(|value| value.input_target.clone())
            .unwrap_or_else(|| "stdin".into());
        self.inner.storage.append_event(&AuditEvent::new(
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
        trust: &TrustLevel,
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
                trust.clone(),
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
        self.inner.storage.append_event(&AuditEvent::new(
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
            summary_text: Some(format!("delivered input to task {}", task.id)),
        })
    }

    async fn finish_stopped_task(
        &self,
        agent_id: String,
        stopped: TaskRecord,
        trust: &TrustLevel,
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
                    trust.clone(),
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

    pub async fn update_work_item(
        &self,
        work_item_id: Option<String>,
        delivery_target: String,
        status: WorkItemStatus,
        summary: Option<String>,
        progress_note: Option<String>,
        parent_id: Option<String>,
    ) -> Result<WorkItemRecord> {
        let agent_id = self.agent_id().await?;
        let (action, record) = if let Some(work_item_id) = work_item_id {
            let existing = self
                .inner
                .storage
                .latest_work_item(&work_item_id)?
                .ok_or_else(|| anyhow!("unknown work item {}", work_item_id))?;
            if existing.agent_id != agent_id {
                return Err(anyhow!(
                    "work item {} belongs to another agent",
                    work_item_id
                ));
            }
            (
                "updated",
                WorkItemRecord {
                    id: existing.id,
                    agent_id,
                    workspace_id: existing.workspace_id,
                    parent_id,
                    delivery_target,
                    status,
                    summary,
                    progress_note,
                    created_at: existing.created_at,
                    updated_at: Utc::now(),
                },
            )
        } else {
            let mut record = WorkItemRecord::new(agent_id, delivery_target, status);
            record.workspace_id = self
                .agent_state()
                .await?
                .active_workspace_entry
                .map(|entry| entry.workspace_id)
                .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string());
            record.summary = summary;
            record.progress_note = progress_note;
            record.parent_id = parent_id;
            ("created", record)
        };
        self.inner.storage.append_work_item(&record)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "work_item_written",
            serde_json::json!({
                "action": action,
                "record": record,
            }),
        ))?;
        self.inner.notify.notify_one();
        Ok(record)
    }

    pub async fn update_work_plan(
        &self,
        work_item_id: String,
        items: Vec<WorkPlanItem>,
    ) -> Result<WorkPlanSnapshot> {
        let agent_id = self.agent_id().await?;
        let work_item = self
            .inner
            .storage
            .latest_work_item(&work_item_id)?
            .ok_or_else(|| anyhow!("unknown work item {}", work_item_id))?;
        if work_item.agent_id != agent_id {
            return Err(anyhow!(
                "work item {} belongs to another agent",
                work_item_id
            ));
        }

        let snapshot = WorkPlanSnapshot::new(agent_id, work_item_id, items);
        self.inner.storage.append_work_plan(&snapshot)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "work_plan_snapshot_written",
            to_json_value(&snapshot),
        ))?;
        Ok(snapshot)
    }
}

fn task_status_name(status: &TaskStatus) -> &'static str {
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

fn advance_time(base: chrono::DateTime<Utc>, delta_ms: u64) -> Result<chrono::DateTime<Utc>> {
    let delta_ms = i64::try_from(delta_ms).context("duration_ms exceeds supported timer range")?;
    let delta = chrono::Duration::try_milliseconds(delta_ms)
        .ok_or_else(|| anyhow!("duration_ms exceeds supported timer range"))?;
    Ok(base + delta)
}

fn normalize_recovered_timer(mut timer: TimerRecord) -> TimerRecord {
    if timer.next_fire_at.is_some() {
        return timer;
    }

    let anchor = timer.last_fired_at.unwrap_or(timer.created_at);
    let fallback_ms = timer.interval_ms.unwrap_or(timer.duration_ms);
    timer.next_fire_at = advance_time(anchor, fallback_ms).ok().or(Some(Utc::now()));
    timer
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

    Ok(TaskRecord {
        id: task_id,
        agent_id: agent_id.to_string(),
        kind: task_kind,
        status,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        summary,
        detail: metadata.and_then(|value| value.get("task_detail")).cloned(),
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
            status: task_status_from_message(&message, metadata),
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
    if is_terminal_task_status(task_status) {
        return task_status.clone();
    }

    match latest_message {
        Some(message) => message.status.clone(),
        None => task_status.clone(),
    }
}

fn task_output_ready(task: &TaskRecord, status: &TaskStatus) -> bool {
    if matches!(
        status,
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
    status: &TaskStatus,
    output: &str,
    output_path: Option<&str>,
    exit_status: Option<i32>,
) -> Option<FailureArtifact> {
    if !matches!(
        status,
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
            } else if matches!(status, TaskStatus::Interrupted) {
                "command_task_interrupted"
            } else {
                "command_task_failed"
            }
        } else if matches!(status, TaskStatus::Interrupted) {
            "command_task_interrupted"
        } else if has_error {
            "command_task_error"
        } else if output.is_empty() {
            "command_task_failed"
        } else {
            "command_task_output"
        };

        let summary = if matches!(status, TaskStatus::Interrupted) {
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
        let kind = match status {
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
        provider: None,
        model_ref: None,
        status: None,
        task_id: Some(task.id.clone()),
        exit_status,
        source_chain: Vec::new(),
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

fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
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
