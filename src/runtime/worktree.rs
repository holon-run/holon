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
        let agent_id = self.agent_id().await?;
        let worktree_session = WorktreeSession {
            original_cwd,
            original_branch,
            worktree_path: worktree_path.clone(),
            worktree_branch,
        };
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
            guard.persist_state(&self.inner.storage).await?;
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

        let boundary = crate::system::HostLocalBoundary::from_parts(
            &self.agent_state().await?.execution_profile,
            Some(WorkspaceProjectionKind::GitWorktreeRoot),
            Some(WorkspaceAccessMode::ExclusiveWrite),
            Some(execution_root_id),
        );
        self.inner.storage.append_event(&AuditEvent::legacy(
            "worktree_entered",
            serde_json::json!({
                "worktree": to_json_value(&worktree_session),
                "boundary": boundary.audit_metadata(),
            }),
        ))?;
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
            guard.persist_state(&self.inner.storage).await?;
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

        self.inner.storage.append_event(&AuditEvent::legacy(
            "worktree_exited",
            serde_json::json!({
                "worktree_path": worktree_session.as_ref().map(|w| &w.worktree_path),
                "worktree_branch": worktree_session.as_ref().map(|w| &w.worktree_branch),
                "removed": removed,
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn record_task_owned_worktree_metadata(
        &self,
        task_id: &str,
        seed: &ManagedWorktreeSeed,
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
        let mut capture = super::resident_task::TaskCaptureRequest::current();
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
        let _owned = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            usize::try_from(super::input_evidence_budget::serialized_bytes(&(
                task,
                &seed.original_cwd,
                &seed.original_branch,
                &seed.worktree_path,
                &seed.worktree_branch,
            ))?)?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("worktree Task material overflow")?,
            super::child_task_terminal::completion_value_nodes(task.detail.as_ref())?
                .checked_add(16)
                .and_then(|n| n.checked_mul(8))
                .context("worktree Task nodes overflow")?,
            &request,
            &self.inner.input_evidence_budget,
        )?;
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
        drop(mutation);
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &updated,
            "task_status_updated",
        ))
        .await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_worktree_metadata_recorded",
            serde_json::json!({
                "task_id": task_id,
                "worktree_path": seed.worktree_path,
                "worktree_branch": seed.worktree_branch,
            }),
        ))?;
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
