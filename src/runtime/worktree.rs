use super::*;
use std::ffi::OsString;

use crate::system::{
    file::FileHost, CaptureSpec, ExecutionScopeKind, ProcessHost, ProcessPurpose, ProcessRequest,
    ProgramInvocation, StdioSpec,
};
use crate::types::WorktreeSession;

#[derive(Debug, Clone)]
pub(super) struct TaskOwnedWorktreeCleanup {
    pub(super) changed_files: Vec<String>,
    pub(super) status: TaskOwnedWorktreeCleanupStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskOwnedWorktreeCleanupStatus {
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
            let mut guard = self.inner.agent.lock().await;
            guard.state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                workspace_id: workspace_id.clone(),
                workspace_anchor: workspace_anchor.clone(),
                execution_root_id: execution_root_id.clone(),
                execution_root: worktree_path.clone(),
                projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                access_mode: WorkspaceAccessMode::ExclusiveWrite,
                cwd: worktree_path.clone(),
                occupancy_id: occupancy.as_ref().map(|record| record.occupancy_id.clone()),
                projection_metadata: Some(serde_json::json!({
                    "original_cwd": worktree_session.original_cwd,
                    "original_branch": worktree_session.original_branch,
                    "worktree_path": worktree_session.worktree_path,
                    "worktree_branch": worktree_session.worktree_branch,
                })),
            });
            guard.state.worktree_session = Some(worktree_session.clone());
            self.inner.storage.write_agent(&guard.state)?;
        }

        let boundary = crate::system::HostLocalBoundary::from_parts(
            &self.agent_state().await?.execution_profile,
            Some(WorkspaceProjectionKind::GitWorktreeRoot),
            Some(WorkspaceAccessMode::ExclusiveWrite),
            Some(execution_root_id),
        );
        self.inner.storage.append_event(&AuditEvent::new(
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
        let (worktree_session, occupancy_id) = {
            let mut guard = self.inner.agent.lock().await;
            let session = guard.state.worktree_session.take();
            if session.is_none() {
                return Err(anyhow!("no active managed worktree to exit"));
            }
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
            self.inner.storage.write_agent(&guard.state)?;
            (session, occupancy_id)
        };
        if let Some(occupancy_id) = occupancy_id.as_deref() {
            if let Some(bridge) = self.inner.host_bridge.as_ref() {
                let _ = bridge.release_workspace_occupancy(occupancy_id).await?;
            }
        }

        self.inner.storage.append_event(&AuditEvent::new(
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
        let Some(task) = self.task_record(task_id).await? else {
            return Ok(());
        };
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
        self.inner.storage.append_task(&updated)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "task_worktree_metadata_recorded",
            serde_json::json!({
                "task_id": task_id,
                "worktree_path": seed.worktree_path,
                "worktree_branch": seed.worktree_branch,
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn cleanup_task_owned_worktree_in_detail(
        &self,
        task_id: &str,
        detail: &mut serde_json::Value,
        reason: &str,
    ) -> Result<Option<TaskOwnedWorktreeCleanup>> {
        let Some(worktree) = detail.get_mut("worktree") else {
            return Ok(None);
        };
        let cleanup = self
            .cleanup_task_owned_worktree_metadata(task_id, worktree, reason)
            .await?;
        Ok(Some(cleanup))
    }

    async fn cleanup_task_owned_worktree_metadata(
        &self,
        task_id: &str,
        worktree: &mut serde_json::Value,
        reason: &str,
    ) -> Result<TaskOwnedWorktreeCleanup> {
        let artifact = match parse_task_owned_worktree_artifact(worktree) {
            Ok(artifact) => artifact,
            Err(err) => {
                let error = err.to_string();
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Failed.label());
                worktree["cleanup_error"] = serde_json::json!(error);
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_worktree_cleanup_failed",
                    serde_json::json!({
                        "task_id": task_id,
                        "reason": reason,
                        "error": worktree["cleanup_error"],
                    }),
                ))?;
                return Ok(TaskOwnedWorktreeCleanup {
                    changed_files: Vec::new(),
                    status: TaskOwnedWorktreeCleanupStatus::Failed,
                });
            }
        };

        if !artifact.worktree_path.exists() {
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::AlreadyRemoved.label());
            worktree["cleanup_reason"] = serde_json::json!("worktree_path_missing");
            worktree["auto_cleaned_up"] = serde_json::json!(true);
            self.inner.storage.append_event(&AuditEvent::new(
                "task_worktree_cleanup_already_removed",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": reason,
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files: Vec::new(),
                status: TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
            });
        }

        let actual_branch = self.current_worktree_branch(&artifact.worktree_path).await;
        match actual_branch {
            Ok(actual_branch) if actual_branch != artifact.worktree_branch => {
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
                worktree["cleanup_reason"] = serde_json::json!("branch_mismatch");
                worktree["actual_branch"] = serde_json::json!(actual_branch);
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_worktree_cleanup_retained",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "actual_branch": actual_branch,
                        "reason": "branch_mismatch",
                        "cleanup_trigger": reason,
                    }),
                ))?;
                return Ok(TaskOwnedWorktreeCleanup {
                    changed_files: changed_files_from_metadata(worktree),
                    status: TaskOwnedWorktreeCleanupStatus::Retained,
                });
            }
            Err(err) => {
                let error = err.to_string();
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
                worktree["cleanup_reason"] = serde_json::json!("branch_inspection_failed");
                worktree["cleanup_error"] = serde_json::json!(error);
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_worktree_cleanup_retained",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": "branch_inspection_failed",
                        "cleanup_trigger": reason,
                        "error": worktree["cleanup_error"],
                    }),
                ))?;
                return Ok(TaskOwnedWorktreeCleanup {
                    changed_files: changed_files_from_metadata(worktree),
                    status: TaskOwnedWorktreeCleanupStatus::Retained,
                });
            }
            _ => {}
        }

        let changed_files = match self
            .changed_files_for_worktree(&artifact.worktree_path)
            .await
        {
            Ok(changed_files) => changed_files,
            Err(err) => {
                let error = err.to_string();
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
                worktree["cleanup_reason"] = serde_json::json!("change_detection_failed");
                worktree["cleanup_error"] = serde_json::json!(error);
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_worktree_cleanup_retained",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": "change_detection_failed",
                        "cleanup_trigger": reason,
                        "error": worktree["cleanup_error"],
                    }),
                ))?;
                return Ok(TaskOwnedWorktreeCleanup {
                    changed_files: changed_files_from_metadata(worktree),
                    status: TaskOwnedWorktreeCleanupStatus::Retained,
                });
            }
        };
        worktree["changed_files"] = serde_json::json!(changed_files);

        if !changed_files.is_empty() {
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
            worktree["cleanup_reason"] = serde_json::json!("changed_files");
            worktree["retained_for_review"] = serde_json::json!(true);
            self.inner.storage.append_event(&AuditEvent::new(
                "worktree_retained_for_review",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "changed_files": changed_files,
                    "reason": "changes detected in worktree",
                    "cleanup_trigger": reason,
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files,
                status: TaskOwnedWorktreeCleanupStatus::Retained,
            });
        }

        match self.remove_task_owned_worktree(&artifact).await {
            Ok(remove_outcome) => {
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Cleaned.label());
                worktree["cleanup_reason"] = serde_json::json!("clean_worktree");
                worktree["auto_cleaned_up"] = serde_json::json!(true);
                worktree["branch_cleanup_status"] =
                    serde_json::json!(if remove_outcome.branch_deleted {
                        "deleted"
                    } else if remove_outcome.branch_delete_error.is_some() {
                        "retained"
                    } else {
                        "already_removed"
                    });
                if let Some(error) = remove_outcome.branch_delete_error.as_ref() {
                    worktree["branch_cleanup_error"] = serde_json::json!(error);
                    self.inner.storage.append_event(&AuditEvent::new(
                        "task_worktree_branch_cleanup_retained",
                        serde_json::json!({
                            "task_id": task_id,
                            "worktree_path": artifact.worktree_path,
                            "worktree_branch": artifact.worktree_branch,
                            "reason": reason,
                            "error": error,
                        }),
                    ))?;
                }
                self.inner.storage.append_event(&AuditEvent::new(
                    "worktree_auto_cleaned_up",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": reason,
                        "branch_cleanup_status": worktree["branch_cleanup_status"],
                    }),
                ))?;
                Ok(TaskOwnedWorktreeCleanup {
                    changed_files,
                    status: TaskOwnedWorktreeCleanupStatus::Cleaned,
                })
            }
            Err(err) if !artifact.worktree_path.exists() => {
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::AlreadyRemoved.label());
                worktree["cleanup_reason"] =
                    serde_json::json!("worktree_path_missing_after_remove");
                worktree["auto_cleaned_up"] = serde_json::json!(true);
                self.inner.storage.append_event(&AuditEvent::new(
                    "task_worktree_cleanup_already_removed",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": reason,
                        "remove_error": err.to_string(),
                    }),
                ))?;
                Ok(TaskOwnedWorktreeCleanup {
                    changed_files,
                    status: TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
                })
            }
            Err(err) => {
                let error = err.to_string();
                worktree["cleanup_status"] =
                    serde_json::json!(TaskOwnedWorktreeCleanupStatus::Failed.label());
                worktree["cleanup_error"] = serde_json::json!(error);
                self.inner.storage.append_event(&AuditEvent::new(
                    "worktree_auto_cleanup_failed",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": reason,
                        "error": worktree["cleanup_error"],
                    }),
                ))?;
                Ok(TaskOwnedWorktreeCleanup {
                    changed_files,
                    status: TaskOwnedWorktreeCleanupStatus::Failed,
                })
            }
        }
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

    async fn current_worktree_branch(&self, worktree_path: &Path) -> Result<String> {
        git_stdout(
            self,
            worktree_path,
            &["rev-parse", "--abbrev-ref", "HEAD"],
            "failed to inspect worktree branch",
        )
        .await
    }

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
