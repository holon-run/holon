use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

use crate::storage::AppStorage;
use crate::system::{WorkspaceProjectionKind, WorkspaceView};
use crate::types::{AgentState, WorkspaceEntry};

fn build_execution_root_id(
    workspace_id: &str,
    projection_kind: WorkspaceProjectionKind,
    execution_root: &Path,
) -> Result<String> {
    Ok(match projection_kind {
        WorkspaceProjectionKind::CanonicalRoot => format!("canonical_root:{workspace_id}"),
        WorkspaceProjectionKind::GitWorktreeRoot => format!(
            "git_worktree_root:{workspace_id}:{}",
            crate::system::workspace::normalize_path(execution_root)?.display()
        ),
    })
}

fn agent_home_workspace_entry(data_dir: &Path) -> WorkspaceEntry {
    WorkspaceEntry::new(
        crate::types::AGENT_HOME_WORKSPACE_ID,
        data_dir.to_path_buf(),
        Some("AgentHome".into()),
    )
}

fn initial_workspace_entry(
    storage: &AppStorage,
    initial_workspace: &crate::runtime::InitialWorkspaceBinding,
) -> Result<Option<WorkspaceEntry>> {
    let entry = match initial_workspace {
        crate::runtime::InitialWorkspaceBinding::Entry(entry) => Some(entry.clone()),
        crate::runtime::InitialWorkspaceBinding::Anchor(anchor) => Some(WorkspaceEntry::new(
            format!("ws-{}", Uuid::new_v4().simple()),
            anchor.clone(),
            anchor
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string),
        )),
        crate::runtime::InitialWorkspaceBinding::Detached => Some(agent_home_workspace_entry(storage.data_dir())),
    };

    if let Some(workspace) = entry.as_ref() {
        let known = storage.latest_workspace_entries()?;
        if !known
            .iter()
            .any(|entry| entry.workspace_id == workspace.workspace_id)
        {
            storage.append_workspace_entry(workspace)?;
        }
    }

    Ok(entry)
}

fn initial_active_workspace_entry(
    workspace_id: &str,
    workspace_anchor: &Path,
) -> Result<crate::types::ActiveWorkspaceEntry> {
    Ok(crate::types::ActiveWorkspaceEntry {
        workspace_id: workspace_id.to_string(),
        workspace_anchor: workspace_anchor.to_path_buf(),
        execution_root_id: build_execution_root_id(
            workspace_id,
            WorkspaceProjectionKind::CanonicalRoot,
            workspace_anchor,
        )?,
        execution_root: workspace_anchor.to_path_buf(),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: workspace_anchor.to_path_buf(),
        occupancy_id: None,
        projection_metadata: None,
    })
}

fn seed_initial_workspace_binding(
    state: &mut crate::types::AgentState,
    workspace: &Option<WorkspaceEntry>,
    recovered_from_storage: bool,
) -> Result<()> {
    if state.attached_workspaces.is_empty() {
        if let Some(workspace) = workspace.as_ref() {
            let should_seed = !recovered_from_storage
                || state
                    .active_workspace_entry
                    .as_ref()
                    .is_some_and(|entry| entry.workspace_id == workspace.workspace_id);
            if should_seed {
                state.attached_workspaces.push(workspace.workspace_id.clone());
            }
        }
    }
    Ok(())
}

fn recover_missing_worktree_session(
    storage: &AppStorage,
    state: &mut crate::types::AgentState,
    agent_id: &str,
) -> Result<()> {
    let Some(worktree) = state.worktree_session.as_ref() else {
        return Ok(());
    };
    if worktree.worktree_path.exists() {
        return Ok(());
    }

    storage.append_event(&crate::types::AuditEvent::new(
        "recovery_cleared_missing_worktree_session",
        serde_json::json!({
            "agent_id": agent_id,
            "worktree_path": state.worktree_session.as_ref().map(|w| w.worktree_path.display().to_string()),
            "reason": "worktree_path_does_not_exist"
        }),
    ))?;
    state.worktree_session = None;
    if state
        .active_workspace_entry
        .as_ref()
        .is_some_and(|entry| entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot)
    {
        let agent_home = storage.data_dir();
        let agent_home_id = format!("agent_home-{agent_id}");
        state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: agent_home_id.clone(),
            workspace_anchor: agent_home.to_path_buf(),
            execution_root_id: build_execution_root_id(
                &agent_home_id,
                WorkspaceProjectionKind::CanonicalRoot,
                &agent_home,
            )?,
            execution_root: agent_home.to_path_buf(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
            cwd: agent_home.to_path_buf(),
            occupancy_id: None,
            projection_metadata: None,
        });
    }
    Ok(())
}

fn detached_execution_root(data_dir: &Path) -> PathBuf {
    data_dir.to_path_buf()
}

fn workspace_view_for_root(
    storage: &AppStorage,
    execution_root: PathBuf,
    cwd: PathBuf,
    worktree_root: Option<PathBuf>,
) -> Result<WorkspaceView> {
    let state = storage
        .read_agent()?
        .ok_or_else(|| anyhow!("agent state not found"))?;
    let workspace_id = state.active_workspace_entry.as_ref().map(|entry| entry.workspace_id.clone());
    let workspace_anchor = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.workspace_anchor.clone())
        .unwrap_or_else(|| detached_execution_root(storage.data_dir()));
    let execution_root_id = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.execution_root_id.clone());
    let access_mode = state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.access_mode);

    WorkspaceView::new(
        workspace_id,
        workspace_anchor,
        execution_root,
        cwd,
        execution_root_id,
        access_mode,
        worktree_root,
    )
}

fn workspace_view_from_state(state: &AgentState) -> Result<WorkspaceView> {
    if let Some(entry) = state.active_workspace_entry.as_ref() {
        let worktree_root = (entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot)
            .then(|| entry.execution_root.clone());
        return WorkspaceView::new(
            Some(entry.workspace_id.clone()),
            entry.workspace_anchor.clone(),
            entry.execution_root.clone(),
            entry.cwd.clone(),
            Some(entry.execution_root_id.clone()),
            Some(entry.access_mode),
            worktree_root,
        );
    }

    Err(anyhow!(
        "workspace root unavailable: no active workspace entry and no worktree session"
    ))
}

fn load_attached_workspace_entries_for(
    storage: &AppStorage,
    attached_workspace_ids: &[String],
    workspace: &WorkspaceView,
) -> Result<Vec<(String, PathBuf)>> {
    let known_entries = storage
        .latest_workspace_entries()?
        .into_iter()
        .map(|entry| (entry.workspace_id, entry.workspace_anchor))
        .collect::<HashMap<_, _>>();

    let active_workspace_id = workspace.workspace_id().map(ToString::to_string);
    let mut ordered_ids = Vec::new();

    if let Some(active_id) = active_workspace_id.as_ref() {
        if attached_workspace_ids.is_empty() || attached_workspace_ids.iter().any(|id| id == active_id)
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

fn execution_snapshot_for_view(
    profile: crate::system::ExecutionProfile,
    workspace: &WorkspaceView,
    attached_workspace_ids: &[String],
    storage: &AppStorage,
) -> crate::system::ExecutionSnapshot {
    let attached_workspaces = load_attached_workspace_entries_for(storage, attached_workspace_ids, workspace)
        .unwrap_or_else(|_| {
            workspace
                .workspace_id()
                .map(|id| (id.to_string(), workspace.workspace_anchor().to_path_buf()))
                .into_iter()
                .collect()
        });

    crate::system::ExecutionSnapshot {
        policy: profile.policy_snapshot(),
        profile,
        attached_workspaces,
        workspace_id: workspace.workspace_id().map(ToString::to_string),
        workspace_anchor: workspace.workspace_anchor().to_path_buf(),
        execution_root: workspace.execution_root().to_path_buf(),
        cwd: workspace.cwd().to_path_buf(),
        execution_root_id: workspace.execution_root_id().map(ToString::to_string),
        projection_kind: Some(if workspace.worktree_root().is_some() {
            WorkspaceProjectionKind::GitWorktreeRoot
        } else {
            WorkspaceProjectionKind::CanonicalRoot
        }),
        access_mode: workspace.access_mode(),
        worktree_root: workspace.worktree_root().map(|path| path.to_path_buf()),
    }
}

fn workspace_anchor_for_state_ref(
    state: &AgentState,
) -> Option<&std::path::Path> {
    state
        .active_workspace_entry
        .as_ref()
        .map(|entry| entry.workspace_anchor.as_path())
}

fn execution_root_sync(storage: &AppStorage) -> PathBuf {
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
        .unwrap_or_else(|| detached_execution_root(storage.data_dir()))
}

pub(crate) use {
    agent_home_workspace_entry as agent_home_workspace_entry_fn,
    build_execution_root_id as build_execution_root_id_fn,
    execution_root_sync as execution_root_sync_fn,
    execution_snapshot_for_view as execution_snapshot_for_view_fn,
    initial_active_workspace_entry as initial_active_workspace_entry_fn,
    initial_workspace_entry as initial_workspace_entry_fn,
    load_attached_workspace_entries_for as load_attached_workspace_entries_for_fn,
    recover_missing_worktree_session as recover_missing_worktree_session_fn,
    seed_initial_workspace_binding as seed_initial_workspace_binding_fn,
    workspace_anchor_for_state_ref as workspace_anchor_for_state_ref_fn,
    workspace_view_for_root as workspace_view_for_root_fn,
    workspace_view_from_state as workspace_view_from_state_fn,
    detached_execution_root as detached_execution_root_fn,
};
