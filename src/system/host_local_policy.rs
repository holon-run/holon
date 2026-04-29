use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::types::{
    execution_backend_label, execution_guarantee_label, workspace_access_mode_label,
    workspace_projection_label, ExecutionBackendKind, ExecutionProfile, ExecutionSnapshot,
    WorkspaceAccessMode, WorkspaceProjectionKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLocalBoundary {
    pub backend: ExecutionBackendKind,
    pub projection_kind: Option<WorkspaceProjectionKind>,
    pub access_mode: Option<WorkspaceAccessMode>,
    pub execution_root_id: Option<String>,
    pub process_execution_exposed: bool,
    pub allow_background_tasks: bool,
    pub supports_managed_worktrees: bool,
}

impl HostLocalBoundary {
    pub fn from_snapshot(execution: &ExecutionSnapshot) -> Self {
        Self::from_parts(
            &execution.profile,
            execution.projection_kind,
            execution.access_mode,
            execution.execution_root_id.clone(),
        )
    }

    pub fn from_parts(
        profile: &ExecutionProfile,
        projection_kind: Option<WorkspaceProjectionKind>,
        access_mode: Option<WorkspaceAccessMode>,
        execution_root_id: Option<String>,
    ) -> Self {
        Self {
            backend: profile.backend,
            projection_kind,
            access_mode,
            execution_root_id,
            process_execution_exposed: profile.process_execution_exposed,
            allow_background_tasks: profile.allow_background_tasks,
            supports_managed_worktrees: profile.supports_managed_worktrees,
        }
    }

    pub fn audit_metadata(&self) -> Value {
        json!({
            "backend": execution_backend_label(self.backend),
            "projection_kind": self.projection_kind.map(super::types::workspace_projection_kind_label),
            "access_mode": self.access_mode.map(super::types::workspace_access_mode_kind_label),
            "execution_root_id": self.execution_root_id,
            "process_execution_exposed": self.process_execution_exposed,
            "allow_background_tasks": self.allow_background_tasks,
            "supports_managed_worktrees": self.supports_managed_worktrees,
        })
    }
}

pub fn ensure_process_execution_allowed(boundary: &HostLocalBoundary, surface: &str) -> Result<()> {
    if boundary.process_execution_exposed {
        return Ok(());
    }
    Err(anyhow!(
        "{} is not available because process execution is disabled by the current host_local execution profile",
        surface
    ))
}

pub fn ensure_background_task_allowed(boundary: &HostLocalBoundary, surface: &str) -> Result<()> {
    if boundary.allow_background_tasks {
        return Ok(());
    }
    Err(anyhow!(
        "{} is not available because background tasks are disabled by the current host_local execution profile",
        surface
    ))
}

pub fn ensure_workspace_projection_allowed(
    boundary: &HostLocalBoundary,
    projection_kind: WorkspaceProjectionKind,
    surface: &str,
) -> Result<()> {
    if projection_kind != WorkspaceProjectionKind::GitWorktreeRoot {
        return Ok(());
    }
    if boundary.supports_managed_worktrees {
        return Ok(());
    }
    Err(anyhow!(
        "{} is not available because git_worktree_root is disabled by the current host_local execution profile",
        surface
    ))
}

pub fn execution_policy_summary_lines(execution: &ExecutionSnapshot) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Backend: {}",
            execution_backend_label(execution.policy.backend)
        ),
        format!(
            "Process execution exposed: {}",
            execution.policy.process_execution_exposed
        ),
        format!(
            "Background tasks supported: {}",
            execution.profile.allow_background_tasks
        ),
        format!(
            "Managed worktrees supported: {}",
            execution.profile.supports_managed_worktrees
        ),
        format!(
            "Projection kind: {}",
            workspace_projection_label(execution.projection_kind)
        ),
        format!(
            "Access mode: {}",
            workspace_access_mode_label(execution.access_mode)
        ),
    ];

    // Show all attached workspaces
    if execution.attached_workspaces.is_empty() {
        lines.push(format!(
            "Workspace id: {}",
            execution.workspace_id.as_deref().unwrap_or("none")
        ));
        lines.push(format!(
            "Workspace anchor: {}",
            execution.workspace_anchor.display()
        ));
    } else {
        for (ws_id, ws_path) in &execution.attached_workspaces {
            lines.push(format!("Workspace: {} @ {}", ws_id, ws_path.display()));
        }
    }

    lines.extend(vec![
        format!("Execution root: {}", execution.execution_root.display()),
        format!("Cwd: {}", execution.cwd.display()),
        format!(
            "Worktree root: {}",
            execution
                .worktree_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        "Resource authority:".into(),
        format!(
            "  - message_ingress: {}",
            execution_guarantee_label(execution.policy.resource_authority.message_ingress)
        ),
        format!(
            "  - agent_state: {}",
            execution_guarantee_label(execution.policy.resource_authority.agent_state)
        ),
        format!(
            "  - control_plane: {}",
            execution_guarantee_label(execution.policy.resource_authority.control_plane)
        ),
        format!(
            "  - workspace_projection: {}",
            execution_guarantee_label(execution.policy.resource_authority.workspace_projection)
        ),
        format!(
            "  - process_execution: {}",
            execution_guarantee_label(execution.policy.resource_authority.process_execution)
        ),
        "Process execution guarantees:".into(),
        format!(
            "  - cwd_rooting: {}",
            execution_guarantee_label(execution.policy.process_execution.cwd_rooting)
        ),
        format!(
            "  - projection_rooting: {}",
            execution_guarantee_label(execution.policy.process_execution.projection_rooting)
        ),
        format!(
            "  - path_confinement: {}",
            execution_guarantee_label(execution.policy.process_execution.path_confinement)
        ),
        format!(
            "  - write_confinement: {}",
            execution_guarantee_label(execution.policy.process_execution.write_confinement)
        ),
        format!(
            "  - network_confinement: {}",
            execution_guarantee_label(execution.policy.process_execution.network_confinement)
        ),
        format!(
            "  - secret_isolation: {}",
            execution_guarantee_label(execution.policy.process_execution.secret_isolation)
        ),
        format!(
            "  - child_process_containment: {}",
            execution_guarantee_label(execution.policy.process_execution.child_process_containment)
        ),
    ]);

    lines
}
