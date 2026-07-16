//! Shared HTTP wire DTOs for first-party state bootstrap clients.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::ModelRouteRef,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AgentIdentityView, AgentLifecycleHint, AgentListEntry,
        AgentListModelSummary, AgentModelSource, AgentPostureProjection, AgentState, AgentStatus,
        AgentSummary, ChildAgentBlockedReason, ChildAgentObservabilitySnapshot, ChildAgentPhase,
        ChildAgentSummary, ClosureDecision, ClosureOutcome, ExternalTriggerStateSnapshot,
        RuntimePosture, TaskKind, TaskRecord, TaskStatus, TimerRecord, TodoItem,
        TurnTerminalRecord, WaitingReason, WorkItemPlanStatus, WorkItemReadiness,
        WorkItemSchedulingState, WorkItemState,
    },
    work_item_scheduling::{
        WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingProjection,
        WorkItemSchedulingReasonCode,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentStateSnapshotDto {
    pub agent: SlimAgentDto,
    pub session: StateSessionSnapshotDto,
    pub tasks: Vec<SlimTaskDto>,
    #[serde(default)]
    pub timers: Vec<TimerRecord>,
    #[serde(default)]
    pub work_items: Vec<SlimWorkItemDto>,
    #[serde(default)]
    pub external_triggers: Vec<ExternalTriggerStateSnapshot>,
    #[serde(default)]
    pub workspace: StateWorkspaceSnapshotDto,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SlimAgentDto {
    pub identity: AgentIdentityView,
    pub agent: SlimAgentRuntimeDto,
    #[serde(default)]
    pub scheduling_posture: AgentPostureProjection,
    #[serde(default)]
    pub active_task_count: usize,
    #[serde(default)]
    pub lifecycle: AgentLifecycleHint,
    pub model: SlimAgentModelDto,
    pub closure: SlimClosureDto,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_children: Vec<SlimChildAgentDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SlimAgentRuntimeDto {
    pub id: String,
    pub status: AgentStatus,
    pub sleeping_until: Option<DateTime<Utc>>,
    pub current_run_id: Option<String>,
    pub pending: usize,
    pub last_wake_reason: Option<String>,
    pub last_brief_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attached_workspaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_entry: Option<SlimActiveWorkspaceDto>,
    #[serde(default)]
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_terminal: Option<TurnTerminalRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimActiveWorkspaceDto {
    pub workspace_id: String,
    pub workspace_anchor: String,
    pub execution_root_id: String,
    pub execution_root: String,
    pub projection_kind: WorkspaceProjectionKind,
    pub access_mode: WorkspaceAccessMode,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimAgentModelDto {
    pub source: AgentModelSource,
    #[schemars(with = "String")]
    pub runtime_default_model: ModelRouteRef,
    #[schemars(with = "String")]
    pub effective_model: ModelRouteRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub requested_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub active_model: Option<ModelRouteRef>,
    #[serde(default)]
    pub fallback_active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Vec<String>")]
    pub effective_fallback_models: Vec<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub override_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimClosureDto {
    pub outcome: ClosureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    pub runtime_posture: RuntimePosture,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SlimChildAgentDto {
    pub identity: AgentIdentityView,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run_id: Option<String>,
    pub pending: usize,
    pub active_task_count: usize,
    pub observability: SlimChildObservabilityDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimChildObservabilityDto {
    pub phase: ChildAgentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<ChildAgentBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_brief: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result_brief: Option<String>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SlimWorkItemDto {
    pub id: String,
    pub agent_id: String,
    pub workspace_id: String,
    pub revision: u64,
    pub objective: String,
    pub state: WorkItemState,
    pub plan_status: WorkItemPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_brief_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub focus: WorkItemFocus,
    pub is_current: bool,
    pub is_runnable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_todo: Option<TodoItem>,
    pub reason_code: WorkItemSchedulingReasonCode,
}

impl From<&AgentSummary> for SlimAgentDto {
    fn from(summary: &AgentSummary) -> Self {
        Self {
            identity: summary.identity.clone(),
            agent: (&summary.agent).into(),
            scheduling_posture: summary.scheduling_posture.clone(),
            active_task_count: summary.active_task_count,
            lifecycle: summary.lifecycle.clone(),
            model: (&summary.model).into(),
            closure: (&summary.closure).into(),
            active_children: summary.active_children.iter().map(Into::into).collect(),
        }
    }
}

impl From<&AgentState> for SlimAgentRuntimeDto {
    fn from(agent: &AgentState) -> Self {
        Self {
            id: agent.id.clone(),
            status: agent.status.clone(),
            sleeping_until: agent.sleeping_until,
            current_run_id: agent.current_run_id.clone(),
            pending: agent.pending,
            last_wake_reason: agent.last_wake_reason.clone(),
            last_brief_at: agent.last_brief_at,
            attached_workspaces: agent.attached_workspaces.clone(),
            active_workspace_entry: agent.active_workspace_entry.as_ref().map(Into::into),
            turn_index: agent.turn_index,
            current_turn_id: agent.current_turn_id.clone(),
            current_turn_work_item_id: agent.current_turn_work_item_id.clone(),
            current_work_item_id: agent.current_work_item_id.clone(),
            last_turn_terminal: None,
        }
    }
}

impl From<&ActiveWorkspaceEntry> for SlimActiveWorkspaceDto {
    fn from(workspace: &ActiveWorkspaceEntry) -> Self {
        Self {
            workspace_id: workspace.workspace_id.clone(),
            workspace_anchor: workspace.workspace_anchor.display().to_string(),
            execution_root_id: workspace.execution_root_id.clone(),
            execution_root: workspace.execution_root.display().to_string(),
            projection_kind: workspace.projection_kind,
            access_mode: workspace.access_mode,
            cwd: workspace.cwd.display().to_string(),
        }
    }
}

impl From<&crate::types::AgentModelState> for SlimAgentModelDto {
    fn from(model: &crate::types::AgentModelState) -> Self {
        Self {
            source: model.source.clone(),
            runtime_default_model: model.runtime_default_model.clone(),
            effective_model: model.effective_model.clone(),
            requested_model: model.requested_model.clone(),
            active_model: model.active_model.clone(),
            fallback_active: model.fallback_active,
            effective_fallback_models: model.effective_fallback_models.clone(),
            override_model: model.override_model.clone(),
            override_reasoning_effort: model.override_reasoning_effort.clone(),
        }
    }
}

impl From<&ClosureDecision> for SlimClosureDto {
    fn from(closure: &ClosureDecision) -> Self {
        Self {
            outcome: closure.outcome,
            waiting_reason: closure.waiting_reason,
            runtime_posture: closure.runtime_posture,
        }
    }
}

impl From<&ChildAgentSummary> for SlimChildAgentDto {
    fn from(child: &ChildAgentSummary) -> Self {
        Self {
            identity: child.identity.clone(),
            status: child.status.clone(),
            current_run_id: child.current_run_id.clone(),
            pending: child.pending,
            active_task_count: child.active_task_count,
            observability: (&child.observability).into(),
        }
    }
}

impl From<&ChildAgentObservabilitySnapshot> for SlimChildObservabilityDto {
    fn from(observability: &ChildAgentObservabilitySnapshot) -> Self {
        Self {
            phase: observability.phase.clone(),
            blocked_reason: observability.blocked_reason.clone(),
            waiting_reason: observability.waiting_reason,
            current_work_item_id: observability.current_work_item_id.clone(),
            work_summary: observability.work_summary.clone(),
            last_progress_brief: observability.last_progress_brief.clone(),
            last_result_brief: observability.last_result_brief.clone(),
        }
    }
}

impl From<TaskRecord> for SlimTaskDto {
    fn from(task: TaskRecord) -> Self {
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
            detail: None,
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

impl SlimAgentDto {
    pub fn into_agent_summary(self) -> AgentSummary {
        let active_workspace_entry = self.agent.active_workspace_entry.clone().map(Into::into);
        let model = AgentListModelSummary {
            source: self.model.source,
            runtime_default_model: self.model.runtime_default_model,
            effective_model: self.model.effective_model,
            requested_model: self.model.requested_model,
            active_model: self.model.active_model,
            fallback_active: self.model.fallback_active,
            effective_fallback_models: self.model.effective_fallback_models,
            override_model: self.model.override_model,
            override_reasoning_effort: self.model.override_reasoning_effort,
        };
        let mut summary = AgentListEntry {
            identity: self.identity,
            status: self.agent.status.clone(),
            scheduling_posture: self.scheduling_posture,
            lifecycle: self.lifecycle,
            pending: self.agent.pending,
            current_run_id: self.agent.current_run_id.clone(),
            waiting_reason: self.closure.waiting_reason,
            model,
            active_workspace_entry,
        }
        .into_agent_summary_placeholder();
        summary.agent.id = self.agent.id;
        summary.agent.status = self.agent.status;
        summary.agent.sleeping_until = self.agent.sleeping_until;
        summary.agent.current_run_id = self.agent.current_run_id;
        summary.agent.pending = self.agent.pending;
        summary.agent.last_wake_reason = self.agent.last_wake_reason;
        summary.agent.last_brief_at = self.agent.last_brief_at;
        summary.agent.attached_workspaces = self.agent.attached_workspaces;
        summary.agent.turn_index = self.agent.turn_index;
        summary.agent.current_turn_id = self.agent.current_turn_id;
        summary.agent.current_turn_work_item_id = self.agent.current_turn_work_item_id;
        summary.agent.current_work_item_id = self.agent.current_work_item_id;
        summary.agent.last_turn_terminal = self.agent.last_turn_terminal;
        summary.active_task_count = self.active_task_count;
        summary.closure = ClosureDecision {
            outcome: self.closure.outcome,
            waiting_reason: self.closure.waiting_reason,
            work_signal: None,
            runtime_posture: self.closure.runtime_posture,
            evidence: Vec::new(),
        };
        summary.active_children = self.active_children.into_iter().map(Into::into).collect();
        summary
    }
}

impl From<SlimActiveWorkspaceDto> for ActiveWorkspaceEntry {
    fn from(workspace: SlimActiveWorkspaceDto) -> Self {
        Self {
            workspace_id: workspace.workspace_id,
            workspace_anchor: PathBuf::from(workspace.workspace_anchor),
            execution_root_id: workspace.execution_root_id,
            execution_root: PathBuf::from(workspace.execution_root),
            projection_kind: workspace.projection_kind,
            access_mode: workspace.access_mode,
            cwd: PathBuf::from(workspace.cwd),
            occupancy_id: None,
            projection_metadata: None,
        }
    }
}

impl From<SlimChildAgentDto> for ChildAgentSummary {
    fn from(child: SlimChildAgentDto) -> Self {
        Self {
            identity: child.identity,
            status: child.status,
            current_run_id: child.current_run_id,
            pending: child.pending,
            active_task_count: child.active_task_count,
            observability: child.observability.into(),
        }
    }
}

impl From<SlimChildObservabilityDto> for ChildAgentObservabilitySnapshot {
    fn from(observability: SlimChildObservabilityDto) -> Self {
        Self {
            phase: observability.phase,
            blocked_reason: observability.blocked_reason,
            waiting_reason: observability.waiting_reason,
            current_work_item_id: observability.current_work_item_id,
            work_summary: observability.work_summary,
            last_progress_brief: observability.last_progress_brief,
            last_result_brief: observability.last_result_brief,
        }
    }
}
