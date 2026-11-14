//! Shared HTTP wire DTOs for first-party state bootstrap clients.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ExternalTriggerStateSnapshot, TaskKind, TaskRecord, TaskStatus, TaskTargetRef, TimerRecord,
        TodoItem, TurnTerminalRecord, WorkItemPlanStatus, WorkItemReadiness,
        WorkItemSchedulingState, WorkItemState,
    },
    work_item_scheduling::{
        WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingProjection,
        WorkItemSchedulingReasonCode,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(bound(deserialize = "Timer: Deserialize<'de>, WorkItem: Deserialize<'de>"))]
#[schemars(
    rename = "AgentStateSnapshotDto",
    bound = "Timer: JsonSchema + Serialize, WorkItem: JsonSchema + Serialize"
)]
pub struct AgentStateSnapshotDto<Timer = TimerRecord, WorkItem = SlimWorkItemDto> {
    pub agent: SlimAgentDto,
    pub session: StateSessionSnapshotDto,
    pub tasks: Vec<SlimTaskDto>,
    #[serde(default)]
    pub waits: Vec<SlimWaitDto>,
    #[serde(default)]
    pub timers: Vec<Timer>,
    #[serde(default)]
    pub work_items: Vec<WorkItem>,
    /// Total nonterminal WorkItems; `work_items` is a bounded current-first window.
    #[serde(default)]
    pub active_work_item_count: usize,
    /// Total active Timers; `timers` is a bounded window.
    #[serde(default)]
    pub active_timer_count: usize,
    #[serde(default)]
    pub external_triggers: Vec<ExternalTriggerStateSnapshot>,
    #[serde(default)]
    pub workspace: StateWorkspaceSnapshotDto,
}

/// Public waiting metadata; excludes continuation payloads and callback details.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimWaitDto {
    pub id: String,
    pub work_item_id: Option<String>,
    pub kind: crate::types::WaitConditionKind,
    pub status: crate::types::WaitConditionStatus,
    pub created_at: DateTime<Utc>,
    pub task_ids: Vec<String>,
}

impl From<crate::types::WaitConditionRecord> for SlimWaitDto {
    fn from(wait: crate::types::WaitConditionRecord) -> Self {
        Self {
            id: wait.id,
            work_item_id: wait.work_item_id,
            kind: wait.kind,
            status: wait.status,
            created_at: wait.created_at,
            task_ids: wait
                .wake_sources
                .into_iter()
                .filter_map(|source| match source {
                    crate::types::WakeSource::TaskResult { task_id } => Some(task_id),
                    _ => None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct StateSessionSnapshotDto {
    pub current_run_id: Option<String>,
    pub pending_count: usize,
    pub last_turn: Option<TurnTerminalRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct StateWorkspaceSnapshotDto {
    #[serde(default)]
    pub workspaces: Vec<SlimWorkspaceDto>,
}

impl StateWorkspaceSnapshotDto {
    pub fn active_workspace(&self) -> Option<&SlimWorkspaceDto> {
        self.workspaces.iter().find(|workspace| workspace.is_active)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimWorkspaceDto {
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
    pub worktree: Option<SlimWorktreeDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimWorktreeDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_cwd: Option<String>,
}

pub use holon_domain::{
    SlimActiveWorkspaceDto, SlimAgentDto, SlimAgentModelDto, SlimAgentRuntimeDto,
    SlimChildAgentDto, SlimChildObservabilityDto, SlimClosureDto,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SlimTaskDto {
    pub id: String,
    pub agent_id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTargetRef>,
}

/// Keep the original task-list fields while sharing the state/detail target contract.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TaskRecordDto {
    #[serde(flatten)]
    pub task: TaskRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTargetRef>,
}

impl From<TaskRecord> for TaskRecordDto {
    fn from(task: TaskRecord) -> Self {
        let target = task.target();
        Self { task, target }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(bound(deserialize = "Text: Deserialize<'de>, Todo: Deserialize<'de>"))]
#[schemars(rename = "SlimWorkItemDto")]
pub struct SlimWorkItemDto<Text = String, Todo = TodoItem> {
    pub id: Text,
    pub agent_id: Text,
    pub workspace_id: Text,
    pub revision: u64,
    pub objective: Text,
    pub state: WorkItemState,
    pub plan_status: WorkItemPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_brief_id: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<Text>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Text>,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub focus: WorkItemFocus,
    pub is_current: bool,
    pub is_runnable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_todo: Option<Todo>,
    pub reason_code: WorkItemSchedulingReasonCode,
}

impl From<&crate::runtime::LightweightAgentStateProjection> for SlimAgentDto {
    fn from(projection: &crate::runtime::LightweightAgentStateProjection) -> Self {
        Self {
            identity: projection.identity.clone(),
            agent: (&projection.agent).into(),
            scheduling_posture: projection.scheduling_posture.clone(),
            active_task_count: projection.active_task_count,
            lifecycle: projection.lifecycle.clone(),
            model: (&projection.model).into(),
            closure: (&projection.closure).into(),
            active_children: Vec::new(),
        }
    }
}

impl From<TaskRecord> for SlimTaskDto {
    fn from(task: TaskRecord) -> Self {
        let target = task.target();
        Self {
            id: task.id,
            agent_id: task.agent_id,
            kind: task.kind,
            status: task.status,
            created_at: task.created_at,
            updated_at: task.updated_at,
            parent_message_id: task.parent_message_id,
            work_item_id: task.work_item_id,
            summary: task.summary,
            target,
        }
    }
}

impl From<SlimTaskDto> for TaskRecord {
    fn from(task: SlimTaskDto) -> Self {
        Self {
            id: task.id,
            agent_id: task.agent_id,
            kind: task.kind,
            status: task.status,
            created_at: task.created_at,
            updated_at: task.updated_at,
            parent_message_id: task.parent_message_id,
            work_item_id: task.work_item_id,
            summary: task.summary,
            detail: task
                .target
                .map(|target| serde_json::json!({ "target": target })),
            recovery: None,
        }
    }
}

impl From<SlimWorkspaceDto> for crate::types::AgentWorkspaceInfo {
    fn from(workspace: SlimWorkspaceDto) -> Self {
        Self {
            workspace_id: workspace.workspace_id,
            workspace_alias: workspace.workspace_alias,
            workspace_anchor: workspace.workspace_anchor,
            repo_name: workspace.repo_name,
            is_active: workspace.is_active,
            execution_root_id: workspace.execution_root_id,
            execution_root: workspace.execution_root,
            cwd: workspace.cwd,
            projection_kind: workspace.projection_kind,
            access_mode: workspace.access_mode,
            worktree: workspace.worktree.map(Into::into),
        }
    }
}

impl From<crate::types::AgentWorkspaceInfo> for SlimWorkspaceDto {
    fn from(workspace: crate::types::AgentWorkspaceInfo) -> Self {
        Self {
            workspace_id: workspace.workspace_id,
            workspace_alias: workspace.workspace_alias,
            workspace_anchor: workspace.workspace_anchor,
            repo_name: workspace.repo_name,
            is_active: workspace.is_active,
            execution_root_id: workspace.execution_root_id,
            execution_root: workspace.execution_root,
            cwd: workspace.cwd,
            projection_kind: workspace.projection_kind,
            access_mode: workspace.access_mode,
            worktree: workspace.worktree.map(Into::into),
        }
    }
}

impl From<SlimWorktreeDto> for crate::types::WorktreeInfo {
    fn from(worktree: SlimWorktreeDto) -> Self {
        Self {
            branch: worktree.branch,
            path: worktree.path,
            original_branch: worktree.original_branch,
            original_cwd: worktree.original_cwd,
        }
    }
}

impl From<crate::types::WorktreeInfo> for SlimWorktreeDto {
    fn from(worktree: crate::types::WorktreeInfo) -> Self {
        Self {
            branch: worktree.branch,
            path: worktree.path,
            original_branch: worktree.original_branch,
            original_cwd: worktree.original_cwd,
        }
    }
}

impl From<WorkItemSchedulingProjection> for SlimWorkItemDto {
    fn from(projection: WorkItemSchedulingProjection) -> Self {
        let record = projection.work_item;
        Self {
            id: record.id,
            agent_id: record.agent_id,
            workspace_id: record.workspace_id,
            revision: record.revision,
            objective: record.objective,
            state: record.state,
            plan_status: record.plan_status,
            blocked_by: record.blocked_by,
            recheck_at: record.recheck_at,
            result_brief_id: record.result_brief_id,
            result_summary: record.result_summary,
            created_at: record.created_at,
            updated_at: record.updated_at,
            turn_id: record.turn_id,
            scheduling_state: projection.scheduling_state,
            readiness: projection.readiness,
            candidate_class: projection.candidate_class,
            focus: projection.focus,
            is_current: projection.is_current,
            is_runnable: projection.is_runnable,
            current_todo: projection.current_todo,
            reason_code: projection.reason_code,
        }
    }
}

impl<W: std::borrow::Borrow<crate::types::WorkItemRecord>> From<&WorkItemSchedulingProjection<W>>
    for SlimWorkItemDto
{
    fn from(projection: &WorkItemSchedulingProjection<W>) -> Self {
        let record = projection.record();
        Self {
            id: record.id.clone(),
            agent_id: record.agent_id.clone(),
            workspace_id: record.workspace_id.clone(),
            revision: record.revision,
            objective: record.objective.clone(),
            state: record.state.clone(),
            plan_status: record.plan_status,
            blocked_by: record.blocked_by.clone(),
            recheck_at: record.recheck_at,
            result_brief_id: record.result_brief_id.clone(),
            result_summary: record.result_summary.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            turn_id: record.turn_id.clone(),
            scheduling_state: projection.scheduling_state,
            readiness: projection.readiness,
            candidate_class: projection.candidate_class,
            focus: projection.focus,
            is_current: projection.is_current,
            is_runnable: projection.is_runnable,
            current_todo: projection.current_todo.clone(),
            reason_code: projection.reason_code,
        }
    }
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod work_status_tests {
    use super::*;

    #[test]
    fn slim_wait_keeps_timing_and_task_binding_without_internal_payloads() {
        let record: crate::types::WaitConditionRecord = serde_json::from_value(serde_json::json!({
            "id": "wait-1", "agent_id": "agent-1", "work_item_id": "work-1",
            "kind": "task", "status": "active", "waiting_for": "task_completed",
            "created_at": "2026-09-16T00:00:00Z", "updated_at": "2026-09-16T00:01:00Z",
            "wake_sources": [{"kind": "task_result", "task_id": "task-1"}, {"kind": "operator_input"}],
            "continuation": {"private": "internal"}, "subject_ref": "private-callback"
        })).unwrap();
        let value = serde_json::to_value(SlimWaitDto::from(record)).unwrap();
        assert_eq!(value["task_ids"], serde_json::json!(["task-1"]));
        assert_eq!(value["work_item_id"], "work-1");
        assert_eq!(value["created_at"], "2026-09-16T00:00:00Z");
        assert!(value.get("continuation").is_none());
        assert!(value.get("subject_ref").is_none());
        assert!(value.get("waiting_for").is_none());
    }
}
