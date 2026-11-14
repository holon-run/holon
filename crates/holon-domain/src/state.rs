use super::agent::AgentStatus;
use super::memory::WorkingMemoryState;
use super::message::MessageBody;
use super::serde_helpers::is_false;
use super::skills::ActiveSkillRecord;
use super::tool::ToolLatencyMetrics;
use super::turn::{TurnOwner, TurnTerminalRecord, TurnTerminalSummary};
use super::wake::ExternalTriggerScope;
use super::workspace::{ActiveWorkspaceEntry, WorktreeSession};
use crate::execution_types::ExecutionProfile;
use crate::model_values::ModelRouteRef;
use crate::runtime_error::{RuntimeErrorContext, RuntimeErrorDomain};
use crate::scheduler::{ScenarioMode, SchedulerScenarioClass};
use crate::work_item::WorkItemState;
use crate::ContinuationTriggerKind;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClosureOutcome {
    Completed,
    Continuable,
    Failed,
    Waiting,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkReactivationMode {
    ContinueActive,
    ActivateQueued,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkReactivationSignal {
    pub work_item_id: String,
    pub state: WorkItemState,
    pub reactivation_mode: WorkReactivationMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    AwaitingOperatorInput,
    AwaitingExternalChange,
    AwaitingTaskResult,
    AwaitingTimer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePosture {
    Awake,
    Sleeping,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationClass {
    ResumeExpectedWait,
    ResumeOverride,
    LocalContinuation,
    TaskResultReentry,
    LivenessOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContinuationResolution {
    pub trigger_kind: ContinuationTriggerKind,
    pub class: ContinuationClass,
    // TODO: remove `model_visible` alias after one persisted-state upgrade window.
    #[serde(alias = "model_visible")]
    pub model_reentry: bool,
    pub prior_closure_outcome: ClosureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_waiting_reason: Option<WaitingReason>,
    pub matched_waiting_reason: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClosureDecision {
    pub outcome: ClosureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_signal: Option<WorkReactivationSignal>,
    pub runtime_posture: RuntimePosture,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFailurePhase {
    Startup,
    Shutdown,
    RuntimeTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureArtifactCategory {
    Transport,
    Protocol,
    Runtime,
    Task,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct FailureArtifact {
    pub category: FailureArtifactCategory,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<RuntimeErrorDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<String>,
    #[serde(default, skip_serializing_if = "RuntimeErrorContext::is_empty")]
    pub context: Box<RuntimeErrorContext>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeFailureSummary {
    pub occurred_at: DateTime<Utc>,
    pub summary: String,
    pub phase: RuntimeFailurePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_artifact: Option<FailureArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionAdmissionProvenance {
    Canonical {
        scenario_class: SchedulerScenarioClass,
        activation_id: String,
    },
    /// Historical persisted shape retained for read compatibility only.
    ///
    /// New runtime execution must never create or admit a turn from this variant.
    #[deprecated(note = "historical serde read-only; do not construct in new code")]
    LegacyCompat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scenario_class: Option<SchedulerScenarioClass>,
        effective_mode: ScenarioMode,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkItemExecutionBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_provenance: Option<ExecutionAdmissionProvenance>,
    pub source_message_id: String,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TurnOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_work_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::timezone::deserialize_optional_timezone"
    )]
    pub timezone_override: Option<String>,
    pub id: String,
    pub status: AgentStatus,
    pub sleeping_until: Option<DateTime<Utc>>,
    pub current_run_id: Option<String>,
    /// Current Queue projection. Older State payloads may include this field.
    #[serde(default)]
    pub pending: usize,
    pub last_wake_reason: Option<String>,
    pub last_brief_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub working_memory: WorkingMemoryState,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub total_model_rounds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_token_usage: Option<TokenUsage>,
    #[serde(default)]
    pub tool_latency: Vec<ToolLatencyMetrics>,
    #[serde(default)]
    pub execution_profile: ExecutionProfile,
    #[serde(default)]
    pub pending_wake_hint: Option<PendingWakeHint>,
    #[serde(default)]
    pub attached_workspaces: Vec<String>,
    #[serde(default)]
    pub active_workspace_entry: Option<ActiveWorkspaceEntry>,
    #[serde(default)]
    pub worktree_session: Option<WorktreeSession>,
    #[serde(default)]
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_execution_binding: Option<WorkItemExecutionBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_operator_binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_operator_reply_route_id: Option<String>,
    #[serde(default)]
    pub active_skills: Vec<ActiveSkillRecord>,
    #[serde(default)]
    pub last_continuation: Option<ContinuationResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override_service_tier: Option<crate::model_values::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_fallback_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_requested_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_terminal: Option<TurnTerminalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_runtime_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_budget: Option<TurnBudget>,
    /// In-memory sub-state of the active execution: this agent is locally
    /// waiting for a shared provider cooldown deadline before retrying the
    /// same candidate. Never persisted, so a restart never reports a stale
    /// wait; cleared when the owning turn settles.
    #[serde(skip)]
    pub provider_cooldown_wait: Option<ProviderCooldownWait>,
}

/// Turn budget set by `run_once` to signal the maximum number of turns
/// for the current run. The turn execution pipeline reads this to inject
/// a budget warning when the agent is on the last allowed turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnBudget {
    pub max_turns: u64,
    pub run_start_turn_index: u64,
}

/// Active execution sub-state for a turn-local provider cooldown wait
/// (#3772 option A). Describes what the runtime is waiting for and the
/// earliest retry time; it is not a terminal state, creates no scheduling
/// wake, and never calls the model by itself.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProviderCooldownWait {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub model: Option<ModelRouteRef>,
    pub started_at: DateTime<Utc>,
    /// Authoritative absolute deadline; the earliest retry attempt.
    pub not_before: DateTime<Utc>,
    #[serde(default)]
    pub cause: ProviderCooldownCauseSummary,
}

/// Sanitized origin of a shared cooldown. Carries only classification
/// metadata: no raw upstream error text, no URLs, and no other agent's
/// request content. Missing fields degrade honestly instead of being
/// reconstructed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProviderCooldownCauseSummary {
    /// Failure classification allowlist: connection | timeout | rate_limited |
    /// server_error | transport. None when no cause record is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<ProviderCooldownRetryAfter>,
    /// Local, opaque incident identity for authorized diagnostics lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident_id: Option<String>,
    /// Honest degradation marker, e.g. "cause record unavailable".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProviderCooldownRetryAfter {
    pub source: ProviderCooldownRetryAfterSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCooldownRetryAfterSource {
    ServerHeader,
    LocalBackoff,
}

/// Why an active provider cooldown wait ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCooldownWaitEndOutcome {
    /// Deadline reached, revalidation passed, dispatch retried.
    Resumed,
    /// New operator input returned to the same turn's budgeted input boundary.
    Interjected,
    /// Stop, owner closure, or source replacement aborted the wait.
    Cancelled,
    /// Revalidation at the deadline failed; the original denial path holds.
    ValidationFailed,
    /// Replaced by a newer wait or cleared by turn settlement.
    Superseded,
}

/// Terminal transition record for a provider cooldown wait, carried on the
/// final `agent_state_changed` event of the wait.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProviderCooldownWaitEnded {
    pub outcome: ProviderCooldownWaitEndOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStateChangedEvent {
    pub agent_id: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleeping_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run_id: Option<String>,
    pub pending: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_wake_reason: Option<String>,
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_terminal: Option<TurnTerminalSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_workspace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_occupancy_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree_active: bool,
    /// Active turn-local provider cooldown wait, when the execution is
    /// currently waiting for a shared cooldown deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cooldown_wait: Option<ProviderCooldownWait>,
    /// Present only on the event that clears an active cooldown wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cooldown_wait_ended: Option<ProviderCooldownWaitEnded>,
}

impl AgentStateChangedEvent {
    pub fn from_state(state: &AgentState) -> Self {
        let active_workspace = state.active_workspace_entry.as_ref();
        Self {
            agent_id: state.id.clone(),
            status: state.status.clone(),
            sleeping_until: state.sleeping_until,
            current_run_id: state.current_run_id.clone(),
            pending: state.pending,
            last_wake_reason: state.last_wake_reason.clone(),
            turn_index: state.turn_index,
            current_turn_id: state.current_turn_id.clone(),
            current_turn_work_item_id: state.current_turn_work_item_id.clone(),
            current_work_item_id: state.current_work_item_id.clone(),
            last_turn_terminal: state
                .last_turn_terminal
                .as_ref()
                .map(TurnTerminalSummary::from_terminal),
            attached_workspace_ids: state.attached_workspaces.clone(),
            active_workspace_id: active_workspace.map(|entry| entry.workspace_id.clone()),
            active_execution_root_id: active_workspace.map(|entry| entry.execution_root_id.clone()),
            active_workspace_occupancy_id: active_workspace
                .and_then(|entry| entry.occupancy_id.clone()),
            worktree_active: state.worktree_session.is_some(),
            provider_cooldown_wait: state.provider_cooldown_wait.clone(),
            provider_cooldown_wait_ended: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
        }
    }

    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct AgentTokenUsageSummary {
    pub total: TokenUsage,
    pub total_model_rounds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TokenUsage>,
}

impl AgentState {
    pub fn set_timezone(&mut self, timezone: Option<&str>) -> anyhow::Result<()> {
        if let Some(timezone) = timezone {
            crate::timezone::parse_timezone(timezone)?;
        }
        self.timezone_override = timezone.map(str::to_owned);
        Ok(())
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: AgentStatus::Booting,
            timezone_override: None,
            sleeping_until: None,
            current_run_id: None,
            pending: 0,
            last_wake_reason: None,
            last_brief_at: None,
            working_memory: WorkingMemoryState::default(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_model_rounds: 0,
            last_turn_token_usage: None,
            tool_latency: Vec::new(),
            execution_profile: ExecutionProfile::default(),
            pending_wake_hint: None,
            attached_workspaces: Vec::new(),
            active_workspace_entry: None,
            worktree_session: None,
            turn_index: 0,
            current_turn_id: None,
            current_turn_work_item_id: None,
            current_execution_binding: None,
            current_work_item_id: None,
            current_turn_operator_binding_id: None,
            current_turn_operator_reply_route_id: None,
            active_skills: Vec::new(),
            last_continuation: None,
            model_override: None,
            model_override_reasoning_effort: None,
            model_override_service_tier: None,
            pending_fallback_model: None,
            last_requested_model: None,
            last_active_model: None,
            last_turn_terminal: None,
            last_runtime_failure: None,
            turn_budget: None,
            provider_cooldown_wait: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingWakeHint {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ExternalTriggerScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_trigger_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}
