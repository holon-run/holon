use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    #[serde(alias = "local")]
    HostLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionProfile {
    pub backend: ExecutionBackendKind,
    pub process_execution_exposed: bool,
    pub allow_background_tasks: bool,
    pub supports_managed_worktrees: bool,
}

impl Default for ExecutionProfile {
    fn default() -> Self {
        Self {
            backend: ExecutionBackendKind::HostLocal,
            process_execution_exposed: true,
            allow_background_tasks: true,
            supports_managed_worktrees: true,
        }
    }
}

impl ExecutionProfile {
    pub fn policy_snapshot(&self) -> ExecutionPolicySnapshot {
        match self.backend {
            ExecutionBackendKind::HostLocal => ExecutionPolicySnapshot {
                backend: self.backend,
                process_execution_exposed: self.process_execution_exposed,
                managed_worktree_supported: self.supports_managed_worktrees,
                resource_authority: ResourceAuthoritySnapshot {
                    message_ingress: ExecutionGuaranteeLevel::HardEnforced,
                    agent_state: ExecutionGuaranteeLevel::HardEnforced,
                    control_plane: ExecutionGuaranteeLevel::HardEnforced,
                    workspace_projection: ExecutionGuaranteeLevel::HardEnforced,
                    process_execution: ExecutionGuaranteeLevel::RuntimeShaped,
                },
                process_execution: ProcessExecutionCapabilitySnapshot {
                    cwd_rooting: ExecutionGuaranteeLevel::RuntimeShaped,
                    projection_rooting: ExecutionGuaranteeLevel::HardEnforced,
                    path_confinement: ExecutionGuaranteeLevel::NotEnforced,
                    write_confinement: ExecutionGuaranteeLevel::NotEnforced,
                    network_confinement: ExecutionGuaranteeLevel::NotEnforced,
                    secret_isolation: ExecutionGuaranteeLevel::NotEnforced,
                    child_process_containment: ExecutionGuaranteeLevel::NotEnforced,
                },
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionGuaranteeLevel {
    HardEnforced,
    RuntimeShaped,
    NotEnforced,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceAuthoritySnapshot {
    pub message_ingress: ExecutionGuaranteeLevel,
    pub agent_state: ExecutionGuaranteeLevel,
    pub control_plane: ExecutionGuaranteeLevel,
    pub workspace_projection: ExecutionGuaranteeLevel,
    pub process_execution: ExecutionGuaranteeLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessExecutionCapabilitySnapshot {
    pub cwd_rooting: ExecutionGuaranteeLevel,
    pub projection_rooting: ExecutionGuaranteeLevel,
    pub path_confinement: ExecutionGuaranteeLevel,
    pub write_confinement: ExecutionGuaranteeLevel,
    pub network_confinement: ExecutionGuaranteeLevel,
    pub secret_isolation: ExecutionGuaranteeLevel,
    pub child_process_containment: ExecutionGuaranteeLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionPolicySnapshot {
    pub backend: ExecutionBackendKind,
    pub process_execution_exposed: bool,
    pub managed_worktree_supported: bool,
    pub resource_authority: ResourceAuthoritySnapshot,
    pub process_execution: ProcessExecutionCapabilitySnapshot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProjectionKind {
    CanonicalRoot,
    GitWorktreeRoot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccessMode {
    SharedRead,
    ExclusiveWrite,
}

pub fn execution_backend_label(value: ExecutionBackendKind) -> &'static str {
    match value {
        ExecutionBackendKind::HostLocal => "host_local",
    }
}

pub fn workspace_projection_kind_label(value: WorkspaceProjectionKind) -> &'static str {
    match value {
        WorkspaceProjectionKind::CanonicalRoot => "canonical_root",
        WorkspaceProjectionKind::GitWorktreeRoot => "git_worktree_root",
    }
}

pub fn workspace_projection_label(value: Option<WorkspaceProjectionKind>) -> &'static str {
    value.map(workspace_projection_kind_label).unwrap_or("none")
}

pub fn workspace_access_mode_kind_label(value: WorkspaceAccessMode) -> &'static str {
    match value {
        WorkspaceAccessMode::SharedRead => "shared_read",
        WorkspaceAccessMode::ExclusiveWrite => "exclusive_write",
    }
}

pub fn workspace_access_mode_label(value: Option<WorkspaceAccessMode>) -> &'static str {
    value
        .map(workspace_access_mode_kind_label)
        .unwrap_or("none")
}

pub fn execution_guarantee_label(value: ExecutionGuaranteeLevel) -> &'static str {
    match value {
        ExecutionGuaranteeLevel::HardEnforced => "hard_enforced",
        ExecutionGuaranteeLevel::RuntimeShaped => "runtime_shaped",
        ExecutionGuaranteeLevel::NotEnforced => "not_enforced",
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    pub profile: ExecutionProfile,
    pub policy: ExecutionPolicySnapshot,
    /// All attached workspaces with their path information
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_workspaces: Vec<(String, PathBuf)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub workspace_anchor: PathBuf,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<WorkspaceProjectionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<WorkspaceAccessMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_roots: Vec<ExecutionRootRef>,
}

/// Lightweight reference to an execution root, used by the provider turn
/// resolver to resolve historical `?root=` parameters in `workspace://` URIs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionRootRef {
    pub execution_root_id: String,
    pub workspace_id: String,
    pub filesystem_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct ExecutionSnapshotSerde {
    pub profile: ExecutionProfile,
    #[serde(default)]
    pub policy: Option<ExecutionPolicySnapshot>,
    #[serde(default)]
    pub attached_workspaces: Vec<(String, PathBuf)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub workspace_anchor: PathBuf,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<WorkspaceProjectionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<WorkspaceAccessMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_roots: Vec<ExecutionRootRef>,
}

impl<'de> Deserialize<'de> for ExecutionSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let snapshot = ExecutionSnapshotSerde::deserialize(deserializer)?;
        let policy = snapshot
            .policy
            .unwrap_or_else(|| snapshot.profile.policy_snapshot());
        Ok(Self {
            profile: snapshot.profile,
            attached_workspaces: snapshot.attached_workspaces,
            policy,
            workspace_id: snapshot.workspace_id,
            workspace_anchor: snapshot.workspace_anchor,
            execution_root: snapshot.execution_root,
            cwd: snapshot.cwd,
            execution_root_id: snapshot.execution_root_id,
            projection_kind: snapshot.projection_kind,
            access_mode: snapshot.access_mode,
            worktree_root: snapshot.worktree_root,
            execution_roots: snapshot.execution_roots,
        })
    }
}
