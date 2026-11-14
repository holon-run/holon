use super::agent::AgentLifecycleDisposition;
use super::message::AuthorityClass;
use super::model::AgentModelResolution;
use super::state::{AgentTokenUsageSummary, FailureArtifact};
use super::tool::ToolArtifactRef;
use crate::domain::work_item::WorkItemDelegationRecord;
use crate::runtime::agent_observation::ChildAgentObservabilitySnapshot;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskWaitPolicy {
    Background,
}

pub const CHILD_AGENT_TASK_KIND: &str = "child_agent_task";
pub const ACTOR_INVOCATION_TASK_KIND: &str = "actor_invocation";
pub const AGENT_MESSAGE_WAIT_TASK_KIND: &str = "agent_message_wait";
pub const LEGACY_SUBAGENT_TASK_KIND: &str = "subagent_task";
pub const LEGACY_WORKTREE_SUBAGENT_TASK_KIND: &str = "worktree_subagent_task";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    AgentMessageWait,
    ActorInvocation,
    CommandTask,
    ChildAgentTask,
    SleepJob,
    SubagentTask,
    WorktreeSubagentTask,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentMessageWait => AGENT_MESSAGE_WAIT_TASK_KIND,
            Self::ActorInvocation => ACTOR_INVOCATION_TASK_KIND,
            Self::CommandTask => "command_task",
            Self::ChildAgentTask => CHILD_AGENT_TASK_KIND,
            Self::SleepJob => "sleep_job",
            Self::SubagentTask => LEGACY_SUBAGENT_TASK_KIND,
            Self::WorktreeSubagentTask => LEGACY_WORKTREE_SUBAGENT_TASK_KIND,
        }
    }

    pub fn is_child_agent(self) -> bool {
        matches!(
            self,
            Self::ChildAgentTask | Self::SubagentTask | Self::WorktreeSubagentTask
        )
    }

    pub fn is_legacy_child_agent_compat(self) -> bool {
        matches!(self, Self::SubagentTask | Self::WorktreeSubagentTask)
    }
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentWorkspaceMode {
    Inherit,
    Worktree,
}

impl Default for ChildAgentWorkspaceMode {
    fn default() -> Self {
        Self::Inherit
    }
}

impl ChildAgentWorkspaceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Worktree => "worktree",
        }
    }

    pub fn is_worktree(self) -> bool {
        self == Self::Worktree
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "inherit" => Some(Self::Inherit),
            "worktree" => Some(Self::Worktree),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TaskHandle {
    pub task_id: String,
    pub task_kind: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_output: Option<String>,
}

impl TaskHandle {
    pub fn new(
        task_id: impl Into<String>,
        task_kind: impl Into<String>,
        status: TaskStatus,
        initial_output: Option<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            task_kind: task_kind.into(),
            status,
            initial_output,
        }
    }

    pub fn from_task_record(task: &TaskRecord, initial_output: Option<String>) -> Self {
        Self::new(
            task.id.clone(),
            task.kind.as_str(),
            task.status.clone(),
            initial_output,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub recovery: Option<TaskRecoverySpec>,
}

impl TaskRecord {
    /// Resolve only persisted task evidence, never the current identity for an id.
    pub fn target(&self) -> Option<TaskTargetRef> {
        if let Some(value) = task_detail_value(&self.detail, "target") {
            return serde_json::from_value(value.clone()).ok();
        }
        let (kind, agent_id) = if self.kind == TaskKind::AgentMessageWait {
            (
                TaskTargetKind::AgentMessageWait,
                task_detail_string(&self.detail, "target_agent_id")?,
            )
        } else if self.is_child_agent_task() {
            (
                TaskTargetKind::ChildAgent,
                task_detail_string(&self.detail, "child_agent_id")?,
            )
        } else {
            return None;
        };
        Some(TaskTargetRef {
            kind,
            agent_id,
            incarnation: None,
        })
    }

    pub fn rejoin_fence(
        &self,
    ) -> std::result::Result<crate::domain::execution_protocol::RejoinFence, String> {
        let detail = self
            .detail
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "task rejoin contract is missing task detail".to_string())?;
        let obligation_id = detail
            .get("rejoin_obligation_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "task rejoin contract is missing obligation identity".to_string())?;
        if obligation_id != self.id {
            return Err("task rejoin obligation identity does not match task identity".into());
        }
        let generation = detail
            .get("rejoin_generation")
            .and_then(serde_json::Value::as_u64)
            .filter(|generation| *generation > 0)
            .ok_or_else(|| "task rejoin contract is missing generation".to_string())?;
        use crate::domain::execution_protocol::TaskRejoinSource;
        let source = match (
            detail.get("parent_turn_id"),
            detail.get("command_creation_id"),
        ) {
            (Some(parent), None) => TaskRejoinSource::ParentTurn {
                parent_turn_id: parent
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        "task rejoin contract has invalid parent turn identity".to_string()
                    })?
                    .to_string(),
            },
            (None, Some(creation)) if self.kind == TaskKind::CommandTask => {
                let id = creation
                    .as_str()
                    .filter(|id| *id == self.id)
                    .ok_or_else(|| {
                        "task rejoin creation identity does not match Command".to_string()
                    })?;
                TaskRejoinSource::CommandCreation {
                    command_creation_id: id.to_string(),
                }
            }
            _ => return Err("task rejoin contract requires exactly one creation source".into()),
        };
        Ok(crate::domain::execution_protocol::RejoinFence {
            obligation_id: obligation_id.to_string(),
            generation,
            source,
        })
    }

    pub fn wait_policy(&self) -> TaskWaitPolicy {
        // Active tasks are never scheduler-blocking; terminal results drive re-entry.
        TaskWaitPolicy::Background
    }

    pub fn is_blocking(&self) -> bool {
        false
    }

    pub fn terminal_reentry(&self) -> bool {
        self.detail
            .as_ref()
            .and_then(|detail| detail.get("terminal_reentry"))
            .and_then(Value::as_bool)
            .or_else(|| {
                self.detail
                    .as_ref()
                    .and_then(|detail| detail.get("continue_on_result"))
                    .and_then(Value::as_bool)
            })
            .or_else(|| {
                self.recovery
                    .as_ref()
                    .map(TaskRecoverySpec::terminal_reentry)
            })
            .unwrap_or(false)
    }

    pub fn is_child_agent_task(&self) -> bool {
        self.kind.is_child_agent()
            || (self.kind == TaskKind::ActorInvocation
                && task_detail_bool(&self.detail, "created_new_subagent").unwrap_or(false)
                && task_detail_string(&self.detail, "child_agent_id").is_some())
    }

    pub fn effective_work_item_id(&self) -> Option<&str> {
        self.work_item_id.as_deref().or_else(|| {
            self.detail
                .as_ref()
                .and_then(|detail| detail.get("work_item_id"))
                .and_then(Value::as_str)
        })
    }

    pub fn child_agent_workspace_mode(&self) -> Option<ChildAgentWorkspaceMode> {
        self.detail
            .as_ref()
            .and_then(|detail| detail.get("workspace_mode"))
            .and_then(Value::as_str)
            .and_then(ChildAgentWorkspaceMode::from_label)
            .or_else(|| {
                self.recovery
                    .as_ref()
                    .and_then(TaskRecoverySpec::child_agent_workspace_mode)
            })
            .or_else(|| match self.kind {
                TaskKind::SubagentTask => Some(ChildAgentWorkspaceMode::Inherit),
                TaskKind::WorktreeSubagentTask => Some(ChildAgentWorkspaceMode::Worktree),
                _ => None,
            })
    }

    pub fn is_worktree_child_agent_task(&self) -> bool {
        self.is_child_agent_task()
            && self
                .child_agent_workspace_mode()
                .is_some_and(ChildAgentWorkspaceMode::is_worktree)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskTargetKind {
    ChildAgent,
    AgentMessageWait,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TaskTargetRef {
    pub kind: TaskTargetKind,
    pub agent_id: String,
    /// Absent for legacy evidence; must not be filled from a current identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskListEntry {
    pub id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub summary: Option<String>,
    #[schemars(schema_with = "datetime_schema")]
    pub updated_at: DateTime<Utc>,
    pub wait_policy: TaskWaitPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandTaskStatusSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTargetRef>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CommandTaskOutputPolicy {
    pub retention_bytes: u64,
    pub execution_quota_bytes: u64,
    pub min_free_disk_bytes: u64,
    pub min_free_disk_percent: u8,
}

impl Default for CommandTaskOutputPolicy {
    fn default() -> Self {
        Self {
            retention_bytes: 8 * 1024 * 1024,
            execution_quota_bytes: 64 * 1024 * 1024,
            min_free_disk_bytes: 512 * 1024 * 1024,
            min_free_disk_percent: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandTaskOutputFailureCode {
    OutputLimitExceeded,
    LowDiskSpace,
    OutputPersistenceFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CommandTaskOutputCaptureSnapshot {
    pub emitted_bytes: u64,
    pub decoded_bytes: u64,
    pub retained_bytes: u64,
    pub dropped_bytes: u64,
    pub retention_limit_bytes: u64,
    pub execution_quota_bytes: u64,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<CommandTaskOutputFailureCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_disk_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_free_disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CommandTaskStatusSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reentry: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_from_exec_command: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepts_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_capture: Option<CommandTaskOutputCaptureSnapshot>,
}

impl CommandTaskStatusSnapshot {
    pub(crate) fn from_task_record(task: &TaskRecord) -> Option<Self> {
        Self::from_task_record_with_runtime_fields(task, true)
    }

    pub(crate) fn identity_from_task_record(task: &TaskRecord) -> Option<Self> {
        Self::from_task_record_with_runtime_fields(task, false)
    }

    fn from_task_record_with_runtime_fields(
        task: &TaskRecord,
        include_runtime_fields: bool,
    ) -> Option<Self> {
        if task.kind != TaskKind::CommandTask {
            return None;
        }

        let recovery_command = match task.recovery.as_ref() {
            Some(TaskRecoverySpec::CommandTask {
                spec,
                promoted_from_exec_command,
                ..
            }) => Some((spec, *promoted_from_exec_command)),
            _ => None,
        };

        let cmd = task_detail_string(&task.detail, "cmd")
            .or_else(|| recovery_command.map(|(spec, _)| spec.cmd.clone()));
        let cmd_digest = task_detail_string(&task.detail, "cmd_digest").or_else(|| {
            cmd.as_ref()
                .map(|cmd| crate::tool::helpers::command_digest(cmd))
        });
        let workdir = task_detail_string(&task.detail, "workdir").or_else(|| {
            recovery_command.and_then(|(spec, _)| spec.workdir.as_ref().map(ToString::to_string))
        });
        let shell = task_detail_string(&task.detail, "shell").or_else(|| {
            recovery_command.and_then(|(spec, _)| spec.shell.as_ref().map(ToString::to_string))
        });
        let login = task_detail_bool(&task.detail, "login")
            .or_else(|| recovery_command.map(|(spec, _)| spec.login));
        let tty = task_detail_bool(&task.detail, "tty")
            .or_else(|| recovery_command.map(|(spec, _)| spec.tty));
        let promoted_from_exec_command =
            task_detail_bool(&task.detail, "promoted_from_exec_command")
                .or_else(|| recovery_command.map(|(_, promoted)| promoted));

        Some(Self {
            cmd,
            cmd_digest,
            workdir,
            shell,
            login,
            tty,
            output_path: include_runtime_fields
                .then(|| task_detail_string(&task.detail, "output_path"))
                .flatten(),
            result_summary: include_runtime_fields
                .then(|| task_detail_string(&task.detail, "output_summary"))
                .flatten(),
            exit_status: include_runtime_fields
                .then(|| task_detail_i32(&task.detail, "exit_status"))
                .flatten(),
            terminal_reentry: include_runtime_fields
                .then(|| task.terminal_reentry().then_some(true))
                .flatten(),
            promoted_from_exec_command,
            accepts_input: include_runtime_fields
                .then(|| task_detail_bool(&task.detail, "accepts_input"))
                .flatten(),
            input_target: include_runtime_fields
                .then(|| task_detail_string(&task.detail, "input_target"))
                .flatten(),
            output_capture: include_runtime_fields
                .then(|| {
                    task.detail
                        .as_ref()
                        .and_then(|detail| detail.get("output_capture"))
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                })
                .flatten(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChildSupervisionProjection {
    pub parent_agent_id: String,
    pub child_agent_id: String,
    pub supervision_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<ChildAgentWorkspaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ChildSupervisionWorktreeProjection>,
    pub cleanup_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<String>,
    pub followup_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct ChildSupervisionWorktreeProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cleaned_up: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_for_review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_cleanup_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_branch: Option<String>,
}

impl ChildSupervisionWorktreeProjection {
    fn from_value(value: &Value) -> Self {
        Self {
            worktree_path: value_string(value, "worktree_path"),
            worktree_branch: value_string(value, "worktree_branch"),
            projection_kind: value_string(value, "projection_kind"),
            original_cwd: value_string(value, "original_cwd"),
            original_branch: value_string(value, "original_branch"),
            changed_files: value
                .get("changed_files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect(),
            cleanup_status: value_string(value, "cleanup_status"),
            cleanup_reason: value_string(value, "cleanup_reason"),
            cleanup_error: value_string(value, "cleanup_error"),
            auto_cleaned_up: value.get("auto_cleaned_up").and_then(Value::as_bool),
            retained_for_review: value.get("retained_for_review").and_then(Value::as_bool),
            branch_cleanup_status: value_string(value, "branch_cleanup_status"),
            branch_cleanup_error: value_string(value, "branch_cleanup_error"),
            actual_branch: value_string(value, "actual_branch"),
        }
    }
}

impl ChildSupervisionProjection {
    pub fn from_task_record(task: &TaskRecord) -> Option<Self> {
        if !task.is_child_agent_task() {
            return None;
        }
        let child_agent_id = task_detail_string(&task.detail, "child_agent_id")?;
        let worktree = task_detail_value(&task.detail, "worktree").cloned();
        let cleanup_status = worktree
            .as_ref()
            .and_then(|worktree| worktree.get("cleanup_status"))
            .and_then(Value::as_str)
            .map(ToString::to_string);

        Some(Self {
            parent_agent_id: task.agent_id.clone(),
            child_agent_id,
            supervision_task_id: task.id.clone(),
            parent_work_item_id: None,
            child_work_item_id: None,
            delegation_id: None,
            workspace_mode: task.child_agent_workspace_mode(),
            worktree: worktree
                .as_ref()
                .map(ChildSupervisionWorktreeProjection::from_value),
            cleanup_owner: "supervision_task".into(),
            cleanup_status,
            followup_target: "parent_supervisor".into(),
        })
    }

    pub fn with_work_item_delegation(mut self, delegation: &WorkItemDelegationRecord) -> Self {
        self.parent_work_item_id = Some(delegation.parent_work_item_id.clone());
        self.child_work_item_id = Some(delegation.child_work_item_id.clone());
        self.delegation_id = Some(delegation.delegation_id.clone());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskStatusSnapshot {
    pub task_id: String,
    pub kind: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[schemars(schema_with = "datetime_schema")]
    pub created_at: DateTime<Utc>,
    #[schemars(schema_with = "datetime_schema")]
    pub updated_at: DateTime<Utc>,
    pub wait_policy: TaskWaitPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandTaskStatusSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTargetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_observability: Option<ChildAgentObservabilitySnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_supervision: Option<ChildSupervisionProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<AgentTokenUsageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<AgentModelResolution>,
}

impl TaskStatusSnapshot {
    pub fn from_task_record(task: &TaskRecord) -> Self {
        let command = CommandTaskStatusSnapshot::from_task_record(task);
        let child_agent_id = task_detail_string(&task.detail, "child_agent_id");
        let child_observability = task_detail_value(&task.detail, "child_observability")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let child_supervision = ChildSupervisionProjection::from_task_record(task);
        let token_usage = task_detail_value(&task.detail, "token_usage")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let model_resolution = task_detail_value(&task.detail, "model_resolution")
            .and_then(|value| serde_json::from_value(value.clone()).ok());

        Self {
            task_id: task.id.clone(),
            kind: task.kind.as_str().to_string(),
            status: task.status.clone(),
            summary: task.summary.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
            wait_policy: task.wait_policy(),
            parent_message_id: task.parent_message_id.clone(),
            command,
            child_agent_id,
            target: task.target(),
            child_observability,
            child_supervision,
            token_usage,
            model_resolution,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutputRetrievalStatus {
    Success,
    Timeout,
    NotReady,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskOutputSnapshot {
    pub task_id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub summary: Option<String>,
    pub output_preview: String,
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ToolArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_artifact: Option<usize>,
    pub result_summary: Option<String>,
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_capture: Option<CommandTaskOutputCaptureSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_artifact: Option<FailureArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_supervision: Option<ChildSupervisionProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<AgentTokenUsageSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskOutputResult {
    pub retrieval_status: TaskOutputRetrievalStatus,
    pub task: TaskOutputSnapshot,
}

fn datetime_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "format": "date-time"
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRecoverySpec {
    AgentMessageWait {
        summary: String,
        message: String,
        target_agent_id: String,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_delivery_rowid: Option<i64>,
        /// Missing on historical tasks: retain unlimited waiting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_deadline_at: Option<DateTime<Utc>>,
        /// Required for runtime-derived replies, never inferred on recovery.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester_incarnation: Option<u64>,
    },
    AgentInvocation {
        summary: String,
        prompt: String,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_agent_id: Option<String>,
        created_new_subagent: bool,
        workspace_mode: ChildAgentWorkspaceMode,
        #[serde(default)]
        lifecycle_disposition: AgentLifecycleDisposition,
    },
    ChildAgentTask {
        summary: String,
        prompt: String,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
        workspace_mode: ChildAgentWorkspaceMode,
        #[serde(default = "delete_agent_on_terminal")]
        lifecycle_disposition: AgentLifecycleDisposition,
    },
    SubagentTask {
        summary: String,
        prompt: String,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
    },
    WorktreeSubagentTask {
        summary: String,
        prompt: String,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
    },
    CommandTask {
        summary: String,
        spec: CommandTaskSpec,
        #[serde(alias = "trust")]
        authority_class: AuthorityClass,
        promoted_from_exec_command: bool,
    },
}

fn delete_agent_on_terminal() -> AgentLifecycleDisposition {
    AgentLifecycleDisposition::DeleteOnTerminal
}

fn task_detail_string(detail: &Option<Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn task_detail_bool(detail: &Option<Value>, key: &str) -> Option<bool> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_bool)
}

fn task_detail_i32(detail: &Option<Value>, key: &str) -> Option<i32> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn task_detail_value<'a>(detail: &'a Option<Value>, key: &str) -> Option<&'a Value> {
    detail.as_ref().and_then(|detail| detail.get(key))
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

impl TaskRecoverySpec {
    pub fn is_legacy_child_agent_compat(&self) -> bool {
        matches!(
            self,
            TaskRecoverySpec::SubagentTask { .. } | TaskRecoverySpec::WorktreeSubagentTask { .. }
        )
    }

    pub fn child_agent_workspace_mode(&self) -> Option<ChildAgentWorkspaceMode> {
        match self {
            TaskRecoverySpec::AgentMessageWait { .. } => None,
            TaskRecoverySpec::AgentInvocation { workspace_mode, .. } => Some(*workspace_mode),
            TaskRecoverySpec::ChildAgentTask { workspace_mode, .. } => Some(*workspace_mode),
            TaskRecoverySpec::SubagentTask { .. } => Some(ChildAgentWorkspaceMode::Inherit),
            TaskRecoverySpec::WorktreeSubagentTask { .. } => {
                Some(ChildAgentWorkspaceMode::Worktree)
            }
            TaskRecoverySpec::CommandTask { .. } => None,
        }
    }

    pub fn wait_policy(&self) -> TaskWaitPolicy {
        match self {
            TaskRecoverySpec::AgentMessageWait { .. } => TaskWaitPolicy::Background,
            TaskRecoverySpec::AgentInvocation { .. } => TaskWaitPolicy::Background,
            TaskRecoverySpec::ChildAgentTask { .. } => TaskWaitPolicy::Background,
            TaskRecoverySpec::SubagentTask { .. } => TaskWaitPolicy::Background,
            TaskRecoverySpec::WorktreeSubagentTask { .. } => TaskWaitPolicy::Background,
            TaskRecoverySpec::CommandTask { .. } => TaskWaitPolicy::Background,
        }
    }

    pub fn terminal_reentry(&self) -> bool {
        match self {
            TaskRecoverySpec::CommandTask { spec, .. } => spec.terminal_reentry,
            TaskRecoverySpec::AgentMessageWait { .. }
            | TaskRecoverySpec::AgentInvocation { .. }
            | TaskRecoverySpec::ChildAgentTask { .. }
            | TaskRecoverySpec::SubagentTask { .. }
            | TaskRecoverySpec::WorktreeSubagentTask { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CommandTaskSpec {
    pub cmd: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    pub login: bool,
    #[serde(default)]
    pub tty: bool,
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub accepts_input: bool,
    #[serde(default, alias = "continue_on_result")]
    pub terminal_reentry: bool,
}
