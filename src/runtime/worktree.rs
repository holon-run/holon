use super::{task_state_reducer, *};
use std::ffi::OsString;
#[path = "worktree_cleanup.rs"]
mod cleanup;
#[cfg(test)]
pub(crate) use cleanup::cleanup_completion_worktree_for_test;
pub(crate) use cleanup::{cleanup_frozen_worktree, FrozenWorktreeCleanup, ObservedWorktreeCleanup};

use crate::system::{
    file::FileHost, CaptureSpec, ExecutionScopeKind, ProcessHost, ProcessPurpose, ProcessRequest,
    ProgramInvocation, StdioSpec,
};
use crate::types::{WorkspaceProjectionMetadata, WorktreeSession};

#[derive(Debug, Clone)]
pub(crate) struct TaskOwnedWorktreeCleanup {
    pub(super) changed_files: Vec<String>,
    pub(super) status: TaskOwnedWorktreeCleanupStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskOwnedWorktreeCleanupStatus {
    Cleaned,
    AlreadyRemoved,
    Retained,
    Failed,
}

impl TaskOwnedWorktreeCleanupStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            TaskOwnedWorktreeCleanupStatus::Cleaned => "cleaned",
            TaskOwnedWorktreeCleanupStatus::AlreadyRemoved => "already_removed",
            TaskOwnedWorktreeCleanupStatus::Retained => "retained",
            TaskOwnedWorktreeCleanupStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct TaskOwnedWorktreeArtifact {
    worktree_path: PathBuf,
    worktree_branch: String,
}

#[derive(Debug, Clone)]
struct TaskOwnedWorktreeRemoveOutcome {
    branch_deleted: bool,
    branch_delete_error: Option<String>,
}

impl RuntimeHandle {
    async fn ensure_managed_worktrees_supported(&self, surface: &str) -> Result<()> {
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
            surface,
        )
    }

    pub(crate) async fn prepare_managed_worktree(
        &self,
        branch_name: &str,
    ) -> Result<ManagedWorktreeSeed> {
        self.ensure_managed_worktrees_supported("prepare_managed_worktree")
            .await?;
        let original_cwd = self.workspace_root();
        let original_branch = git_stdout(
            self,
            &original_cwd,
            &["rev-parse", "--abbrev-ref", "HEAD"],
            "failed to determine current git branch",
        )
        .await?;
        if original_branch == "HEAD" {
            return Err(anyhow!(
                "detached HEAD is not supported for git_worktree_root"
            ));
        }

        let repo_name = original_cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo");
        let managed_root = original_cwd
            .parent()
            .unwrap_or(original_cwd.as_path())
            .join(format!(".holon-worktrees-{repo_name}"));
        let system = self.system();
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        system
            .create_dir_all(&execution, &managed_root)
            .await
            .context("failed to create managed worktree directory")?;

        let worktree_path = managed_root.join(sanitize_branch_name(branch_name));
        if worktree_path.exists() {
            return Err(anyhow!(
                "managed worktree path already exists: {}",
                worktree_path.display()
            ));
        }

        let output = system
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("worktree"),
                            OsString::from("add"),
                            OsString::from("-b"),
                            OsString::from(branch_name),
                            worktree_path.as_os_str().to_os_string(),
                        ],
                    },
                    cwd: Some(original_cwd.clone()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::WorktreeSetup,
                },
            )
            .await
            .context("failed to create git worktree")?;
        if !output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(anyhow!("git worktree add failed: {stderr}"));
        }

        Ok(ManagedWorktreeSeed {
            original_cwd,
            original_branch,
            worktree_path,
            worktree_branch: branch_name.to_string(),
        })
    }

    pub async fn enter_worktree(
        &self,
        original_cwd: PathBuf,
        original_branch: String,
        worktree_path: PathBuf,
        worktree_branch: String,
    ) -> Result<()> {
        let admission = self.execution_admission_lease()?;
        let material = self.prepare_original_workspace_request_material(&(
            &original_cwd,
            &original_branch,
            &worktree_path,
            &worktree_branch,
        ))?;
        let session = WorktreeSession {
            original_cwd,
            original_branch,
            worktree_path,
            worktree_branch,
        };
        self.start_owned_legacy_worktree_entry(session, material, admission)
            .await
    }
    pub(crate) async fn enter_worktree_from_seed(&self, seed: &ManagedWorktreeSeed) -> Result<()> {
        let admission = self.execution_admission_lease()?;
        let material = self.prepare_original_workspace_request_material(&(
            &seed.original_cwd,
            &seed.original_branch,
            &seed.worktree_path,
            &seed.worktree_branch,
        ))?;
        let session = WorktreeSession {
            original_cwd: seed.original_cwd.clone(),
            original_branch: seed.original_branch.clone(),
            worktree_path: seed.worktree_path.clone(),
            worktree_branch: seed.worktree_branch.clone(),
        };
        self.start_owned_legacy_worktree_entry(session, material, admission)
            .await
    }
    async fn start_owned_legacy_worktree_entry(
        &self,
        session: WorktreeSession,
        material: Arc<super::OriginalWorkspaceRequestMaterial>,
        _admission: super::execution_admission::ExecutionLease,
    ) -> Result<()> {
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = _admission;
            let result = runtime.enter_worktree_owned(session, material).await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("original worktree entry worker ended before its ACK")?
    }
    pub(crate) async fn enter_worktree_for_original_child_spawn(
        &self,
        source: &super::OriginalChildTaskProducer,
        seed: &ManagedWorktreeSeed,
    ) -> Result<()> {
        source.validate_original_spawn_worktree_target(
            self.instance_agent_id(),
            self.instance_incarnation(),
            seed,
        )?;
        let material = self.prepare_original_workspace_request_material(&(
            &seed.original_cwd,
            &seed.original_branch,
            &seed.worktree_path,
            &seed.worktree_branch,
        ))?;
        let session = WorktreeSession {
            original_cwd: seed.original_cwd.clone(),
            original_branch: seed.original_branch.clone(),
            worktree_path: seed.worktree_path.clone(),
            worktree_branch: seed.worktree_branch.clone(),
        };
        self.enter_worktree_owned(session, material).await
    }
    async fn enter_worktree_owned(
        &self,
        worktree_session: WorktreeSession,
        material: Arc<super::OriginalWorkspaceRequestMaterial>,
    ) -> Result<()> {
        self.invalidate_context_resources(
            super::resource_observation::ResourceInvalidation::WorkspaceChange,
        )?;
        let agent_id = self.agent_id().await?;
        let worktree_path = &worktree_session.worktree_path;
        let (workspace_id, workspace_anchor) = {
            let guard = self.inner.agent.lock().await;
            let entry = guard
                .state
                .active_workspace_entry
                .as_ref()
                .ok_or_else(|| anyhow!("agent has no active workspace attachment"))?;
            (entry.workspace_id.clone(), entry.workspace_anchor.clone())
        };
        let execution_root_id = Self::build_execution_root_id(
            &workspace_id,
            WorkspaceProjectionKind::GitWorktreeRoot,
            &worktree_path,
        )?;
        let occupancy = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            bridge
                .acquire_workspace_occupancy(
                    &workspace_id,
                    &execution_root_id,
                    &agent_id,
                    WorkspaceAccessMode::ExclusiveWrite,
                )
                .await?
        } else {
            None
        };

        {
            let mut guard = self.begin_mutation().await?;
            guard.state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                workspace_id: workspace_id.clone(),
                workspace_anchor: workspace_anchor.clone(),
                execution_root_id: execution_root_id.clone(),
                execution_root: worktree_path.clone(),
                projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                access_mode: WorkspaceAccessMode::ExclusiveWrite,
                cwd: worktree_path.clone(),
                occupancy_id: occupancy.as_ref().map(|record| record.occupancy_id.clone()),
                projection_metadata: Some(WorkspaceProjectionMetadata::ManagedWorktree(
                    crate::types::ManagedWorktreeProjectionMetadata {
                        original_cwd: worktree_session.original_cwd.clone(),
                        original_branch: worktree_session.original_branch.clone(),
                        worktree_path: worktree_session.worktree_path.clone(),
                        worktree_branch: worktree_session.worktree_branch.clone(),
                    },
                )),
            });
            guard.state.worktree_session = Some(worktree_session.clone());
            guard
                .persist_state_with_original_workspace_request(material.clone())
                .await?;
        }
        let active = self
            .agent_state()
            .await?
            .active_workspace_entry
            .ok_or_else(|| anyhow!("managed worktree entry disappeared after persistence"))?;
        let existing = self
            .inner
            .runtime_db
            .execution_root_entries()
            .get(&active.execution_root_id)?;
        let entry = crate::runtime::workspace::execution_root_entry_from_active(
            &active,
            existing
                .as_ref()
                .map(|entry| entry.created_at)
                .unwrap_or_else(chrono::Utc::now),
            existing.and_then(|entry| entry.worktree),
        );
        if let Err(error) = self
            .inner
            .runtime_db
            .execution_root_entries()
            .upsert(&entry)
        {
            tracing::warn!(
                agent_id = %agent_id,
                execution_root_id = %entry.execution_root_id,
                workspace_id = %entry.workspace_id,
                error = %error,
                "failed to register managed worktree execution root"
            );
        }

        let profile = self.agent_state().await?.execution_profile;
        let audit = self.prepare_original_legacy_worktree_entered_observation(
            material,
            &worktree_session,
            &profile,
            &execution_root_id,
        )?;
        self.finish_owned_original_workspace_observation(audit)
            .await?;
        Ok(())
    }

    pub(crate) async fn discard_managed_worktree(&self, worktree: &WorktreeSession) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let remove_output = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("worktree"),
                            OsString::from("remove"),
                            OsString::from("--force"),
                            worktree.worktree_path.as_os_str().to_os_string(),
                        ],
                    },
                    cwd: Some(worktree.original_cwd.clone()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::WorktreeSetup,
                },
            )
            .await
            .context("failed to remove git worktree")?;
        if !remove_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&remove_output.stderr)
                .trim()
                .to_string();
            return Err(anyhow!("git worktree remove failed: {stderr}"));
        }

        let branch_output = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("branch"),
                            OsString::from("-D"),
                            OsString::from(&worktree.worktree_branch),
                        ],
                    },
                    cwd: Some(worktree.original_cwd.clone()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await
            .context("failed to remove worktree branch")?;
        if !branch_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&branch_output.stderr)
                .trim()
                .to_string();
            return Err(anyhow!("git branch -D failed: {stderr}"));
        }
        Ok(())
    }

    pub async fn exit_worktree(&self, original_cwd: PathBuf, removed: bool) -> Result<()> {
        let admission = self.execution_admission_lease()?;
        let material =
            self.prepare_original_workspace_request_material(&(&original_cwd, removed))?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = runtime
                .exit_worktree_owned(original_cwd, removed, material)
                .await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("original worktree exit worker ended before its ACK")?
    }
    async fn exit_worktree_owned(
        &self,
        original_cwd: PathBuf,
        removed: bool,
        material: Arc<super::OriginalWorkspaceRequestMaterial>,
    ) -> Result<()> {
        let (worktree_session, occupancy_id, prev_execution_root_id) = {
            let mut guard = self.begin_mutation().await?;
            let session = guard.state.worktree_session.take();
            if session.is_none() {
                return Err(anyhow!("no active managed worktree to exit"));
            }
            let prev_execution_root_id = guard
                .state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.execution_root_id.clone());
            let occupancy_id = guard
                .state
                .active_workspace_entry
                .as_ref()
                .and_then(|entry| entry.occupancy_id.clone());
            if let Some(entry) = guard.state.active_workspace_entry.clone() {
                // Restore to original workspace before worktree
                let execution_root_id = Self::build_execution_root_id(
                    &entry.workspace_id,
                    WorkspaceProjectionKind::CanonicalRoot,
                    &original_cwd,
                )?;
                guard.state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                    workspace_id: entry.workspace_id,
                    workspace_anchor: original_cwd.clone(),
                    execution_root_id,
                    execution_root: original_cwd.clone(),
                    projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                    access_mode: WorkspaceAccessMode::SharedRead,
                    cwd: original_cwd.clone(),
                    occupancy_id: None,
                    projection_metadata: None,
                });
            }
            guard
                .persist_state_with_original_workspace_request(material.clone())
                .await?;
            (session, occupancy_id, prev_execution_root_id)
        };
        // Soft-delete the worktree execution root entry from the registry
        // when the worktree was physically removed.
        if removed {
            if let Some(root_id) = prev_execution_root_id.as_deref() {
                if !root_id.starts_with("canonical_root:") {
                    if let Err(error) = self
                        .inner
                        .runtime_db
                        .execution_root_entries()
                        .mark_removed(root_id)
                    {
                        tracing::warn!(
                            execution_root_id = root_id,
                            error = %error,
                            "failed to mark execution root as removed"
                        );
                    }
                }
            }
        }
        if let Some(occupancy_id) = occupancy_id.as_deref() {
            if let Some(bridge) = self.inner.host_bridge.as_ref() {
                let _ = bridge.release_workspace_occupancy(occupancy_id).await?;
            }
        }

        let audit = self.prepare_original_legacy_worktree_exited_observation(
            material,
            worktree_session.as_ref(),
            removed,
        )?;
        self.finish_owned_original_workspace_observation(audit)
            .await?;
        Ok(())
    }

    pub(super) async fn record_task_owned_worktree_metadata(
        &self,
        task_id: &str,
        seed: &ManagedWorktreeSeed,
        material: Arc<super::OriginalWorkspaceRequestMaterial>,
    ) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        let limits = self.inner.input_evidence_budget.limits();
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("worktree Task limits overflow")?,
            )?,
        };
        let _selector = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            task_id
                .len()
                .checked_add(256)
                .and_then(|n| n.checked_mul(8))
                .context("worktree Task selector overflow")?,
            8,
            &request,
            &self.inner.input_evidence_budget,
        )?;
        let mut capture = super::resident_task::TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: Default::default(),
        };
        capture
            .required_keys
            .insert(super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                task_id,
            ));
        mutation.prepare_owner_task_request(&capture).await?;
        let selected = mutation.capture_tasks(&capture)?;
        let fact = selected
            .lookup(&super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                task_id,
            ))?
            .context("worktree Task producer missing")?;
        let super::resident_task::CanonicalTaskFact::Task(task) = &fact.fact else {
            anyhow::bail!("worktree Task producer selected another family")
        };
        let (encoded, native) = super::input_evidence_budget::borrowed_json_material(&(
            task,
            &seed.original_cwd,
            &seed.original_branch,
            &seed.worktree_path,
            &seed.worktree_branch,
        ))?;
        let copy_bytes = usize::try_from(encoded)?
            .checked_add(
                native
                    .checked_mul(256)
                    .context("worktree Task native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(1024))
            .and_then(|n| n.checked_mul(8))
            .context("worktree Task copy bytes overflow")?;
        let copy_records = native
            .checked_add(32)
            .and_then(|n| n.checked_mul(8))
            .context("worktree Task copy records overflow")?;
        let selected_usage = physical_worktree_selected_task_material(&selected)?;
        let request_usage = material.material_estimate()?;
        let selector_bytes = usize::try_from(_selector.owned_material_bytes()?)?;
        let prior_bytes = selected_usage
            .0
            .checked_add(request_usage.0)
            .and_then(|n| n.checked_add(selector_bytes))
            .context("worktree Task selected prior bytes overflow")?;
        let prior_records = selected_usage
            .1
            .checked_add(request_usage.1)
            .and_then(|n| n.checked_add(8))
            .context("worktree Task selected prior records overflow")?;
        let mut copy_request = request;
        copy_request.byte_limit = copy_request
            .byte_limit
            .min(64 * 1024 * 1024)
            .checked_sub(prior_bytes)
            .ok_or(super::current_read_facts::CurrentReadError::ResourceExhausted)?;
        copy_request.record_limit = copy_request
            .record_limit
            .min(65536)
            .checked_sub(prior_records)
            .ok_or(super::current_read_facts::CurrentReadError::ResourceExhausted)?;
        let owned = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            copy_bytes,
            copy_records,
            &copy_request,
            &self.inner.input_evidence_budget,
        )?;
        let task_material = material.with_original_task_copy(owned, copy_records)?;
        let task = task.clone();
        let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
        if let Some(detail) = detail.as_object_mut() {
            detail.insert("workspace_mode".into(), serde_json::json!("worktree"));
            detail.insert("task_status".into(), serde_json::json!("running"));
            detail.insert(
                "worktree".into(),
                serde_json::json!({
                    "worktree_path": seed.worktree_path,
                    "worktree_branch": seed.worktree_branch,
                    "projection_kind": "git_worktree_root",
                    "original_cwd": seed.original_cwd,
                    "original_branch": seed.original_branch,
                }),
            );
        }
        let status = if matches!(task.status, TaskStatus::Queued) {
            TaskStatus::Running
        } else {
            task.status
        };
        let updated = TaskRecord {
            detail: Some(detail),
            status,
            updated_at: chrono::Utc::now(),
            ..task
        };
        // These original read/selector allocations end before the business
        // write phase; only the guarded owned updated Task and request survive.
        drop((mutation, selected, capture, _selector));
        self.apply_task_transition(
            task_state_reducer::TaskTransition::new(&updated, "task_status_updated")
                .with_original_worktree_request_material(task_material),
        )
        .await?;
        drop(updated);
        let audit =
            self.prepare_original_task_worktree_metadata_observation(material, task_id, seed)?;
        self.finish_owned_original_workspace_observation(audit)
            .await?;
        Ok(())
    }

    pub(super) async fn cleanup_original_control_worktree_in_detail(
        &self,
        task_id: &str,
        detail: &mut serde_json::Value,
        reason: &str,
        completion: Arc<super::input_evidence_budget::OwnedCompletionBudget>,
        request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ) -> Result<Option<cleanup::ObservedOriginalControlCleanup>> {
        let Some(worktree) = detail.get_mut("worktree") else {
            return Ok(None);
        };
        cleanup::cleanup_original_control_metadata(
            self, task_id, worktree, reason, completion, request,
        )
        .await
        .map(Some)
    }
    pub async fn summarize_worktree_tasks(&self) -> Result<String> {
        let tasks = self.latest_task_records().await?;
        let worktree_tasks: Vec<_> = tasks
            .iter()
            .filter(|task| task.is_worktree_child_agent_task())
            .collect();

        if worktree_tasks.is_empty() {
            return Ok("No worktree tasks found.".to_string());
        }

        let messages = self.inner.storage.read_recent_messages(200)?;
        let mut summary_lines = Vec::new();

        summary_lines.push("=".repeat(60));
        summary_lines.push("Worktree Task Summary".to_string());
        summary_lines.push(format!("Total tasks: {}", worktree_tasks.len()));
        summary_lines.push("=".repeat(60));
        summary_lines.push(String::new());

        let mut completed = Vec::new();
        let mut failed = Vec::new();
        let mut running = Vec::new();
        let mut queued = Vec::new();

        for task in &worktree_tasks {
            match task.status {
                TaskStatus::Completed => completed.push(task),
                TaskStatus::Failed | TaskStatus::Interrupted => failed.push(task),
                TaskStatus::Running | TaskStatus::Cancelling => running.push(task),
                TaskStatus::Queued => queued.push(task),
                TaskStatus::Cancelled => {}
            }
        }

        let format_task = |task: &&TaskRecord| -> String {
            let task_id = &task.id;
            let task_summary = task.summary.as_deref().unwrap_or("(no summary)");

            let worktree_info = task
                .detail
                .as_ref()
                .and_then(|detail| detail.get("worktree").cloned())
                .or_else(|| {
                    messages
                        .iter()
                        .find(|msg| {
                            matches!(msg.kind, MessageKind::TaskResult)
                                && msg
                                    .metadata
                                    .as_ref()
                                    .and_then(|m| m.get("task_id"))
                                    .and_then(|id| id.as_str())
                                    == Some(task_id)
                        })
                        .and_then(|msg| msg.metadata.as_ref())
                        .and_then(|m| m.get("worktree").cloned())
                });

            let mut lines = Vec::new();
            lines.push(format!("Task ID: {}", task_id));
            lines.push(format!("Summary: {}", task_summary));

            if let Some(worktree) = worktree_info {
                if let Some(path) = worktree.get("worktree_path").and_then(|v| v.as_str()) {
                    lines.push(format!("Worktree path: {}", path));
                }
                if let Some(branch) = worktree.get("worktree_branch").and_then(|v| v.as_str()) {
                    lines.push(format!("Branch: {}", branch));
                }
                if let Some(changed) = worktree.get("changed_files").and_then(|v| v.as_array()) {
                    let changed_files: Vec<_> = changed.iter().filter_map(|v| v.as_str()).collect();
                    if changed_files.is_empty() {
                        lines.push("Changed files: none".to_string());
                    } else {
                        lines.push(format!("Changed files: {}", changed_files.join(", ")));
                    }
                }
                if let Some(retained) = worktree
                    .get("retained_for_review")
                    .and_then(|v| v.as_bool())
                {
                    if retained {
                        lines.push("Status: Worktree retained for review".to_string());
                    }
                }
                if let Some(cleaned) = worktree.get("auto_cleaned_up").and_then(|v| v.as_bool()) {
                    if cleaned {
                        lines.push("Status: Worktree auto-cleaned".to_string());
                    }
                }
                if let Some(status) = worktree.get("cleanup_status").and_then(|v| v.as_str()) {
                    lines.push(format!("Cleanup status: {}", status));
                }
                if let Some(reason) = worktree.get("cleanup_reason").and_then(|v| v.as_str()) {
                    lines.push(format!("Cleanup reason: {}", reason));
                }
            }

            lines.join("\n  ")
        };

        if !completed.is_empty() {
            summary_lines.push(format!("Completed Tasks ({})", completed.len()));
            summary_lines.push("-".repeat(40));
            for task in completed {
                summary_lines.push(format!("  {}", format_task(task)));
                summary_lines.push(String::new());
            }
        }

        if !failed.is_empty() {
            summary_lines.push(format!("Failed Tasks ({})", failed.len()));
            summary_lines.push("-".repeat(40));
            for task in failed {
                summary_lines.push(format!("  {}", format_task(task)));
                summary_lines.push(String::new());
            }
        }

        if !running.is_empty() {
            summary_lines.push(format!("Running Tasks ({})", running.len()));
            summary_lines.push("-".repeat(40));
            for task in running {
                summary_lines.push(format!("  {}", format_task(task)));
                summary_lines.push(String::new());
            }
        }

        if !queued.is_empty() {
            summary_lines.push(format!("Queued Tasks ({})", queued.len()));
            summary_lines.push("-".repeat(40));
            for task in queued {
                summary_lines.push(format!("  {}", format_task(task)));
                summary_lines.push(String::new());
            }
        }

        summary_lines.push("=".repeat(60));
        summary_lines.push("Review Guidance:".to_string());
        summary_lines.push("- Inspect worktrees with changes to evaluate approaches".to_string());
        summary_lines
            .push("- Use 'git worktree remove <path>' to discard unwanted attempts".to_string());
        summary_lines.push("- Use 'git diff <worktree-path>' to see detailed changes".to_string());
        summary_lines.push("=".repeat(60));

        Ok(summary_lines.join("\n"))
    }

    #[cfg(test)]
    async fn remove_task_owned_worktree(
        &self,
        artifact: &TaskOwnedWorktreeArtifact,
    ) -> Result<TaskOwnedWorktreeRemoveOutcome> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let remove_output = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("worktree"),
                            OsString::from("remove"),
                            artifact.worktree_path.as_os_str().to_os_string(),
                        ],
                    },
                    cwd: Some(self.workspace_root()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::WorktreeSetup,
                },
            )
            .await
            .context("failed to remove git worktree")?;

        if !remove_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&remove_output.stderr)
                .trim()
                .to_string();
            return Err(anyhow!("git worktree remove failed: {stderr}"));
        }

        if !self
            .local_branch_exists(&artifact.worktree_branch, &execution)
            .await?
        {
            return Ok(TaskOwnedWorktreeRemoveOutcome {
                branch_deleted: false,
                branch_delete_error: None,
            });
        }

        let branch_output = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("branch"),
                            OsString::from("-D"),
                            OsString::from(&artifact.worktree_branch),
                        ],
                    },
                    cwd: Some(self.workspace_root()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await
            .context("failed to remove worktree branch")?;
        if !branch_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&branch_output.stderr)
                .trim()
                .to_string();
            return Ok(TaskOwnedWorktreeRemoveOutcome {
                branch_deleted: false,
                branch_delete_error: Some(format!("git branch -D failed: {stderr}")),
            });
        }

        Ok(TaskOwnedWorktreeRemoveOutcome {
            branch_deleted: true,
            branch_delete_error: None,
        })
    }

    async fn local_branch_exists(
        &self,
        branch: &str,
        execution: &crate::system::EffectiveExecution,
    ) -> Result<bool> {
        let branch_ref = format!("refs/heads/{branch}");
        let output = self
            .system()
            .run(
                execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("show-ref"),
                            OsString::from("--verify"),
                            OsString::from("--quiet"),
                            OsString::from(branch_ref),
                        ],
                    },
                    cwd: Some(self.workspace_root()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await
            .context("failed to inspect worktree branch ref")?;
        Ok(output.exit_status.success())
    }

    #[cfg(test)]
    async fn current_worktree_branch(&self, worktree_path: &Path) -> Result<String> {
        git_stdout(
            self,
            worktree_path,
            &["rev-parse", "--abbrev-ref", "HEAD"],
            "failed to inspect worktree branch",
        )
        .await
    }

    #[cfg(test)]
    async fn changed_files_for_worktree(&self, worktree_path: &Path) -> Result<Vec<String>> {
        let status = git_stdout(
            self,
            worktree_path,
            &["status", "--porcelain"],
            "failed to inspect worktree status",
        )
        .await?;
        let mut changed_files = status
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let trimmed = line.trim();
                trimmed
                    .get(3..)
                    .filter(|path| !path.trim().is_empty())
                    .unwrap_or(trimmed)
                    .to_string()
            })
            .collect::<Vec<_>>();
        changed_files.sort();
        Ok(changed_files)
    }
}
pub(super) fn format_worktree_task_result(result: &WorktreeSubagentResult) -> String {
    let mut lines = Vec::new();
    let cleaned = result.text.trim();
    if !cleaned.is_empty() {
        lines.push(cleaned.to_string());
    }
    lines.push(format!("Worktree path: {}", result.worktree_path.display()));
    lines.push(format!("Worktree branch: {}", result.worktree_branch));
    if result.changed_files.is_empty() {
        lines.push("Changed files: none".to_string());
    } else {
        lines.push(format!(
            "Changed files: {}",
            result.changed_files.join(", ")
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_worktree_task_summary(
    tasks: &[TaskRecord],
    messages: &[MessageEnvelope],
) -> String {
    let worktree_tasks: Vec<_> = tasks
        .iter()
        .filter(|task| task.is_worktree_child_agent_task())
        .collect();

    if worktree_tasks.is_empty() {
        return "No worktree tasks found.".to_string();
    }

    let mut summary_lines = Vec::new();
    summary_lines.push("=".repeat(60));
    summary_lines.push("Worktree Task Summary".to_string());
    summary_lines.push(format!("Total tasks: {}", worktree_tasks.len()));
    summary_lines.push("=".repeat(60));
    summary_lines.push(String::new());

    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let mut running = Vec::new();
    let mut queued = Vec::new();
    for task in &worktree_tasks {
        match task.status {
            TaskStatus::Completed => completed.push(task),
            TaskStatus::Failed | TaskStatus::Interrupted => failed.push(task),
            TaskStatus::Running | TaskStatus::Cancelling => running.push(task),
            TaskStatus::Queued => queued.push(task),
            TaskStatus::Cancelled => {}
        }
    }

    let format_task = |task: &&TaskRecord| -> String {
        let task_id = &task.id;
        let task_summary = task.summary.as_deref().unwrap_or("(no summary)");
        let worktree_info = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("worktree").cloned())
            .or_else(|| {
                messages
                    .iter()
                    .find(|msg| {
                        matches!(msg.kind, MessageKind::TaskResult)
                            && msg
                                .metadata
                                .as_ref()
                                .and_then(|m| m.get("task_id"))
                                .and_then(|id| id.as_str())
                                == Some(task_id)
                    })
                    .and_then(|msg| msg.metadata.as_ref())
                    .and_then(|m| m.get("worktree").cloned())
            });
        let mut lines = Vec::new();
        lines.push(format!("Task ID: {}", task_id));
        lines.push(format!("Summary: {}", task_summary));
        if let Some(worktree) = worktree_info {
            if let Some(path) = worktree.get("worktree_path").and_then(|v| v.as_str()) {
                lines.push(format!("Worktree path: {}", path));
            }
            if let Some(branch) = worktree.get("worktree_branch").and_then(|v| v.as_str()) {
                lines.push(format!("Branch: {}", branch));
            }
            if let Some(changed) = worktree.get("changed_files").and_then(|v| v.as_array()) {
                let changed_files: Vec<_> = changed.iter().filter_map(|v| v.as_str()).collect();
                if changed_files.is_empty() {
                    lines.push("Changed files: none".to_string());
                } else {
                    lines.push(format!("Changed files: {}", changed_files.join(", ")));
                }
            }
            if let Some(retained) = worktree
                .get("retained_for_review")
                .and_then(|v| v.as_bool())
            {
                if retained {
                    lines.push("Status: Worktree retained for review".to_string());
                }
            }
            if let Some(cleaned) = worktree.get("auto_cleaned_up").and_then(|v| v.as_bool()) {
                if cleaned {
                    lines.push("Status: Worktree auto-cleaned".to_string());
                }
            }
            if let Some(status) = worktree.get("cleanup_status").and_then(|v| v.as_str()) {
                lines.push(format!("Cleanup status: {}", status));
            }
            if let Some(reason) = worktree.get("cleanup_reason").and_then(|v| v.as_str()) {
                lines.push(format!("Cleanup reason: {}", reason));
            }
        }
        lines.join("\n  ")
    };

    if !completed.is_empty() {
        summary_lines.push(format!("Completed Tasks ({})", completed.len()));
        summary_lines.push("-".repeat(40));
        for task in completed {
            summary_lines.push(format!("  {}", format_task(task)));
            summary_lines.push(String::new());
        }
    }
    if !failed.is_empty() {
        summary_lines.push(format!("Failed Tasks ({})", failed.len()));
        summary_lines.push("-".repeat(40));
        for task in failed {
            summary_lines.push(format!("  {}", format_task(task)));
            summary_lines.push(String::new());
        }
    }
    if !running.is_empty() {
        summary_lines.push(format!("Running Tasks ({})", running.len()));
        summary_lines.push("-".repeat(40));
        for task in running {
            summary_lines.push(format!("  {}", format_task(task)));
            summary_lines.push(String::new());
        }
    }
    if !queued.is_empty() {
        summary_lines.push(format!("Queued Tasks ({})", queued.len()));
        summary_lines.push("-".repeat(40));
        for task in queued {
            summary_lines.push(format!("  {}", format_task(task)));
            summary_lines.push(String::new());
        }
    }
    summary_lines.push("=".repeat(60));
    summary_lines.push("Review Guidance:".to_string());
    summary_lines.push("- Inspect worktrees with changes to evaluate approaches".to_string());
    summary_lines
        .push("- Use 'git worktree remove <path>' to discard unwanted attempts".to_string());
    summary_lines.push("- Use 'git diff <worktree-path>' to see detailed changes".to_string());
    summary_lines.push("=".repeat(60));
    summary_lines.join("\n")
}

fn parse_task_owned_worktree_artifact(
    worktree: &serde_json::Value,
) -> Result<TaskOwnedWorktreeArtifact> {
    let worktree_path = worktree
        .get("worktree_path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task-owned worktree metadata missing worktree_path"))?;
    let worktree_branch = worktree
        .get("worktree_branch")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task-owned worktree metadata missing worktree_branch"))?;
    Ok(TaskOwnedWorktreeArtifact {
        worktree_path: PathBuf::from(worktree_path),
        worktree_branch: worktree_branch.to_string(),
    })
}

fn changed_files_from_metadata(worktree: &serde_json::Value) -> Vec<String> {
    worktree
        .get("changed_files")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

async fn git_stdout(
    runtime: &RuntimeHandle,
    cwd: &Path,
    args: &[&str],
    context: &str,
) -> Result<String> {
    let system = runtime.system();
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let output = system
        .run(
            &execution,
            ProcessRequest {
                program: ProgramInvocation::Argv {
                    program: "git".into(),
                    args: args.iter().map(OsString::from).collect(),
                },
                cwd: Some(cwd.to_path_buf()),
                env: vec![],
                stdin: StdioSpec::Null,
                tty: false,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::InternalGit,
            },
        )
        .await
        .with_context(|| context.to_string())?;
    if !output.exit_status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!("{context}: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sanitize_branch_name(branch_name: &str) -> PathBuf {
    PathBuf::from(branch_name.replace(['/', '\\', ' '], "-"))
}

fn physical_worktree_selected_task_material(
    selected: &super::resident_task::CapturedTasks,
) -> Result<(usize, usize)> {
    let base = selected.selected_material_estimate()?;
    let mut total = (usize::try_from(base.0)?, base.1);
    fn add(total: &mut (usize, usize), value: &impl serde::Serialize, floor: usize) -> Result<()> {
        let (encoded, nodes) = super::input_evidence_budget::borrowed_json_material(value)?;
        let actual = usize::try_from(encoded)?
            .max(floor)
            .checked_add(
                nodes
                    .checked_mul(256)
                    .context("physical selected Task native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(std::mem::size_of_val(value)))
            .context("physical selected Task bytes overflow")?;
        total.0 = total
            .0
            .checked_add(
                actual
                    .checked_sub(floor)
                    .context("physical selected floor underflow")?,
            )
            .context("physical selected total bytes overflow")?;
        total.1 = total
            .1
            .checked_add(nodes)
            .context("physical selected Task records overflow")?;
        Ok(())
    }
    for record in &selected.records {
        add(&mut total, record.as_ref(), record.resident_bytes)?;
    }
    for record in &selected.stop_records {
        add(
            &mut total,
            record.as_ref(),
            usize::try_from(record.resident_bytes()?)?,
        )?;
    }
    Ok(total)
}
