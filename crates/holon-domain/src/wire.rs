use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    execution_types::{WorkspaceAccessMode, WorkspaceProjectionKind},
    model_values::ModelRouteRef,
    ActiveWorkspaceEntry, AgentIdentityView, AgentLifecycleHint, AgentListEntry,
    AgentListModelSummary, AgentModelSource, AgentPostureProjection, AgentState, AgentStatus,
    AgentSummary, ChildAgentBlockedReason, ChildAgentObservabilitySnapshot, ChildAgentPhase,
    ChildAgentSummary, ClosureDecision, ClosureOutcome, RuntimeFailureSummary, RuntimePosture,
    TurnTerminalRecord, WaitingReason,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetAgentResult {
    pub agent: SlimAgentDto,
}

/// Bounded, read-only lifecycle responsibility; never a deletion decision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SubagentCleanupCandidate {
    pub agent_id: String,
    pub incarnation: u64,
    pub blocker: Option<String>,
    pub observed_since: Option<String>,
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
    /// Active turn-local provider cooldown wait, when one is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cooldown_wait: Option<crate::ProviderCooldownWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_terminal: Option<TurnTerminalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_runtime_failure: Option<RuntimeFailureSummary>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_service_tier: Option<crate::model_values::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_service_tier: Option<crate::model_values::ServiceTier>,
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
            provider_cooldown_wait: agent.provider_cooldown_wait.clone(),
            last_turn_terminal: None,
            last_runtime_failure: None,
        }
    }
}

impl From<AgentState> for SlimAgentRuntimeDto {
    fn from(agent: AgentState) -> Self {
        Self {
            id: agent.id,
            status: agent.status,
            sleeping_until: agent.sleeping_until,
            current_run_id: agent.current_run_id,
            pending: agent.pending,
            last_wake_reason: agent.last_wake_reason,
            last_brief_at: agent.last_brief_at,
            attached_workspaces: agent.attached_workspaces,
            active_workspace_entry: agent.active_workspace_entry.as_ref().map(Into::into),
            turn_index: agent.turn_index,
            current_turn_id: agent.current_turn_id,
            current_turn_work_item_id: agent.current_turn_work_item_id,
            current_work_item_id: agent.current_work_item_id,
            provider_cooldown_wait: agent.provider_cooldown_wait,
            last_turn_terminal: None,
            last_runtime_failure: None,
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

impl From<&crate::AgentModelState> for SlimAgentModelDto {
    fn from(model: &crate::AgentModelState) -> Self {
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
            override_service_tier: model.override_service_tier,
            effective_service_tier: model.effective_service_tier,
        }
    }
}

impl From<crate::AgentModelState> for SlimAgentModelDto {
    fn from(model: crate::AgentModelState) -> Self {
        Self {
            source: model.source,
            runtime_default_model: model.runtime_default_model,
            effective_model: model.effective_model,
            requested_model: model.requested_model,
            active_model: model.active_model,
            fallback_active: model.fallback_active,
            effective_fallback_models: model.effective_fallback_models,
            override_model: model.override_model,
            override_reasoning_effort: model.override_reasoning_effort,
            override_service_tier: model.override_service_tier,
            effective_service_tier: model.effective_service_tier,
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
            override_service_tier: self.model.override_service_tier,
            effective_service_tier: self.model.effective_service_tier,
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
        // This is an internal TUI projection, not a full domain roundtrip. Fields omitted from
        // SlimAgentRuntimeDto intentionally retain placeholder defaults; transported fields must
        // be assigned explicitly here.
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
        summary.agent.last_runtime_failure = self.agent.last_runtime_failure;
        summary.provider_cooldown_wait = self.agent.provider_cooldown_wait;
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
            _read_material: None,
            _source_material: None,
        }
    }
}
