use std::{ffi::OsString, fmt, path::PathBuf, process::ExitStatus, time::Duration};

use serde::{Deserialize, Serialize};

use super::workspace::WorkspaceView;

pub use holon_domain::execution_types::*;

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

impl EffectiveExecution {
    pub fn snapshot(&self) -> ExecutionSnapshot {
        let projection_kind = Some(self.workspace.projection_kind());
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
            execution_roots: Vec::new(),
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
    /// Ask the process to exit cleanly, then use a bounded SIGKILL fallback.
    Graceful,
    /// Terminate the process immediately without a grace period.
    Force,
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
            ExecutionGuaranteeLevel::RuntimeShaped
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
