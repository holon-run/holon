//! The existing Slim schema borrowed before JSON output allocation.
use super::*;
use serde::ser::SerializeSeq;

pub(crate) struct SlimAgentDtoRef<'a>(
    pub(crate) &'a crate::runtime::LightweightAgentStateProjection,
);

#[derive(Serialize)]
struct Compact<'a> {
    identity: &'a AgentIdentityView,
    agent: Runtime<'a>,
    scheduling_posture: &'a AgentPostureProjection,
    active_task_count: usize,
    lifecycle: &'a AgentLifecycleHint,
    model: Model<'a>,
    closure: Closure<'a>,
    #[serde(skip_serializing_if = "Children::is_empty")]
    active_children: Children<'a>,
}
#[derive(Serialize)]
struct Runtime<'a> {
    id: &'a str,
    status: &'a AgentStatus,
    sleeping_until: &'a Option<DateTime<Utc>>,
    current_run_id: &'a Option<String>,
    pending: usize,
    last_wake_reason: &'a Option<String>,
    last_brief_at: &'a Option<DateTime<Utc>>,
    attached_workspaces: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    active_workspace_entry: Option<Workspace<'a>>,
    turn_index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_turn_work_item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_work_item_id: Option<&'a str>,
    // The original From<&AgentState> deliberately omits both terminal fields.
}
struct DisplayPath<'a>(&'a std::path::Path);
impl Serialize for DisplayPath<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0.display())
    }
}
#[derive(Serialize)]
struct Workspace<'a> {
    workspace_id: &'a str,
    workspace_anchor: DisplayPath<'a>,
    execution_root_id: &'a str,
    execution_root: DisplayPath<'a>,
    projection_kind: WorkspaceProjectionKind,
    access_mode: WorkspaceAccessMode,
    cwd: DisplayPath<'a>,
}
impl<'a> From<&'a ActiveWorkspaceEntry> for Workspace<'a> {
    fn from(value: &'a ActiveWorkspaceEntry) -> Self {
        Self {
            workspace_id: &value.workspace_id,
            workspace_anchor: DisplayPath(&value.workspace_anchor),
            execution_root_id: &value.execution_root_id,
            execution_root: DisplayPath(&value.execution_root),
            projection_kind: value.projection_kind,
            access_mode: value.access_mode,
            cwd: DisplayPath(&value.cwd),
        }
    }
}
#[derive(Serialize)]
struct Model<'a> {
    source: &'a AgentModelSource,
    runtime_default_model: &'a ModelRouteRef,
    effective_model: &'a ModelRouteRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_model: Option<&'a ModelRouteRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_model: Option<&'a ModelRouteRef>,
    fallback_active: bool,
    #[serde(skip_serializing_if = "no_routes")]
    effective_fallback_models: &'a [ModelRouteRef],
    #[serde(skip_serializing_if = "Option::is_none")]
    override_model: Option<&'a ModelRouteRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    override_reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    override_service_tier: &'a Option<crate::config::ServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_service_tier: &'a Option<crate::config::ServiceTier>,
}
fn no_routes(value: &&[ModelRouteRef]) -> bool {
    value.is_empty()
}
#[derive(Serialize)]
struct Closure<'a> {
    outcome: &'a ClosureOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting_reason: &'a Option<WaitingReason>,
    runtime_posture: &'a RuntimePosture,
}
struct Children<'a>(&'a [ChildAgentSummary]);
impl Children<'_> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl Serialize for Children<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for child in self.0 {
            sequence.serialize_element(&Child {
                identity: &child.identity,
                status: &child.status,
                current_run_id: child.current_run_id.as_deref(),
                pending: child.pending,
                active_task_count: child.active_task_count,
                observability: Observability {
                    phase: &child.observability.phase,
                    blocked_reason: &child.observability.blocked_reason,
                    waiting_reason: &child.observability.waiting_reason,
                    current_work_item_id: child.observability.current_work_item_id.as_deref(),
                    work_summary: child.observability.work_summary.as_deref(),
                    last_progress_brief: child.observability.last_progress_brief.as_deref(),
                    last_result_brief: child.observability.last_result_brief.as_deref(),
                },
            })?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct Child<'a> {
    identity: &'a AgentIdentityView,
    status: &'a AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_run_id: Option<&'a str>,
    pending: usize,
    active_task_count: usize,
    observability: Observability<'a>,
}
#[derive(Serialize)]
struct Observability<'a> {
    phase: &'a ChildAgentPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: &'a Option<ChildAgentBlockedReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting_reason: &'a Option<WaitingReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_work_item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_progress_brief: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_result_brief: Option<&'a str>,
}
impl Serialize for SlimAgentDtoRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let projection = self.0;
        let state = &projection.agent;
        let model = &projection.model;
        Compact {
            identity: &projection.identity,
            agent: Runtime {
                id: &state.id,
                status: &state.status,
                sleeping_until: &state.sleeping_until,
                current_run_id: &state.current_run_id,
                pending: state.pending,
                last_wake_reason: &state.last_wake_reason,
                last_brief_at: &state.last_brief_at,
                attached_workspaces: &state.attached_workspaces,
                active_workspace_entry: state.active_workspace_entry.as_ref().map(Into::into),
                turn_index: state.turn_index,
                current_turn_id: state.current_turn_id.as_deref(),
                current_turn_work_item_id: state.current_turn_work_item_id.as_deref(),
                current_work_item_id: state.current_work_item_id.as_deref(),
            },
            scheduling_posture: &projection.scheduling_posture,
            active_task_count: projection.active_task_count,
            lifecycle: &projection.lifecycle,
            model: Model {
                source: &model.source,
                runtime_default_model: &model.runtime_default_model,
                effective_model: &model.effective_model,
                requested_model: model.requested_model.as_ref(),
                active_model: model.active_model.as_ref(),
                fallback_active: model.fallback_active,
                effective_fallback_models: &model.effective_fallback_models,
                override_model: model.override_model.as_ref(),
                override_reasoning_effort: model.override_reasoning_effort.as_deref(),
                override_service_tier: &model.override_service_tier,
                effective_service_tier: &model.effective_service_tier,
            },
            closure: Closure {
                outcome: &projection.closure.outcome,
                waiting_reason: &projection.closure.waiting_reason,
                runtime_posture: &projection.closure.runtime_posture,
            },
            active_children: Children(&projection.active_children),
        }
        .serialize(serializer)
    }
}
