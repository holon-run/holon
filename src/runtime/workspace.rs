use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};

use crate::{
    storage::AppStorage,
    system::{
        EffectiveExecution, ExecutionProfile, ExecutionScopeKind, ExecutionSnapshot,
        WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView,
    },
    types::{
        agent_home_workspace_id, ActiveWorkspaceEntry, AgentState, WorkspaceEntry,
        AGENT_HOME_WORKSPACE_ID,
    },
};

pub(crate) fn build_execution_root_id(
    workspace_id: &str,
    projection_kind: WorkspaceProjectionKind,
    execution_root: &Path,
) -> Result<String> {
    Ok(match projection_kind {
        WorkspaceProjectionKind::CanonicalRoot => {
            format!("canonical_root:{workspace_id}")
        }
        WorkspaceProjectionKind::GitWorktreeRoot => format!(
            "git_worktree_root:{workspace_id}:{}",
            crate::system::workspace::normalize_path(execution_root)?.display()
        ),
    })
}

pub(crate) fn agent_home_workspace_entry(data_dir: &Path, agent_id: &str) -> WorkspaceEntry {
    let mut entry = WorkspaceEntry::new(
        agent_home_workspace_id(agent_id),
        data_dir.to_path_buf(),
        Some("AgentHome".into()),
    );
    entry.workspace_alias = Some(AGENT_HOME_WORKSPACE_ID.into());
    entry.workspace_kind = Some("agent_home".into());
    entry.owner_agent_id = Some(agent_id.to_string());
    entry
}

pub(crate) fn canonicalize_agent_home_bindings(
    state: &mut AgentState,
    data_dir: &Path,
    agent_id: &str,
) -> Result<bool> {
    let canonical_id = agent_home_workspace_id(agent_id);
    let mut changed = false;

    if state
        .active_workspace_entry
        .as_ref()
        .is_some_and(|entry| entry.workspace_id == AGENT_HOME_WORKSPACE_ID)
    {
        let previous_entry = state.active_workspace_entry.as_ref();
        let access_mode = previous_entry
            .map(|entry| entry.access_mode)
            .unwrap_or(WorkspaceAccessMode::ExclusiveWrite);
        let mut entry = canonical_agent_home_active_entry(data_dir, agent_id, access_mode)?;
        if let Some(previous_cwd) = previous_entry
            .map(|entry| entry.cwd.as_path())
            .filter(|cwd| cwd.starts_with(&entry.execution_root))
        {
            entry.cwd = previous_cwd.to_path_buf();
        }
        state.active_workspace_entry = Some(entry);
        state.worktree_session = None;
        changed = true;
    }

    let mut next = Vec::with_capacity(state.attached_workspaces.len().max(1));
    for workspace_id in &state.attached_workspaces {
        let next_id = if workspace_id == AGENT_HOME_WORKSPACE_ID {
            changed = true;
            canonical_id.as_str()
        } else if workspace_id.starts_with("agent_home:") && workspace_id != &canonical_id {
            changed = true;
            continue;
        } else {
            workspace_id.as_str()
        };
        if !next.iter().any(|id| id == next_id) {
            next.push(next_id.to_string());
        } else {
            changed = true;
        }
    }

    if !next.iter().any(|id| id == &canonical_id) {
        next.insert(0, canonical_id);
        changed = true;
    } else if next.first() != Some(&canonical_id) {
        next.retain(|id| id != &canonical_id);
        next.insert(0, canonical_id);
        changed = true;
    }

    if changed {
        state.attached_workspaces = next;
        tracing::info!(
            agent_id = %agent_id,
            "workspace migration: canonicalized legacy AGENT_HOME_WORKSPACE_ID references"
        );
    }

    Ok(changed)
}

pub(crate) fn inherited_attached_workspaces_for_agent(
    parent_state: &AgentState,
    agent_id: &str,
) -> Vec<String> {
    let canonical_id = agent_home_workspace_id(agent_id);
    let mut inherited = Vec::with_capacity(parent_state.attached_workspaces.len().max(1));
    inherited.push(canonical_id);
    for workspace_id in &parent_state.attached_workspaces {
        if workspace_id == AGENT_HOME_WORKSPACE_ID || workspace_id.starts_with("agent_home:") {
            continue;
        }
        if !inherited.iter().any(|id| id == workspace_id) {
            inherited.push(workspace_id.clone());
        }
    }
    inherited
}

pub(crate) fn canonical_agent_home_active_entry(
    data_dir: &Path,
    agent_id: &str,
    access_mode: WorkspaceAccessMode,
) -> Result<ActiveWorkspaceEntry> {
    let workspace = agent_home_workspace_entry(data_dir, agent_id);
    let execution_root = crate::system::workspace::normalize_path(&workspace.workspace_anchor)?;
    let execution_root_id = build_execution_root_id(
        &workspace.workspace_id,
        WorkspaceProjectionKind::CanonicalRoot,
        &execution_root,
    )?;
    Ok(ActiveWorkspaceEntry {
        workspace_id: workspace.workspace_id,
        workspace_anchor: workspace.workspace_anchor,
        execution_root_id,
        execution_root: execution_root.clone(),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode,
        cwd: execution_root,
        occupancy_id: None,
        projection_metadata: None,
    })
}

pub(crate) fn detached_execution_root(storage: &AppStorage) -> PathBuf {
    storage.data_dir().to_path_buf()
}

impl ActiveWorkspaceEntry {
    /// Convert the active entry into an immutable `WorkspaceView`.
    /// Centralizes the field mapping that was previously duplicated across
    /// `workspace_view_from_state` and other call sites.
    pub(crate) fn to_workspace_view(&self) -> Result<WorkspaceView> {
        let worktree_root = (self.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot)
            .then(|| self.execution_root.clone());
        WorkspaceView::new(
            Some(self.workspace_id.clone()),
            self.workspace_anchor.clone(),
            self.execution_root.clone(),
            self.cwd.clone(),
            Some(self.execution_root_id.clone()),
            Some(self.access_mode),
            self.projection_kind,
            worktree_root,
        )
    }
}

pub(crate) fn workspace_view_from_state(
    state: &AgentState,
    detached_execution_root: PathBuf,
) -> Result<WorkspaceView> {
    if let Some(entry) = state.active_workspace_entry.as_ref() {
        return entry.to_workspace_view();
    }

    let execution_root = detached_execution_root;
    WorkspaceView::new(
        None,
        execution_root.clone(),
        execution_root.clone(),
        execution_root.clone(),
        None,
        Some(WorkspaceAccessMode::ExclusiveWrite),
        WorkspaceProjectionKind::CanonicalRoot,
        None,
    )
}

pub(crate) fn workspace_view_for_root(
    storage: &AppStorage,
    execution_root: PathBuf,
    cwd: PathBuf,
    worktree_root: Option<PathBuf>,
) -> Result<WorkspaceView> {
    let state = storage
        .read_agent()?
        .ok_or_else(|| anyhow!("agent state not found"))?;
    let workspace_id = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.workspace_id.clone());
    let workspace_anchor = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.workspace_anchor.clone())
        .unwrap_or_else(|| detached_execution_root(storage));
    let execution_root_id = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.execution_root_id.clone());
    let access_mode = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.access_mode);
    let projection_kind = if worktree_root.is_some() {
        WorkspaceProjectionKind::GitWorktreeRoot
    } else {
        WorkspaceProjectionKind::CanonicalRoot
    };
    WorkspaceView::new(
        workspace_id,
        workspace_anchor,
        execution_root,
        cwd,
        execution_root_id,
        access_mode,
        projection_kind,
        worktree_root,
    )
}

pub(crate) fn load_attached_workspace_entries(
    storage: &AppStorage,
) -> Result<Vec<(String, PathBuf)>> {
    let entries = storage.latest_workspace_entries()?;
    Ok(entries
        .into_iter()
        .map(|entry| (entry.workspace_id, entry.workspace_anchor))
        .collect())
}

pub(crate) fn load_attached_workspace_entries_for(
    storage: &AppStorage,
    attached_workspace_ids: &[String],
    workspace: &WorkspaceView,
) -> Result<Vec<(String, PathBuf)>> {
    let known_entries = load_attached_workspace_entries(storage)?
        .into_iter()
        .collect::<HashMap<_, _>>();
    let active_workspace_id = workspace.workspace_id().map(ToString::to_string);
    let mut ordered_ids = Vec::new();

    if let Some(active_id) = active_workspace_id.as_ref() {
        if attached_workspace_ids.is_empty()
            || attached_workspace_ids.iter().any(|id| id == active_id)
        {
            ordered_ids.push(active_id.clone());
        }
    }

    for workspace_id in attached_workspace_ids {
        if !ordered_ids.iter().any(|id| id == workspace_id) {
            ordered_ids.push(workspace_id.clone());
        }
    }

    let mut resolved = Vec::new();
    for workspace_id in ordered_ids {
        if let Some(anchor) = known_entries.get(&workspace_id) {
            resolved.push((workspace_id, anchor.clone()));
        } else if active_workspace_id.as_deref() == Some(workspace_id.as_str()) {
            resolved.push((workspace_id, workspace.workspace_anchor().to_path_buf()));
        }
    }

    Ok(resolved)
}

pub(crate) fn execution_snapshot_for_view(
    profile: ExecutionProfile,
    workspace: &WorkspaceView,
    attached_workspace_ids: &[String],
    storage: &AppStorage,
) -> ExecutionSnapshot {
    let attached_workspaces =
        load_attached_workspace_entries_for(storage, attached_workspace_ids, workspace)
            .unwrap_or_else(|_| {
                workspace
                    .workspace_id()
                    .map(|id| (id.to_string(), workspace.workspace_anchor().to_path_buf()))
                    .into_iter()
                    .collect()
            });

    ExecutionSnapshot {
        policy: profile.policy_snapshot(),
        profile,
        attached_workspaces,
        workspace_id: workspace.workspace_id().map(ToString::to_string),
        workspace_anchor: workspace.workspace_anchor().to_path_buf(),
        execution_root: workspace.execution_root().to_path_buf(),
        cwd: workspace.cwd().to_path_buf(),
        execution_root_id: workspace.execution_root_id().map(ToString::to_string),
        projection_kind: if workspace.worktree_root().is_some() {
            Some(WorkspaceProjectionKind::GitWorktreeRoot)
        } else {
            Some(WorkspaceProjectionKind::CanonicalRoot)
        },
        access_mode: workspace.access_mode(),
        worktree_root: workspace.worktree_root().map(|path| path.to_path_buf()),
    }
}

pub(crate) fn build_effective_execution(
    storage: &AppStorage,
    scope: ExecutionScopeKind,
    profile: ExecutionProfile,
    workspace: WorkspaceView,
    attached_workspace_ids: &[String],
) -> EffectiveExecution {
    let attached_workspaces =
        load_attached_workspace_entries_for(storage, attached_workspace_ids, &workspace)
            .unwrap_or_else(|_| {
                workspace
                    .workspace_id()
                    .map(|id| (id.to_string(), workspace.workspace_anchor().to_path_buf()))
                    .into_iter()
                    .collect()
            });

    EffectiveExecution {
        profile,
        workspace,
        scope,
        attached_workspaces,
    }
}

pub(crate) fn workspace_anchor_for_state_ref<'a>(state: &'a AgentState) -> Option<&'a Path> {
    state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.workspace_anchor.as_path())
}

pub(crate) fn execution_root_sync(storage: &AppStorage) -> PathBuf {
    storage
        .read_agent()
        .ok()
        .flatten()
        .and_then(|state| {
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.execution_root.clone())
                .or_else(|| {
                    state
                        .worktree_session
                        .as_ref()
                        .map(|worktree| worktree.worktree_path.clone())
                })
        })
        .unwrap_or_else(|| detached_execution_root(storage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActiveWorkspaceEntry;

    // ── build_execution_root_id ──

    #[test]
    fn execution_root_id_canonical_root() {
        let id = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::CanonicalRoot,
            Path::new("/tmp/project"),
        )
        .unwrap();
        assert_eq!(id, "canonical_root:ws_abc");
    }

    #[test]
    fn execution_root_id_git_worktree_root_includes_normalized_path() {
        let id = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::GitWorktreeRoot,
            Path::new("/tmp/project/worktree-x"),
        )
        .unwrap();
        assert!(id.starts_with("git_worktree_root:ws_abc:"));
        assert!(id.contains("/tmp/project/worktree-x"));
    }

    #[test]
    fn execution_root_id_git_worktree_root_is_stable_for_same_path() {
        let id1 = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::GitWorktreeRoot,
            Path::new("/tmp/project/worktree-x"),
        )
        .unwrap();
        let id2 = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::GitWorktreeRoot,
            Path::new("/tmp/project/worktree-x"),
        )
        .unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn execution_root_id_git_worktree_root_differs_for_different_paths() {
        let id1 = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::GitWorktreeRoot,
            Path::new("/tmp/project/wt-a"),
        )
        .unwrap();
        let id2 = build_execution_root_id(
            "ws_abc",
            WorkspaceProjectionKind::GitWorktreeRoot,
            Path::new("/tmp/project/wt-b"),
        )
        .unwrap();
        assert_ne!(id1, id2);
    }

    // ── canonicalize_agent_home_bindings ──

    #[test]
    fn canonicalize_migrates_legacy_agent_home_id() {
        let data_dir = PathBuf::from("/tmp/agent-home");
        let mut state = AgentState::new("my-agent");
        state.active_workspace_entry = Some(ActiveWorkspaceEntry {
            workspace_id: AGENT_HOME_WORKSPACE_ID.to_string(),
            workspace_anchor: data_dir.clone(),
            execution_root_id: "canonical_root:agent_home".to_string(),
            execution_root: data_dir.clone(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: WorkspaceAccessMode::SharedRead,
            cwd: data_dir.clone(),
            occupancy_id: None,
            projection_metadata: None,
        });
        state.attached_workspaces = vec![AGENT_HOME_WORKSPACE_ID.to_string()];

        let changed = canonicalize_agent_home_bindings(&mut state, &data_dir, "my-agent").unwrap();

        assert!(changed, "should report change when migrating legacy ID");
        let canonical = agent_home_workspace_id("my-agent");
        assert_eq!(
            state.active_workspace_entry.as_ref().unwrap().workspace_id,
            canonical
        );
        assert!(
            state.attached_workspaces.contains(&canonical),
            "attached_workspaces should contain canonical ID"
        );
        assert!(
            !state
                .attached_workspaces
                .contains(&AGENT_HOME_WORKSPACE_ID.to_string()),
            "legacy ID should be removed from attached_workspaces"
        );
    }

    #[test]
    fn canonicalize_is_noop_when_already_canonical() {
        let data_dir = PathBuf::from("/tmp/agent-home");
        let canonical = agent_home_workspace_id("my-agent");
        let mut state = AgentState::new("my-agent");
        state.attached_workspaces = vec![canonical.clone()];

        let changed = canonicalize_agent_home_bindings(&mut state, &data_dir, "my-agent").unwrap();

        assert!(!changed, "should be no-op when already using canonical ID");
    }

    #[test]
    fn canonicalize_repairs_missing_own_agent_home() {
        let data_dir = PathBuf::from("/tmp/agent-home");
        let mut state = AgentState::new("my-agent");
        state.attached_workspaces = vec!["ws_other".to_string()];

        let changed = canonicalize_agent_home_bindings(&mut state, &data_dir, "my-agent").unwrap();

        assert!(changed, "should repair missing own agent home");
        assert_eq!(
            state.attached_workspaces,
            vec![agent_home_workspace_id("my-agent"), "ws_other".to_string()]
        );
    }

    #[test]
    fn canonicalize_filters_foreign_agent_home_bindings() {
        let data_dir = PathBuf::from("/tmp/agent-home");
        let mut state = AgentState::new("my-agent");
        state.attached_workspaces = vec![
            "agent_home:other-agent".to_string(),
            "ws_other".to_string(),
            agent_home_workspace_id("my-agent"),
        ];

        let changed = canonicalize_agent_home_bindings(&mut state, &data_dir, "my-agent").unwrap();

        assert!(changed, "should filter foreign agent home");
        assert_eq!(
            state.attached_workspaces,
            vec![agent_home_workspace_id("my-agent"), "ws_other".to_string()]
        );
    }
}
