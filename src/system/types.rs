use clap::ValueEnum;
use std::{ffi::OsString, fmt, path::PathBuf, process::ExitStatus, time::Duration};

use serde::{Deserialize, Deserializer, Serialize};

use super::workspace::WorkspaceView;

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
                    cwd_rooting: ExecutionGuaranteeLevel::HardEnforced,
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
pub enum ExecutionScopeKind {
    AgentTurn,
    CommandTask,
    SubagentTask,
    WorktreeSubagentTask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveExecution {
    pub profile: ExecutionProfile,
    pub workspace: WorkspaceView,
    pub scope: ExecutionScopeKind,
    pub attached_workspaces: Vec<(String, PathBuf)>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProjectionKind {
    CanonicalRoot,
    GitWorktreeRoot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
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
        })
    }
}

impl EffectiveExecution {
    pub fn snapshot(&self) -> ExecutionSnapshot {
        let projection_kind = if self.workspace.worktree_root().is_some() {
            Some(WorkspaceProjectionKind::GitWorktreeRoot)
        } else {
            Some(WorkspaceProjectionKind::CanonicalRoot)
        };
        ExecutionSnapshot {
            profile: self.profile.clone(),
            policy: self.profile.policy_snapshot(),
            attached_workspaces: self.attached_workspaces.clone(),
            workspace_id: self.workspace.workspace_id().map(ToString::to_string),
            workspace_anchor: self.workspace.workspace_anchor().to_path_buf(),
            execution_root: self.workspace.execution_root().to_path_buf(),
            cwd: self.workspace.cwd().to_path_buf(),
            execution_root_id: self.workspace.execution_root_id().map(ToString::to_string),
            projection_kind,
            access_mode: self.workspace.access_mode(),
            worktree_root: self
                .workspace
                .worktree_root()
                .map(|path| path.to_path_buf()),
        }
    }
}
#[derive(Debug, Clone)]
pub enum ProgramInvocation {
    Argv {
        program: String,
        args: Vec<OsString>,
    },
    Shell {
        command: String,
        shell: Option<String>,
        login: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPurpose {
    ToolExec,
    CommandTask,
    InternalGit,
    WorktreeSetup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioSpec {
    Null,
    Inherit,
    Piped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureSpec {
    pub stdout: bool,
    pub stderr: bool,
}

impl CaptureSpec {
    pub const NONE: Self = Self {
        stdout: false,
        stderr: false,
    };

    pub const BOTH: Self = Self {
        stdout: true,
        stderr: true,
    };
}

#[derive(Debug, Clone)]
pub struct ProcessRequest {
    pub program: ProgramInvocation,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: StdioSpec,
    pub tty: bool,
    pub capture: CaptureSpec,
    pub timeout: Option<Duration>,
    pub purpose: ProcessPurpose,
}

#[derive(Debug)]
pub struct ProcessResult {
    pub exit_status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct FileRead {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone)]
pub enum FileContent {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    pub create_parents: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EditOptions {
    pub replace_all: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RemoveOptions {
    pub recursive: bool,
}

#[derive(Debug, Clone)]
pub struct FileStat {
    pub path: PathBuf,
    pub exists: bool,
    pub is_file: bool,
    pub is_dir: bool,
    pub len: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcessExitStatus {
    code: Option<i32>,
    signal: Option<String>,
}

impl RunningProcessExitStatus {
    pub fn new(code: Option<i32>, signal: Option<String>) -> Self {
        Self { code, signal }
    }

    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == Some(0)
    }

    pub fn code(&self) -> Option<i32> {
        self.code
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }
}

impl fmt::Display for RunningProcessExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.code, &self.signal) {
            (_, Some(signal)) => write!(f, "terminated by {signal}"),
            (Some(code), None) => write!(f, "exited with code {code}"),
            (None, None) => write!(f, "exited"),
        }
    }
}

impl From<ExitStatus> for RunningProcessExitStatus {
    fn from(status: ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            if let Some(signal) = status.signal() {
                let signal_name = unsafe { libc::strsignal(signal) };
                let signal = if signal_name.is_null() {
                    format!("signal {signal}")
                } else {
                    unsafe { std::ffi::CStr::from_ptr(signal_name) }
                        .to_string_lossy()
                        .to_string()
                };
                return Self::new(None, Some(signal));
            }
        }

        Self::new(status.code(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_backend_deserializes_legacy_local_value() {
        let backend: ExecutionBackendKind = serde_json::from_str("\"local\"").unwrap();
        assert_eq!(backend, ExecutionBackendKind::HostLocal);
    }

    #[test]
    fn host_local_policy_snapshot_reports_capability_boundary() {
        let profile = ExecutionProfile::default();
        let snapshot = profile.policy_snapshot();

        assert_eq!(snapshot.backend, ExecutionBackendKind::HostLocal);
        assert!(snapshot.process_execution_exposed);
        assert!(snapshot.managed_worktree_supported);
        assert_eq!(
            snapshot.resource_authority.workspace_projection,
            ExecutionGuaranteeLevel::HardEnforced
        );
        assert_eq!(
            snapshot.resource_authority.process_execution,
            ExecutionGuaranteeLevel::RuntimeShaped
        );
        assert_eq!(
            snapshot.process_execution.cwd_rooting,
            ExecutionGuaranteeLevel::HardEnforced
        );
        assert_eq!(
            snapshot.process_execution.path_confinement,
            ExecutionGuaranteeLevel::NotEnforced
        );
        assert_eq!(
            snapshot.process_execution.network_confinement,
            ExecutionGuaranteeLevel::NotEnforced
        );
    }

    #[test]
    fn execution_snapshot_derives_missing_policy_from_profile() {
        let snapshot: ExecutionSnapshot = serde_json::from_value(serde_json::json!({
            "profile": {
                "backend": "host_local",
                "process_execution_exposed": false,
                "allow_background_tasks": true,
                "supports_managed_worktrees": false
            },
            "workspace_anchor": "/workspace",
            "execution_root": "/workspace",
            "cwd": "/workspace"
        }))
        .unwrap();

        assert!(!snapshot.policy.process_execution_exposed);
        assert!(!snapshot.policy.managed_worktree_supported);
        assert_eq!(snapshot.policy.backend, ExecutionBackendKind::HostLocal);
    }
}
