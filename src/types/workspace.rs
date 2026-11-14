use crate::system::{WorkspaceAccessMode, WorkspaceProjectionKind};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    pub workspace_anchor: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkspaceEntry {
    pub fn new(
        workspace_id: impl Into<String>,
        workspace_anchor: PathBuf,
        repo_name: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            workspace_id: workspace_id.into(),
            workspace_alias: None,
            workspace_kind: None,
            owner_agent_id: None,
            workspace_anchor,
            repo_name,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Typed metadata for workspace projections, replacing untyped `serde_json::Value`.
/// Uses `untagged` serde for backward compatibility with previously serialized JSON.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum WorkspaceProjectionMetadata {
    ManagedWorktree(ManagedWorktreeProjectionMetadata),
    ExistingGitWorktree(ExistingGitWorktreeProjectionMetadata),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ManagedWorktreeProjectionMetadata {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ExistingGitWorktreeProjectionMetadata {
    pub worktree_root: PathBuf,
}

/// Unified per-workspace info returned by the workspace state snapshot.
/// Replaces the previous fragmented fields (`attached_workspaces`,
/// `workspace_entries`, `active_workspace_entry`, `worktree_session`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkspaceInfo {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<WorkspaceProjectionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<WorkspaceAccessMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
}

/// Worktree details merged from `projection_metadata` and `worktree_session`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ActiveWorkspaceEntry {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    pub execution_root_id: String,
    pub execution_root: PathBuf,
    pub projection_kind: WorkspaceProjectionKind,
    pub access_mode: WorkspaceAccessMode,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupancy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_metadata: Option<WorkspaceProjectionMetadata>,
}

impl ActiveWorkspaceEntry {
    pub fn without_projection_metadata(mut self) -> Self {
        self.projection_metadata = None;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOccupancyRecord {
    pub occupancy_id: String,
    pub execution_root_id: String,
    pub workspace_id: String,
    pub holder_agent_id: String,
    pub access_mode: WorkspaceAccessMode,
    pub acquired_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
}

/// Durable registry entry mapping an `execution_root_id` to its filesystem
/// path. Independent of any agent's `active_workspace_entry`, so links
/// remain resolvable after the agent switches roots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionRootEntry {
    pub execution_root_id: String,
    pub workspace_id: String,
    pub filesystem_path: PathBuf,
    pub root_kind: WorkspaceProjectionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeArtifactMetadata>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProvenance {
    RuntimeCreated,
    Discovered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeCleanupEvidence {
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub inspected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeArtifactMetadata {
    pub provenance: WorktreeProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_by_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorized_agent_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    #[serde(default)]
    pub detached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cleanup: Option<WorktreeCleanupEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorktreeSession {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}
