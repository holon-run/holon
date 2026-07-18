use super::*;

use std::ffi::OsString;

use crate::system::{
    CaptureSpec, ProcessHost, ProcessPurpose, ProcessRequest, ProgramInvocation, StdioSpec,
    WorkspaceView,
};
use crate::types::{
    AttachWorkspaceResult, CreateWorktreeResult, DetachWorkspaceResult, ExecutionRootEntry,
    RemoveWorktreeResult, SwitchWorkspaceResult, WorkspaceStateResult, WorktreeArtifactMetadata,
    WorktreeCleanupEvidence, WorktreeProvenance,
};

#[derive(Debug, Clone)]
pub(crate) enum WorkspaceSwitchTarget {
    WorkspaceId(String),
    ExecutionRootId(String),
    Path(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExistingWorktreePolicy {
    Reuse,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeBranchPolicy {
    Keep,
    DeleteIfMerged,
}

#[derive(Debug, Clone)]
struct GitCommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Default)]
struct GitWorktreeRecord {
    path: PathBuf,
    head: Option<String>,
    branch_ref: Option<String>,
    detached: bool,
    locked: Option<String>,
}

impl RuntimeHandle {
    pub(crate) async fn workspace_state_result(&self) -> Result<WorkspaceStateResult> {
        let state = self.agent_state().await?;
        let all_roots = self
            .inner
            .runtime_db
            .execution_root_entries()
            .latest_all()?;
        let related_workspace_ids = all_roots
            .iter()
            .filter(|entry| {
                state
                    .attached_workspaces
                    .iter()
                    .any(|workspace_id| workspace_id == &entry.workspace_id)
                    || entry
                        .worktree
                        .as_ref()
                        .and_then(|worktree| worktree.registered_by_agent_id.as_deref())
                        == Some(state.id.as_str())
            })
            .map(|entry| entry.workspace_id.clone())
            .collect::<HashSet<_>>();
        let mut workspaces = self
            .inner
            .storage
            .latest_workspace_entries()?
            .into_iter()
            .filter(|workspace| {
                state
                    .attached_workspaces
                    .iter()
                    .any(|workspace_id| workspace_id == &workspace.workspace_id)
                    || related_workspace_ids.contains(&workspace.workspace_id)
            })
            .collect::<Vec<_>>();
        workspaces.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));

        let mut execution_roots = all_roots
            .into_iter()
            .filter(|entry| {
                state
                    .attached_workspaces
                    .iter()
                    .any(|workspace_id| workspace_id == &entry.workspace_id)
                    || entry
                        .worktree
                        .as_ref()
                        .and_then(|worktree| worktree.registered_by_agent_id.as_deref())
                        == Some(state.id.as_str())
            })
            .collect::<Vec<_>>();
        for entry in execution_roots.iter_mut().filter(|entry| {
            entry.root_kind == WorkspaceProjectionKind::GitWorktreeRoot
                && entry.removed_at.is_none()
        }) {
            let Some(workspace) = workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == entry.workspace_id)
            else {
                continue;
            };
            let inspection = async {
                let record = self
                    .git_worktree_records(workspace)
                    .await?
                    .into_iter()
                    .find(|record| record.path == entry.filesystem_path)
                    .ok_or_else(|| anyhow!("worktree is missing from live git state"))?;
                let changed_files = self.changed_files(workspace, &record.path).await?;
                Ok::<_, anyhow::Error>((record, changed_files))
            }
            .await;
            match inspection {
                Ok((record, changed_files)) => {
                    let worktree = entry.worktree.get_or_insert(WorktreeArtifactMetadata {
                        provenance: WorktreeProvenance::Discovered,
                        registered_by_agent_id: Some(state.id.clone()),
                        authorized_agent_ids: vec![state.id.clone()],
                        branch: None,
                        branch_ref: None,
                        head_commit: None,
                        detached: false,
                        requested_base_ref: None,
                        resolved_base_commit: None,
                        git_common_dir: None,
                        git_dir: None,
                        last_cleanup: None,
                    });
                    worktree.branch = record
                        .branch_ref
                        .as_deref()
                        .and_then(short_branch_name)
                        .map(str::to_owned);
                    worktree.branch_ref = record.branch_ref;
                    worktree.head_commit = record.head;
                    worktree.detached = record.detached;
                    worktree.last_cleanup = Some(WorktreeCleanupEvidence {
                        status: if changed_files.is_empty() {
                            "clean".into()
                        } else {
                            "dirty".into()
                        },
                        changed_files,
                        error: record
                            .locked
                            .filter(|reason| !reason.is_empty())
                            .map(|reason| format!("worktree is locked: {reason}")),
                        inspected_at: chrono::Utc::now(),
                    });
                }
                Err(error) => {
                    let worktree = entry.worktree.get_or_insert(WorktreeArtifactMetadata {
                        provenance: WorktreeProvenance::Discovered,
                        registered_by_agent_id: Some(state.id.clone()),
                        authorized_agent_ids: vec![state.id.clone()],
                        branch: None,
                        branch_ref: None,
                        head_commit: None,
                        detached: false,
                        requested_base_ref: None,
                        resolved_base_commit: None,
                        git_common_dir: None,
                        git_dir: None,
                        last_cleanup: None,
                    });
                    worktree.last_cleanup = Some(WorktreeCleanupEvidence {
                        status: "inspection_failed".into(),
                        changed_files: Vec::new(),
                        error: Some(error.to_string()),
                        inspected_at: chrono::Utc::now(),
                    });
                }
            }
        }
        execution_roots.sort_by(|left, right| {
            left.workspace_id
                .cmp(&right.workspace_id)
                .then_with(|| left.execution_root_id.cmp(&right.execution_root_id))
        });

        let mut occupancies = match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge.workspace_occupancies().await?,
            None => self.inner.storage.latest_workspace_occupancies()?,
        };
        let root_ids = execution_roots
            .iter()
            .map(|entry| entry.execution_root_id.as_str())
            .collect::<HashSet<_>>();
        occupancies.retain(|record| {
            record.released_at.is_none() && root_ids.contains(record.execution_root_id.as_str())
        });
        occupancies.sort_by(|left, right| left.occupancy_id.cmp(&right.occupancy_id));

        Ok(WorkspaceStateResult {
            agent_id: state.id,
            attached_workspace_ids: state.attached_workspaces,
            workspaces,
            active: state.active_workspace_entry,
            execution_roots,
            occupancies,
            summary_text: Some(
                "workspace bindings, active projection, and worktree artifacts".into(),
            ),
        })
    }

    pub(crate) async fn attach_workspace_path(
        &self,
        path: PathBuf,
    ) -> Result<AttachWorkspaceResult> {
        let discovery = crate::system::workspace::discover_workspace_path(&path)?;
        let workspace = self
            .ensure_workspace_entry_for_path(discovery.workspace_anchor.clone())
            .await?;
        let state = self.agent_state().await?;
        let already_attached = state
            .attached_workspaces
            .iter()
            .any(|workspace_id| workspace_id == &workspace.workspace_id);
        if !already_attached {
            self.attach_workspace(&workspace).await?;
        }
        Ok(AttachWorkspaceResult {
            workspace,
            already_attached,
            active_unchanged: true,
            discovered_projection_kind: discovery.projection_kind,
            discovered_execution_root: Some(discovery.execution_root),
            summary_text: Some(if already_attached {
                "workspace was already attached; active projection was unchanged".into()
            } else {
                "workspace attached; active projection was unchanged".into()
            }),
        })
    }

    pub(crate) async fn detach_workspace_with_fallback(
        &self,
        workspace_id: &str,
    ) -> Result<DetachWorkspaceResult> {
        let state = self.agent_state().await?;
        let canonical_agent_home_id = crate::types::agent_home_workspace_id(&state.id);
        if workspace_id == AGENT_HOME_WORKSPACE_ID || workspace_id == canonical_agent_home_id {
            return Err(anyhow!("AgentHome cannot be detached"));
        }
        let switched_to_agent_home = state
            .active_workspace_entry
            .as_ref()
            .is_some_and(|entry| entry.workspace_id == workspace_id);
        let retained_execution_roots = self
            .inner
            .runtime_db
            .execution_root_entries()
            .active_for_workspace(workspace_id)?
            .into_iter()
            .filter(|entry| entry.root_kind == WorkspaceProjectionKind::GitWorktreeRoot)
            .collect::<Vec<_>>();
        self.detach_workspace(workspace_id).await?;
        Ok(DetachWorkspaceResult {
            workspace_id: workspace_id.to_string(),
            detached: true,
            switched_to_agent_home,
            retained_execution_roots,
            summary_text: Some(if switched_to_agent_home {
                "switched to agent_home and detached workspace; worktree artifacts were retained"
                    .into()
            } else {
                "workspace detached; worktree artifacts were retained".into()
            }),
        })
    }

    pub(crate) async fn switch_workspace_target(
        &self,
        target: WorkspaceSwitchTarget,
        requested_cwd: Option<PathBuf>,
    ) -> Result<SwitchWorkspaceResult> {
        let state = self.agent_state().await?;
        let (workspace, execution_root, projection_kind, default_cwd, selected_root_id) =
            match target {
                WorkspaceSwitchTarget::WorkspaceId(workspace_id) => {
                    if workspace_id == AGENT_HOME_WORKSPACE_ID
                        || workspace_id == crate::types::agent_home_workspace_id(&state.id)
                    {
                        self.activate_agent_home(
                            WorkspaceAccessMode::SharedRead,
                            requested_cwd.clone(),
                        )
                        .await?;
                        return self.switch_workspace_result("switched").await;
                    }
                    let workspace = self
                        .workspace_entry_for_use(&workspace_id)
                        .await?
                        .ok_or_else(|| anyhow!("workspace `{workspace_id}` was not found"))?;
                    if !state
                        .attached_workspaces
                        .iter()
                        .any(|attached| attached == &workspace.workspace_id)
                    {
                        return Err(anyhow!("workspace `{workspace_id}` is not attached"));
                    }
                    (
                        workspace.clone(),
                        workspace.workspace_anchor.clone(),
                        WorkspaceProjectionKind::CanonicalRoot,
                        workspace.workspace_anchor,
                        None,
                    )
                }
                WorkspaceSwitchTarget::ExecutionRootId(execution_root_id) => {
                    let entry = self
                        .inner
                        .runtime_db
                        .execution_root_entries()
                        .get(&execution_root_id)?
                        .ok_or_else(|| {
                            anyhow!("execution root `{execution_root_id}` was not found")
                        })?;
                    if entry.removed_at.is_some() {
                        return Err(anyhow!("execution root `{execution_root_id}` was removed"));
                    }
                    if !state
                        .attached_workspaces
                        .iter()
                        .any(|attached| attached == &entry.workspace_id)
                    {
                        return Err(anyhow!(
                            "workspace `{}` is not attached; attach it before switching",
                            entry.workspace_id
                        ));
                    }
                    let workspace = self
                        .workspace_entry_for_use(&entry.workspace_id)
                        .await?
                        .ok_or_else(|| {
                            anyhow!("workspace `{}` was not found", entry.workspace_id)
                        })?;
                    (
                        workspace,
                        entry.filesystem_path.clone(),
                        entry.root_kind,
                        entry.filesystem_path,
                        Some(entry.execution_root_id),
                    )
                }
                WorkspaceSwitchTarget::Path(path) => {
                    let discovery = crate::system::workspace::discover_workspace_path(&path)?;
                    let workspace = self
                        .inner
                        .storage
                        .latest_workspace_entries()?
                        .into_iter()
                        .find(|entry| {
                            entry.workspace_anchor == discovery.workspace_anchor
                                && state
                                    .attached_workspaces
                                    .iter()
                                    .any(|attached| attached == &entry.workspace_id)
                        })
                        .ok_or_else(|| {
                            anyhow!(
                            "workspace for `{}` is not attached; attach canonical root `{}` first",
                            path.display(),
                            discovery.workspace_anchor.display()
                        )
                        })?;
                    (
                        workspace,
                        discovery.execution_root,
                        discovery.projection_kind,
                        discovery.cwd,
                        None,
                    )
                }
            };

        let cwd = requested_cwd.or(Some(default_cwd));
        let execution_root_id = selected_root_id.unwrap_or(Self::build_execution_root_id(
            &workspace.workspace_id,
            projection_kind,
            &execution_root,
        )?);
        if state.active_workspace_entry.as_ref().is_some_and(|entry| {
            entry.execution_root_id == execution_root_id
                && cwd.as_ref().is_none_or(|requested| requested == &entry.cwd)
        }) {
            return self.switch_workspace_result("no_op").await;
        }

        match projection_kind {
            WorkspaceProjectionKind::CanonicalRoot => {
                self.enter_workspace(
                    &workspace,
                    WorkspaceProjectionKind::CanonicalRoot,
                    WorkspaceAccessMode::SharedRead,
                    cwd,
                    None,
                )
                .await?;
            }
            WorkspaceProjectionKind::GitWorktreeRoot => {
                let records = self.git_worktree_records(&workspace).await?;
                let record = records
                    .iter()
                    .find(|record| record.path == execution_root)
                    .ok_or_else(|| anyhow!("worktree is missing from live git state"))?;
                let registered = self
                    .register_worktree_artifact(
                        &state.id,
                        &workspace,
                        record,
                        WorktreeProvenance::Discovered,
                        None,
                        None,
                    )
                    .await?;
                self.enter_existing_git_worktree_with_id(
                    &workspace,
                    execution_root,
                    WorkspaceAccessMode::ExclusiveWrite,
                    cwd,
                    registered.execution_root_id,
                )
                .await?;
            }
        }
        self.switch_workspace_result("switched").await
    }

    async fn switch_workspace_result(&self, disposition: &str) -> Result<SwitchWorkspaceResult> {
        let state = self.agent_state().await?;
        let active = state
            .active_workspace_entry
            .ok_or_else(|| anyhow!("agent has no active workspace"))?;
        Ok(SwitchWorkspaceResult {
            disposition: disposition.to_string(),
            workspace_id: active.workspace_id,
            workspace_anchor: active.workspace_anchor,
            execution_root_id: active.execution_root_id,
            execution_root: active.execution_root,
            cwd: active.cwd,
            projection_kind: active.projection_kind,
            access_mode: active.access_mode,
            summary_text: Some(format!("{disposition} active workspace projection")),
        })
    }

    pub(crate) async fn create_worktree_for_workspace(
        &self,
        workspace_id: &str,
        branch: &str,
        base_ref: &str,
        label: Option<&str>,
        activate: bool,
        on_existing: ExistingWorktreePolicy,
    ) -> Result<CreateWorktreeResult> {
        let state = self.agent_state().await?;
        if !state.execution_profile.supports_managed_worktrees {
            return Err(anyhow!(
                "managed worktrees are disabled by the execution profile"
            ));
        }
        if !state
            .attached_workspaces
            .iter()
            .any(|attached| attached == workspace_id)
        {
            return Err(anyhow!("workspace `{workspace_id}` is not attached"));
        }
        let workspace = self
            .workspace_entry_for_use(workspace_id)
            .await?
            .ok_or_else(|| anyhow!("workspace `{workspace_id}` was not found"))?;
        let base_commit = self
            .git_stdout(
                &workspace,
                &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
            )
            .await?;
        let branch_ref = format!("refs/heads/{branch}");
        let records = self.git_worktree_records(&workspace).await?;
        let matching = records
            .iter()
            .filter(|record| record.branch_ref.as_deref() == Some(branch_ref.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let branch_tip = self
            .git_optional_stdout(
                &workspace,
                &["rev-parse", "--verify", &format!("{branch_ref}^{{commit}}")],
            )
            .await?;

        if !matching.is_empty() {
            if matching.len() != 1 {
                return Ok(CreateWorktreeResult {
                    disposition: "conflict".into(),
                    created: false,
                    reused: false,
                    activated: false,
                    base_ref_applied: false,
                    requested_base_ref: base_ref.into(),
                    resolved_base_commit: base_commit,
                    branch: branch.into(),
                    branch_ref,
                    branch_tip,
                    execution_root_id: None,
                    worktree_path: None,
                    conflict: Some("multiple live worktrees match the requested branch".into()),
                    candidates: self.registered_candidates(workspace_id, &matching)?,
                    activation_error: None,
                    summary_text: Some("branch match is ambiguous; no state changed".into()),
                });
            }
            let record = &matching[0];
            if !record.path.is_dir() {
                return Ok(CreateWorktreeResult {
                    disposition: "conflict".into(),
                    created: false,
                    reused: false,
                    activated: false,
                    base_ref_applied: false,
                    requested_base_ref: base_ref.into(),
                    resolved_base_commit: base_commit,
                    branch: branch.into(),
                    branch_ref,
                    branch_tip,
                    execution_root_id: None,
                    worktree_path: Some(record.path.clone()),
                    conflict: Some("matching worktree backing path is missing".into()),
                    candidates: self.registered_candidates(workspace_id, &matching)?,
                    activation_error: None,
                    summary_text: Some("missing worktree path conflict; no state changed".into()),
                });
            }
            if record.path == workspace.workspace_anchor {
                return Ok(CreateWorktreeResult {
                    disposition: "conflict".into(),
                    created: false,
                    reused: false,
                    activated: false,
                    base_ref_applied: false,
                    requested_base_ref: base_ref.into(),
                    resolved_base_commit: base_commit,
                    branch: branch.into(),
                    branch_ref,
                    branch_tip,
                    execution_root_id: None,
                    worktree_path: Some(record.path.clone()),
                    conflict: Some("branch is checked out by the canonical workspace".into()),
                    candidates: Vec::new(),
                    activation_error: None,
                    summary_text: Some(
                        "canonical checkout already uses branch; no state changed".into(),
                    ),
                });
            }
            if on_existing == ExistingWorktreePolicy::Error {
                return Ok(CreateWorktreeResult {
                    disposition: "already_exists".into(),
                    created: false,
                    reused: false,
                    activated: false,
                    base_ref_applied: false,
                    requested_base_ref: base_ref.into(),
                    resolved_base_commit: base_commit,
                    branch: branch.into(),
                    branch_ref,
                    branch_tip,
                    execution_root_id: None,
                    worktree_path: Some(record.path.clone()),
                    conflict: Some("matching live worktree already exists".into()),
                    candidates: Vec::new(),
                    activation_error: None,
                    summary_text: Some("create-only policy rejected existing worktree".into()),
                });
            }
            let entry = self
                .register_worktree_artifact(
                    &state.id,
                    &workspace,
                    record,
                    WorktreeProvenance::Discovered,
                    Some(base_ref),
                    Some(&base_commit),
                )
                .await?;
            let activation_error = if activate {
                self.switch_workspace_target(
                    WorkspaceSwitchTarget::ExecutionRootId(entry.execution_root_id.clone()),
                    None,
                )
                .await
                .err()
                .map(|error| error.to_string())
            } else {
                None
            };
            return Ok(CreateWorktreeResult {
                disposition: "reused".into(),
                created: false,
                reused: true,
                activated: activate && activation_error.is_none(),
                base_ref_applied: false,
                requested_base_ref: base_ref.into(),
                resolved_base_commit: base_commit,
                branch: branch.into(),
                branch_ref,
                branch_tip: record.head.clone().or(branch_tip),
                execution_root_id: Some(entry.execution_root_id),
                worktree_path: Some(record.path.clone()),
                conflict: None,
                candidates: Vec::new(),
                activation_error,
                summary_text: Some(
                    "reused the unique live linked worktree without applying base_ref".into(),
                ),
            });
        }

        if branch_tip.is_some() {
            return Ok(CreateWorktreeResult {
                disposition: "conflict".into(),
                created: false,
                reused: false,
                activated: false,
                base_ref_applied: false,
                requested_base_ref: base_ref.into(),
                resolved_base_commit: base_commit,
                branch: branch.into(),
                branch_ref,
                branch_tip,
                execution_root_id: None,
                worktree_path: None,
                conflict: Some("branch exists without a live linked worktree".into()),
                candidates: Vec::new(),
                activation_error: None,
                summary_text: Some("branch-only conflict; no state changed".into()),
            });
        }

        let repo_name = workspace
            .workspace_anchor
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo");
        let managed_root = workspace
            .workspace_anchor
            .parent()
            .unwrap_or(workspace.workspace_anchor.as_path())
            .join(format!(".holon-worktrees-{repo_name}"));
        std::fs::create_dir_all(&managed_root)?;
        let readable = label.unwrap_or(branch);
        let worktree_path = managed_root.join(format!(
            "{}-{}",
            sanitize_label(readable),
            crate::ids::runtime_id("wt")
        ));
        let output = self
            .run_git(
                &workspace,
                &[
                    OsString::from("worktree"),
                    OsString::from("add"),
                    OsString::from("-b"),
                    OsString::from(branch),
                    worktree_path.as_os_str().to_os_string(),
                    OsString::from(&base_commit),
                ],
                Some(workspace.workspace_anchor.clone()),
            )
            .await?;
        if !output.success {
            return Err(anyhow!("git worktree add failed: {}", output.stderr));
        }
        let record = self
            .git_worktree_records(&workspace)
            .await?
            .into_iter()
            .find(|record| record.path == worktree_path)
            .ok_or_else(|| anyhow!("created worktree was not reported by git"))?;
        let entry = self
            .register_worktree_artifact(
                &state.id,
                &workspace,
                &record,
                WorktreeProvenance::RuntimeCreated,
                Some(base_ref),
                Some(&base_commit),
            )
            .await?;
        let activation_error = if activate {
            self.switch_workspace_target(
                WorkspaceSwitchTarget::ExecutionRootId(entry.execution_root_id.clone()),
                None,
            )
            .await
            .err()
            .map(|error| error.to_string())
        } else {
            None
        };
        self.append_audit_event(
            "worktree_created",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "execution_root_id": entry.execution_root_id,
                "branch": branch,
                "requested_base_ref": base_ref,
                "resolved_base_commit": base_commit,
                "activated": activate && activation_error.is_none(),
            }),
        )?;
        Ok(CreateWorktreeResult {
            disposition: "created".into(),
            created: true,
            reused: false,
            activated: activate && activation_error.is_none(),
            base_ref_applied: true,
            requested_base_ref: base_ref.into(),
            resolved_base_commit: base_commit,
            branch: branch.into(),
            branch_ref,
            branch_tip: record.head,
            execution_root_id: Some(entry.execution_root_id),
            worktree_path: Some(worktree_path),
            conflict: None,
            candidates: Vec::new(),
            activation_error,
            summary_text: Some("created and registered linked worktree".into()),
        })
    }

    pub(crate) async fn remove_registered_worktree(
        &self,
        execution_root_id: &str,
        return_to: Option<&str>,
        branch_policy: WorktreeBranchPolicy,
        merged_into: Option<&str>,
    ) -> Result<RemoveWorktreeResult> {
        let mut entry = self
            .inner
            .runtime_db
            .execution_root_entries()
            .get(execution_root_id)?
            .ok_or_else(|| anyhow!("execution root `{execution_root_id}` was not found"))?;
        if entry.root_kind != WorkspaceProjectionKind::GitWorktreeRoot {
            return Err(anyhow!("canonical workspace roots cannot be removed"));
        }
        if entry.removed_at.is_some() {
            return Ok(RemoveWorktreeResult {
                execution_root_id: execution_root_id.into(),
                disposition: "already_removed".into(),
                switched: false,
                removed: false,
                branch_deleted: false,
                branch: entry
                    .worktree
                    .as_ref()
                    .and_then(|worktree| worktree.branch.clone()),
                branch_tip: entry
                    .worktree
                    .as_ref()
                    .and_then(|worktree| worktree.head_commit.clone()),
                changed_files: Vec::new(),
                error: None,
                summary_text: Some("worktree was already removed".into()),
            });
        }
        let workspace = self
            .workspace_entry_for_use(&entry.workspace_id)
            .await?
            .ok_or_else(|| anyhow!("workspace `{}` was not found", entry.workspace_id))?;
        let state = self.agent_state().await?;
        let worktree_metadata = entry
            .worktree
            .as_ref()
            .ok_or_else(|| anyhow!("registered worktree identity metadata is unavailable"))?;
        let authorized = worktree_metadata
            .authorized_agent_ids
            .iter()
            .any(|agent_id| agent_id == &state.id)
            || (worktree_metadata.authorized_agent_ids.is_empty()
                && worktree_metadata.registered_by_agent_id.as_deref() == Some(state.id.as_str()));
        if !authorized {
            return Err(anyhow!(
                "agent `{}` is not authorized to remove execution root `{execution_root_id}`",
                state.id
            ));
        }
        let active_target = state
            .active_workspace_entry
            .as_ref()
            .is_some_and(|active| active.execution_root_id == execution_root_id);
        let return_target = if active_target {
            let return_to =
                return_to.ok_or_else(|| anyhow!("active worktree requires `return_to`"))?;
            if return_to == execution_root_id {
                return Err(anyhow!("return_to must select a different execution root"));
            }
            Some(match return_to {
                "canonical" => WorkspaceSwitchTarget::WorkspaceId(entry.workspace_id.clone()),
                "agent_home" => WorkspaceSwitchTarget::WorkspaceId(AGENT_HOME_WORKSPACE_ID.into()),
                execution_root_id => {
                    WorkspaceSwitchTarget::ExecutionRootId(execution_root_id.to_string())
                }
            })
        } else {
            None
        };

        let occupancies = match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge.workspace_occupancies().await?,
            None => self.inner.storage.latest_workspace_occupancies()?,
        };
        let active_occupancy_id = active_target
            .then(|| {
                state
                    .active_workspace_entry
                    .as_ref()
                    .and_then(|active| active.occupancy_id.as_deref())
            })
            .flatten();
        let blocking_holders = occupancies
            .into_iter()
            .filter(|record| {
                record.execution_root_id == execution_root_id
                    && record.released_at.is_none()
                    && Some(record.occupancy_id.as_str()) != active_occupancy_id
            })
            .collect::<Vec<_>>();
        if !blocking_holders.is_empty() {
            return Err(anyhow!(
                "worktree has active occupancy outside the current projection"
            ));
        }

        let records = self.git_worktree_records(&workspace).await?;
        let record = records
            .into_iter()
            .find(|record| record.path == entry.filesystem_path)
            .ok_or_else(|| anyhow!("registered worktree is missing from live git state"))?;
        let expected_common_dir = worktree_metadata
            .git_common_dir
            .as_ref()
            .ok_or_else(|| anyhow!("registered repository identity is unavailable"))?;
        let actual_common_dir_text = self
            .git_stdout_at(&workspace, &record.path, &["rev-parse", "--git-common-dir"])
            .await?;
        let actual_common_dir =
            crate::system::workspace::normalize_path(&if Path::new(&actual_common_dir_text)
                .is_absolute()
            {
                PathBuf::from(actual_common_dir_text)
            } else {
                record.path.join(actual_common_dir_text)
            })?;
        let expected_git_dir = worktree_metadata
            .git_dir
            .as_ref()
            .ok_or_else(|| anyhow!("registered worktree generation identity is unavailable"))?;
        let actual_git_dir_text = self
            .git_stdout_at(&workspace, &record.path, &["rev-parse", "--git-dir"])
            .await?;
        let actual_git_dir =
            crate::system::workspace::normalize_path(&if Path::new(&actual_git_dir_text)
                .is_absolute()
            {
                PathBuf::from(actual_git_dir_text)
            } else {
                record.path.join(actual_git_dir_text)
            })?;
        if actual_common_dir != *expected_common_dir || actual_git_dir != *expected_git_dir {
            return Err(anyhow!(
                "registered worktree generation does not match live git state"
            ));
        }
        if worktree_metadata.branch_ref != record.branch_ref
            || worktree_metadata.detached != record.detached
        {
            return Err(anyhow!(
                "registered worktree checkout identity does not match live git state"
            ));
        }
        if let Some(locked) = record.locked.as_deref() {
            return Err(anyhow!("worktree is locked: {locked}"));
        }
        let changed_files = self.changed_files(&workspace, &record.path).await?;
        if !changed_files.is_empty() {
            update_cleanup_evidence(&mut entry, "retained_dirty", changed_files.clone(), None);
            self.inner
                .runtime_db
                .execution_root_entries()
                .upsert(&entry)?;
            self.append_audit_event(
                "worktree_cleanup_retained",
                serde_json::json!({
                    "execution_root_id": execution_root_id,
                    "changed_files": changed_files,
                }),
            )?;
            return Ok(RemoveWorktreeResult {
                execution_root_id: execution_root_id.into(),
                disposition: "retained_dirty".into(),
                switched: false,
                removed: false,
                branch_deleted: false,
                branch: entry
                    .worktree
                    .as_ref()
                    .and_then(|worktree| worktree.branch.clone()),
                branch_tip: record.head,
                changed_files,
                error: None,
                summary_text: Some("dirty worktree retained".into()),
            });
        }

        let branch = record.branch_ref.as_deref().and_then(short_branch_name);
        let tip = record.head.clone();
        if record.detached && merged_into.is_none() {
            return Err(anyhow!(
                "detached HEAD removal requires `merged_into` reachability proof"
            ));
        }
        if branch_policy == WorktreeBranchPolicy::DeleteIfMerged || record.detached {
            let merged_into =
                merged_into.ok_or_else(|| anyhow!("`merged_into` is required by branch policy"))?;
            let tip = tip
                .as_deref()
                .ok_or_else(|| anyhow!("worktree HEAD commit is unavailable"))?;
            if !self
                .git_success(
                    &workspace,
                    &["merge-base", "--is-ancestor", tip, merged_into],
                )
                .await?
            {
                return Err(anyhow!(
                    "worktree HEAD is not merged into `{merged_into}`; artifact retained"
                ));
            }
        }

        let switched = if let Some(target) = return_target {
            self.switch_workspace_target(target, None).await?;
            true
        } else {
            false
        };
        let _cleanup_lease = match self.inner.host_bridge.as_ref() {
            Some(bridge) => Some(
                bridge
                    .acquire_workspace_cleanup_lease(execution_root_id)
                    .await?,
            ),
            None => None,
        };
        let remove = self
            .run_git(
                &workspace,
                &[
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    entry.filesystem_path.as_os_str().to_os_string(),
                ],
                Some(workspace.workspace_anchor.clone()),
            )
            .await?;
        if !remove.success {
            update_cleanup_evidence(
                &mut entry,
                "failed",
                Vec::new(),
                Some(remove.stderr.clone()),
            );
            self.inner
                .runtime_db
                .execution_root_entries()
                .upsert(&entry)?;
            return Ok(RemoveWorktreeResult {
                execution_root_id: execution_root_id.into(),
                disposition: "failed".into(),
                switched,
                removed: false,
                branch_deleted: false,
                branch: branch.map(ToString::to_string),
                branch_tip: tip,
                changed_files: Vec::new(),
                error: Some(remove.stderr),
                summary_text: Some("git refused worktree removal; artifact retained".into()),
            });
        }

        self.inner
            .runtime_db
            .execution_root_entries()
            .mark_removed(execution_root_id)?;
        let mut branch_deleted = false;
        if branch_policy == WorktreeBranchPolicy::DeleteIfMerged {
            if let (Some(branch_ref), Some(tip)) = (record.branch_ref.as_deref(), tip.as_deref()) {
                let delete = self
                    .run_git(
                        &workspace,
                        &[
                            OsString::from("update-ref"),
                            OsString::from("-d"),
                            OsString::from(branch_ref),
                            OsString::from(tip),
                        ],
                        Some(workspace.workspace_anchor.clone()),
                    )
                    .await?;
                if !delete.success {
                    return Ok(RemoveWorktreeResult {
                        execution_root_id: execution_root_id.into(),
                        disposition: "removed_branch_retained".into(),
                        switched,
                        removed: true,
                        branch_deleted: false,
                        branch: branch.map(ToString::to_string),
                        branch_tip: Some(tip.to_string()),
                        changed_files: Vec::new(),
                        error: Some(delete.stderr),
                        summary_text: Some(
                            "worktree removed but branch deletion failed safely".into(),
                        ),
                    });
                }
                branch_deleted = true;
            }
        }
        self.append_audit_event(
            "worktree_cleanup_removed",
            serde_json::json!({
                "execution_root_id": execution_root_id,
                "branch": branch,
                "branch_deleted": branch_deleted,
            }),
        )?;
        Ok(RemoveWorktreeResult {
            execution_root_id: execution_root_id.into(),
            disposition: "removed".into(),
            switched,
            removed: true,
            branch_deleted,
            branch: branch.map(ToString::to_string),
            branch_tip: tip,
            changed_files: Vec::new(),
            error: None,
            summary_text: Some("worktree removed safely".into()),
        })
    }

    fn registered_candidates(
        &self,
        workspace_id: &str,
        records: &[GitWorktreeRecord],
    ) -> Result<Vec<ExecutionRootEntry>> {
        let paths = records
            .iter()
            .map(|record| record.path.as_path())
            .collect::<HashSet<_>>();
        Ok(self
            .inner
            .runtime_db
            .execution_root_entries()
            .active_for_workspace(workspace_id)?
            .into_iter()
            .filter(|entry| paths.contains(entry.filesystem_path.as_path()))
            .collect())
    }

    async fn register_worktree_artifact(
        &self,
        agent_id: &str,
        workspace: &WorkspaceEntry,
        record: &GitWorktreeRecord,
        provenance: WorktreeProvenance,
        requested_base_ref: Option<&str>,
        resolved_base_commit: Option<&str>,
    ) -> Result<ExecutionRootEntry> {
        let base_execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            WorkspaceProjectionKind::GitWorktreeRoot,
            &record.path,
        )?;
        let common_dir_text = self
            .git_stdout_at(workspace, &record.path, &["rev-parse", "--git-common-dir"])
            .await?;
        let common_dir = crate::system::workspace::normalize_path(&if Path::new(&common_dir_text)
            .is_absolute()
        {
            PathBuf::from(common_dir_text)
        } else {
            record.path.join(common_dir_text)
        })?;
        let git_dir_text = self
            .git_stdout_at(workspace, &record.path, &["rev-parse", "--git-dir"])
            .await?;
        let git_dir =
            crate::system::workspace::normalize_path(&if Path::new(&git_dir_text).is_absolute() {
                PathBuf::from(git_dir_text)
            } else {
                record.path.join(git_dir_text)
            })?;
        let repository = self.inner.runtime_db.execution_root_entries();
        let active_entries = repository.active_for_workspace(&workspace.workspace_id)?;
        let existing = active_entries
            .iter()
            .find(|entry| {
                entry.filesystem_path == record.path
                    && entry.worktree.as_ref().is_none_or(|worktree| {
                        worktree
                            .git_dir
                            .as_ref()
                            .is_none_or(|registered| registered == &git_dir)
                    })
            })
            .cloned();
        for stale in active_entries.iter().filter(|entry| {
            entry.filesystem_path == record.path
                && entry.execution_root_id
                    != existing
                        .as_ref()
                        .map(|entry| entry.execution_root_id.as_str())
                        .unwrap_or_default()
        }) {
            repository.mark_removed(&stale.execution_root_id)?;
        }
        let execution_root_id = existing
            .as_ref()
            .map(|entry| entry.execution_root_id.clone())
            .unwrap_or_else(|| {
                if repository
                    .get(&base_execution_root_id)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    format!("{base_execution_root_id}:{}", crate::ids::runtime_id("gen"))
                } else {
                    base_execution_root_id
                }
            });
        let created_at = existing
            .as_ref()
            .map(|entry| entry.created_at)
            .unwrap_or_else(chrono::Utc::now);
        let previous_worktree = existing.as_ref().and_then(|entry| entry.worktree.as_ref());
        let registered_by_agent_id = previous_worktree
            .and_then(|worktree| worktree.registered_by_agent_id.clone())
            .or_else(|| Some(agent_id.to_string()));
        let mut authorized_agent_ids = previous_worktree
            .map(|worktree| worktree.authorized_agent_ids.clone())
            .unwrap_or_default();
        if authorized_agent_ids.is_empty() {
            if let Some(owner) = registered_by_agent_id.as_ref() {
                authorized_agent_ids.push(owner.clone());
            }
        }
        let entry = ExecutionRootEntry {
            execution_root_id,
            workspace_id: workspace.workspace_id.clone(),
            filesystem_path: record.path.clone(),
            root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: Some(WorktreeArtifactMetadata {
                provenance: existing
                    .as_ref()
                    .and_then(|entry| entry.worktree.as_ref())
                    .map(|worktree| worktree.provenance)
                    .unwrap_or(provenance),
                registered_by_agent_id,
                authorized_agent_ids,
                branch: record
                    .branch_ref
                    .as_deref()
                    .and_then(short_branch_name)
                    .map(str::to_owned),
                branch_ref: record.branch_ref.clone(),
                head_commit: record.head.clone(),
                detached: record.detached,
                requested_base_ref: requested_base_ref.map(str::to_owned).or_else(|| {
                    previous_worktree.and_then(|worktree| worktree.requested_base_ref.clone())
                }),
                resolved_base_commit: resolved_base_commit.map(str::to_owned).or_else(|| {
                    previous_worktree.and_then(|worktree| worktree.resolved_base_commit.clone())
                }),
                git_common_dir: Some(common_dir),
                git_dir: Some(git_dir),
                last_cleanup: existing
                    .and_then(|entry| entry.worktree)
                    .and_then(|worktree| worktree.last_cleanup),
            }),
            created_at,
            removed_at: None,
        };
        repository.upsert(&entry)?;
        Ok(entry)
    }

    async fn changed_files(
        &self,
        workspace: &WorkspaceEntry,
        worktree_path: &Path,
    ) -> Result<Vec<String>> {
        let status = self
            .git_stdout_at(
                workspace,
                worktree_path,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )
            .await?;
        let mut files = status
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.get(3..).unwrap_or(line).trim().to_string())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(files)
    }

    async fn git_worktree_records(
        &self,
        workspace: &WorkspaceEntry,
    ) -> Result<Vec<GitWorktreeRecord>> {
        let text = self
            .git_stdout(workspace, &["worktree", "list", "--porcelain"])
            .await?;
        parse_git_worktree_records(&text)
    }

    async fn git_success(&self, workspace: &WorkspaceEntry, args: &[&str]) -> Result<bool> {
        Ok(self
            .run_git(
                workspace,
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                Some(workspace.workspace_anchor.clone()),
            )
            .await?
            .success)
    }

    async fn git_optional_stdout(
        &self,
        workspace: &WorkspaceEntry,
        args: &[&str],
    ) -> Result<Option<String>> {
        let output = self
            .run_git(
                workspace,
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                Some(workspace.workspace_anchor.clone()),
            )
            .await?;
        Ok(output.success.then_some(output.stdout))
    }

    async fn git_stdout(&self, workspace: &WorkspaceEntry, args: &[&str]) -> Result<String> {
        self.git_stdout_at(workspace, &workspace.workspace_anchor, args)
            .await
    }

    async fn git_stdout_at(
        &self,
        workspace: &WorkspaceEntry,
        cwd: &Path,
        args: &[&str],
    ) -> Result<String> {
        let output = self
            .run_git(
                workspace,
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                Some(cwd.to_path_buf()),
            )
            .await?;
        if !output.success {
            return Err(anyhow!("git {} failed: {}", args.join(" "), output.stderr));
        }
        Ok(output.stdout)
    }

    async fn run_git(
        &self,
        workspace: &WorkspaceEntry,
        args: &[OsString],
        cwd: Option<PathBuf>,
    ) -> Result<GitCommandOutput> {
        let execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            WorkspaceProjectionKind::CanonicalRoot,
            &workspace.workspace_anchor,
        )?;
        let view = WorkspaceView::new(
            Some(workspace.workspace_id.clone()),
            workspace.workspace_anchor.clone(),
            workspace.workspace_anchor.clone(),
            workspace.workspace_anchor.clone(),
            Some(execution_root_id),
            Some(WorkspaceAccessMode::SharedRead),
            WorkspaceProjectionKind::CanonicalRoot,
            None,
        )?;
        let execution = self
            .effective_execution_for_workspace(ExecutionScopeKind::AgentTurn, view)
            .await?;
        let result = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: args.to_vec(),
                    },
                    cwd,
                    env: Vec::new(),
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await?;
        Ok(GitCommandOutput {
            success: result.exit_status.success(),
            stdout: String::from_utf8_lossy(&result.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).trim().to_string(),
        })
    }
}

fn parse_git_worktree_records(text: &str) -> Result<Vec<GitWorktreeRecord>> {
    let mut records = Vec::new();
    let mut current = GitWorktreeRecord::default();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !current.path.as_os_str().is_empty() {
                records.push(current);
                current = GitWorktreeRecord::default();
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            current.path = crate::system::workspace::normalize_path(Path::new(path))?;
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current.head = Some(head.to_string());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current.branch_ref = Some(branch.to_string());
        } else if line == "detached" {
            current.detached = true;
        } else if let Some(reason) = line.strip_prefix("locked") {
            current.locked = Some(reason.trim().to_string());
        }
    }
    Ok(records)
}

fn short_branch_name(branch_ref: &str) -> Option<&str> {
    branch_ref.strip_prefix("refs/heads/")
}

fn sanitize_label(label: &str) -> String {
    let mut value = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while value.contains("--") {
        value = value.replace("--", "-");
    }
    let value = value.trim_matches('-');
    if value.is_empty() {
        "worktree".into()
    } else {
        value.chars().take(64).collect()
    }
}

fn update_cleanup_evidence(
    entry: &mut ExecutionRootEntry,
    status: &str,
    changed_files: Vec<String>,
    error: Option<String>,
) {
    if let Some(worktree) = entry.worktree.as_mut() {
        worktree.last_cleanup = Some(WorktreeCleanupEvidence {
            status: status.to_string(),
            changed_files,
            error,
            inspected_at: chrono::Utc::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_worktree_records() {
        let records = parse_git_worktree_records(
            "worktree /tmp/repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /tmp/wt\nHEAD def\ndetached\nlocked reason\n",
        )
        .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].branch_ref.as_deref(), Some("refs/heads/main"));
        assert!(records[1].detached);
        assert_eq!(records[1].locked.as_deref(), Some("reason"));
    }

    #[test]
    fn sanitizes_worktree_label() {
        assert_eq!(sanitize_label("feature/issue 1224"), "feature-issue-1224");
        assert_eq!(sanitize_label("///"), "worktree");
    }
}
