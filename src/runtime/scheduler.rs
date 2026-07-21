use super::*;
use crate::domain::scheduler_semantic::{
    structural_semantic_proposal, SemanticProposalProviderConfig, SemanticProposalProviderIdentity,
    SemanticProposalResponse, SemanticValidationPolicy, SemanticWaitCandidate,
    SemanticWaitCandidateState, SemanticWorkItemCandidate, SemanticWorkItemCandidateState,
    TrustedSemanticIngress, SEMANTIC_CONTRACT_REVISION,
};
use crate::runtime::closure::runtime_error_active;
use crate::storage::{AppStorage, WorkQueueReadModel};
use crate::types::{
    AgentPostureProjection, AgentSchedulingPosture, AgentStatus, AuthorityClass,
    ExternalWaitRecoverability, MessageEnvelope, MessageKind, MessageOrigin, PendingWakeHint,
    Priority, QueueEntryRecord, QueueEntryStatus, TaskRecord, TaskStatus, TimerStatus,
    TurnTerminalKind, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
    WorkItemRecord, WorkItemSchedulingState, WorkItemState, WorkReactivationMode,
    WorkReactivationSignal,
};
use crate::work_item_scheduling::WorkItemSchedulingProjection;
use chrono::{DateTime, Utc};
use serde::Serialize;

const REDUCER_ONLY_CANDIDATES_SCENARIO: &str = "reducer_only_candidates";
const WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO: &str = "work_item_autonomous_continuation";
const WAIT_RESUME_SCENARIO: &str = "wait_resume";
const SETTLEMENT_SCENARIO: &str = "settlement";
const DELIVERY_SCENARIO: &str = "delivery";
const INTERJECTION_SCENARIO: &str = "operator_interjection";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SchedulerProjection {
    /// Captured once per scheduling decision and included in derived equality.
    now: DateTime<Utc>,
    pub status: AgentStatus,
    pub queue_len: usize,
    pub active_run_id: Option<String>,
    pub active_tasks: Vec<TaskRecord>,
    pub has_blocking_active_tasks: bool,
    pub current_work_item: Option<WorkItemRecord>,
    pub current_work_item_scheduling_state: Option<WorkItemSchedulingState>,
    pub queued_runnable_work_items: Vec<WorkItemRecord>,
    pub queued_work_items: usize,
    pub pending_wake_hint: bool,
    pub active_waiting_intents: usize,
    pub active_work_item_waiting_intents: usize,
    pub active_agent_waiting_intents: usize,
    pub active_timers: usize,
    pub waiting_work_item: Option<WorkItemRecord>,
    pub waiting_work_item_scheduling_state: Option<WorkItemSchedulingState>,
    pub last_turn_terminal: Option<TurnTerminalKind>,
    pub turn_in_progress: bool,
    pub runtime_error: bool,
    semantic_waits: Vec<WaitConditionRecord>,
    semantic_work_items: Vec<WorkItemSchedulingProjection>,
}

pub(crate) struct SchedulerAgentSnapshot {
    id: String,
    status: AgentStatus,
    active_run_id: Option<String>,
    pending_wake_hint: bool,
    last_turn_terminal: Option<TurnTerminalKind>,
}

impl SchedulerAgentSnapshot {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn from_state(state: &AgentState) -> Self {
        Self {
            id: state.id.clone(),
            status: state.status.clone(),
            active_run_id: state.current_run_id.clone(),
            pending_wake_hint: state.pending_wake_hint.is_some(),
            last_turn_terminal: state
                .last_turn_terminal
                .as_ref()
                .map(|terminal| terminal.kind.clone()),
        }
    }
}

impl SchedulerProjection {
    pub(crate) fn from_state(storage: &AppStorage, state: &AgentState) -> Result<Self> {
        Self::from_state_with_queue_len(storage, state, state.pending)
    }

    pub(crate) fn from_state_with_queue_len(
        storage: &AppStorage,
        state: &AgentState,
        queue_len: usize,
    ) -> Result<Self> {
        Self::from_state_with_queue_len_at(storage, state, queue_len, Utc::now())
    }

    pub(crate) fn from_state_with_queue_len_at(
        storage: &AppStorage,
        state: &AgentState,
        queue_len: usize,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len_at(storage, &snapshot, queue_len, now)
    }

    pub(crate) fn from_snapshot_with_queue_len_at(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let work_queue = storage.work_queue_prompt_projection()?;
        Self::from_snapshot_with_queue_len_and_work_queue_at(
            storage, snapshot, queue_len, work_queue, now,
        )
    }

    pub(crate) fn from_state_with_work_queue_at(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: WorkQueueReadModel,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len_and_work_queue_at(
            storage,
            &snapshot,
            state.pending,
            work_queue,
            now,
        )
    }

    pub(crate) fn from_snapshot_with_queue_len_and_work_queue_at(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: WorkQueueReadModel,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let active_tasks =
            storage.latest_active_task_records_for_agent(&snapshot.id, usize::MAX)?;
        let has_blocking_active_tasks = active_tasks.iter().any(TaskRecord::is_blocking);
        let queued_runnable_work_items = work_queue
            .queued_runnable
            .iter()
            .map(|item| item.work_item.clone())
            .collect::<Vec<_>>();
        let current_work_item_scheduling_state = work_queue
            .items
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.scheduling_state);
        let waiting_work_item_projection = work_queue.items.iter().find(|item| {
            (item.is_current || item.has_active_waits || item.has_active_task_waits)
                && matches!(
                    item.scheduling_state,
                    WorkItemSchedulingState::WaitingOperator
                        | WorkItemSchedulingState::WaitingTask
                        | WorkItemSchedulingState::WaitingExternal
                        | WorkItemSchedulingState::WaitingTimer
                        | WorkItemSchedulingState::WaitingSystem
                )
        });
        let waiting_work_item = waiting_work_item_projection.map(|item| item.work_item.clone());
        let waiting_work_item_scheduling_state =
            waiting_work_item_projection.map(|item| item.scheduling_state);
        let active_wait_conditions = storage.active_wait_conditions_for_agent(&snapshot.id)?;
        let active_work_item_waiting_intents = active_wait_conditions
            .iter()
            .filter(|condition| condition.work_item_id.is_some())
            .count();
        let active_agent_waiting_intents = active_wait_conditions
            .iter()
            .filter(|condition| condition.work_item_id.is_none())
            .filter(|condition| {
                matches!(
                    condition.kind,
                    WaitConditionKind::External
                        | WaitConditionKind::Timer
                        | WaitConditionKind::System
                        | WaitConditionKind::Operator
                )
            })
            .count();
        let active_timers = storage
            .latest_timer_records()?
            .into_iter()
            .filter(|timer| timer.agent_id == snapshot.id && timer.status == TimerStatus::Active)
            .count();
        Ok(Self {
            now,
            status: snapshot.status.clone(),
            queue_len,
            active_run_id: snapshot.active_run_id.clone(),
            active_tasks,
            has_blocking_active_tasks,
            current_work_item: work_queue.current,
            current_work_item_scheduling_state,
            queued_work_items: queued_runnable_work_items.len(),
            queued_runnable_work_items,
            pending_wake_hint: snapshot.pending_wake_hint,
            active_waiting_intents: active_wait_conditions.len(),
            active_work_item_waiting_intents,
            active_agent_waiting_intents,
            active_timers,
            waiting_work_item,
            waiting_work_item_scheduling_state,
            last_turn_terminal: snapshot.last_turn_terminal.clone(),
            turn_in_progress: snapshot.active_run_id.is_some(),
            runtime_error: runtime_error_active(
                &storage.read_recent_events(64)?,
                &storage.read_recent_briefs(64)?,
            ),
            semantic_waits: active_wait_conditions,
            semantic_work_items: work_queue.items,
        })
    }

    pub(crate) fn work_reactivation_signal(&self) -> Option<WorkReactivationSignal> {
        self.current_work_item
            .as_ref()
            .filter(|_| {
                self.current_work_item_scheduling_state == Some(WorkItemSchedulingState::Runnable)
            })
            .map(|item| WorkReactivationSignal {
                work_item_id: item.id.clone(),
                state: item.state.clone(),
                reactivation_mode: WorkReactivationMode::ContinueActive,
            })
            .or_else(|| {
                self.queued_runnable_work_items
                    .first()
                    .map(|item| WorkReactivationSignal {
                        work_item_id: item.id.clone(),
                        state: item.state.clone(),
                        reactivation_mode: WorkReactivationMode::ActivateQueued,
                    })
            })
    }

    pub(crate) fn current_work_item_waits_for_operator(&self) -> bool {
        self.current_work_item_scheduling_state == Some(WorkItemSchedulingState::WaitingOperator)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingAdvisory {
    pub kind: String,
    pub severity: SchedulingAdvisorySeverity,
    pub message: String,
    pub work_item_id: Option<String>,
    pub wait_condition_id: Option<String>,
    pub evidence: Vec<String>,
}

impl SchedulingAdvisory {
    fn warning(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            severity: SchedulingAdvisorySeverity::Warning,
            message: message.into(),
            work_item_id: None,
            wait_condition_id: None,
            evidence: Vec::new(),
        }
    }

    fn work_item_id(mut self, work_item_id: impl Into<String>) -> Self {
        self.work_item_id = Some(work_item_id.into());
        self
    }

    fn wait_condition_id(mut self, wait_condition_id: impl Into<String>) -> Self {
        self.wait_condition_id = Some(wait_condition_id.into());
        self
    }

    fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulingAdvisorySeverity {
    Warning,
}

impl SchedulingAdvisorySeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
        }
    }
}

/// Derive evidence-based scheduler diagnostics from authoritative runtime facts.
///
/// Diagnostics are advisory observability signals only. They must not be used as
/// scheduler decisions or as a replacement for the posture/work-item state
/// derivation itself.
#[cfg(test)]
pub(crate) fn scheduling_advisories(
    storage: &AppStorage,
    agent: &AgentState,
) -> Result<Vec<SchedulingAdvisory>> {
    scheduling_advisories_with_queue_len(storage, agent, agent.pending)
}

pub(crate) fn scheduling_advisories_with_queue_len(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<Vec<SchedulingAdvisory>> {
    let projection = SchedulerProjection::from_state_with_queue_len(storage, agent, queue_len)?;
    let posture = storage.agent_posture_projection(agent)?;
    let work_queue = storage.work_queue_prompt_projection()?;
    let wait_conditions = storage.active_wait_conditions()?;

    Ok(scheduling_advisories_for_facts(
        agent,
        &projection,
        &posture,
        &work_queue,
        &wait_conditions,
    ))
}

pub(crate) fn scheduling_advisories_for_facts(
    agent: &AgentState,
    projection: &SchedulerProjection,
    posture: &AgentPostureProjection,
    work_queue: &WorkQueueReadModel,
    wait_conditions: &[WaitConditionRecord],
) -> Vec<SchedulingAdvisory> {
    let mut diagnostics = Vec::new();

    if posture.posture == AgentSchedulingPosture::Idle {
        if let Some(signal) = projection.work_reactivation_signal() {
            diagnostics.push(
                SchedulingAdvisory::warning(
                    "idle_posture_has_runnable_work",
                    "agent posture is idle while scheduler facts contain runnable work",
                )
                .work_item_id(signal.work_item_id)
                .evidence("posture=Idle")
                .evidence(format!("reactivation_mode={:?}", signal.reactivation_mode)),
            );
        } else if projection.queue_len > 0 {
            diagnostics.push(
                SchedulingAdvisory::warning(
                    "idle_posture_has_queued_input",
                    "agent posture is idle while scheduler facts contain queued input",
                )
                .evidence("posture=Idle")
                .evidence(format!("queue_len={}", projection.queue_len)),
            );
        }
    }

    for condition in wait_conditions.iter().filter(|condition| {
        condition.agent_id == agent.id && condition.status == WaitConditionStatus::Active
    }) {
        match condition.external_recoverability() {
            Some(ExternalWaitRecoverability::Weak) => {
                diagnostics.push(
                    SchedulingAdvisory::warning(
                        "external_wait_has_weak_recoverability",
                        "active external wait lacks a durable recovery path",
                    )
                    .wait_condition_id(condition.id.clone())
                    .maybe_work_item_id(condition.work_item_id.clone())
                    .evidence("external_recoverability=Weak")
                    .evidence(format!("wake_sources={:?}", condition.wake_sources)),
                );
            }
            Some(ExternalWaitRecoverability::ExplicitNoFallback) => {
                let mut diagnostic = SchedulingAdvisory::warning(
                    "external_wait_has_no_fallback",
                    "active external wait explicitly has no fallback recovery path",
                )
                .wait_condition_id(condition.id.clone())
                .maybe_work_item_id(condition.work_item_id.clone())
                .evidence("external_recoverability=ExplicitNoFallback")
                .evidence(format!("wake_sources={:?}", condition.wake_sources));
                if let Some(reason) = condition.no_fallback_reason() {
                    diagnostic = diagnostic.evidence(format!("no_fallback_reason={reason}"));
                }
                diagnostics.push(diagnostic);
            }
            Some(ExternalWaitRecoverability::Recoverable) | None => {}
        }
    }

    for item in work_queue.items.iter().filter(|item| {
        item.scheduling_state == WorkItemSchedulingState::Blocked
            && item.work_item.agent_id == agent.id
            && item.work_item.blocked_by.is_some()
            && item.work_item.recheck_at.is_none()
            && !item.has_active_waits
            && !item.has_active_task_waits
    }) {
        diagnostics.push(
            SchedulingAdvisory::warning(
                "blocked_work_item_without_recheck_or_wait",
                "blocked WorkItem has no recheck deadline or active wait condition",
            )
            .work_item_id(item.work_item.id.clone())
            .evidence("scheduling_state=Blocked")
            .evidence("blocked_by_present=true")
            .evidence("recheck_at=None")
            .evidence("has_active_waits=false"),
        );
    }

    diagnostics
}

pub(crate) fn scheduling_advisory_event(diagnostic: &SchedulingAdvisory) -> AuditEvent {
    AuditEvent::legacy(
        "scheduling_advisory",
        serde_json::json!({
            "kind": &diagnostic.kind,
            "severity": diagnostic.severity.as_str(),
            "message": &diagnostic.message,
            "work_item_id": &diagnostic.work_item_id,
            "wait_condition_id": &diagnostic.wait_condition_id,
            "evidence": &diagnostic.evidence,
        }),
    )
}

pub(crate) fn append_scheduling_advisories(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<usize> {
    let diagnostics = scheduling_advisories_with_queue_len(storage, agent, queue_len)?;
    let recent_events = storage.read_recent_events(64)?;
    let mut seen_data = Vec::new();
    let mut appended = 0;

    for diagnostic in diagnostics {
        let event = scheduling_advisory_event(&diagnostic);
        if seen_data.iter().any(|data| data == &event.data) {
            continue;
        }
        seen_data.push(event.data.clone());

        let duplicate = recent_events
            .iter()
            .any(|latest| latest.kind == event.kind && latest.data == event.data);
        if duplicate {
            continue;
        }
        storage.append_event(&event)?;
        appended += 1;
    }

    Ok(appended)
}

trait SchedulingAdvisoryExt {
    fn maybe_work_item_id(self, work_item_id: Option<String>) -> Self;
}

impl SchedulingAdvisoryExt for SchedulingAdvisory {
    fn maybe_work_item_id(mut self, work_item_id: Option<String>) -> Self {
        self.work_item_id = work_item_id;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulerDecisionKind {
    StartModelTurn,
    ReduceMessageOnly,
    EmitSystemTick,
    WaitForTask,
    WaitForExternalChange,
    WaitForTimer,
    WaitForOperator,
    Sleep,
    StayIdle,
    Stop,
    Noop,
}

impl SchedulerDecisionKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::StartModelTurn => "StartModelTurn",
            Self::ReduceMessageOnly => "ReduceMessageOnly",
            Self::EmitSystemTick => "EmitSystemTick",
            Self::WaitForTask => "WaitForTask",
            Self::WaitForExternalChange => "WaitForExternalChange",
            Self::WaitForTimer => "WaitForTimer",
            Self::WaitForOperator => "WaitForOperator",
            Self::Sleep => "Sleep",
            Self::StayIdle => "StayIdle",
            Self::Stop => "Stop",
            Self::Noop => "Noop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerDecision {
    pub kind: SchedulerDecisionKind,
    pub reason: String,
    pub model_reentry: bool,
    pub liveness_only: bool,
    pub message_id: Option<String>,
    pub work_item_id: Option<String>,
    pub task_id: Option<String>,
    pub boundary: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacyMessageAdmissionObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    input_kind: MessageKind,
    legacy_decision: &'static str,
    model_reentry: bool,
    continuation_class: Option<crate::types::ContinuationClass>,
    work_item_id: Option<String>,
    queue_len: usize,
    active_waiting_intents: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedMessageAdmissionCandidate {
    schema_version: u32,
    action: &'static str,
    binding_work_item_id: Option<String>,
    queue_disposition: &'static str,
    resulting_posture: &'static str,
    model_reentry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacyWorkQueueTickObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    reason: String,
    legacy_decision: &'static str,
    model_reentry: bool,
    work_item_id: String,
    work_item_revision: u64,
    queue_len: usize,
    active_waiting_intents: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedWorkQueueTickCandidate {
    schema_version: u32,
    action: &'static str,
    binding_work_item_id: String,
    binding_work_item_revision: u64,
    queue_disposition: &'static str,
    resulting_posture: &'static str,
    model_reentry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacyWaitResumeObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    input_kind: MessageKind,
    wake_source: String,
    resolved_wait_condition_ids: Vec<String>,
    legacy_decision: &'static str,
    model_reentry: bool,
    work_item_id: Option<String>,
    queue_len: usize,
    active_waiting_intents: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedWaitResumeCandidate {
    schema_version: u32,
    action: &'static str,
    consumed_wait_condition_ids: Vec<String>,
    binding_work_item_id: Option<String>,
    queue_disposition: &'static str,
    resulting_posture: &'static str,
    model_reentry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacySettlementObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    settlement_status: &'static str,
    queue_len: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedSettlementCandidate {
    schema_version: u32,
    action: &'static str,
    queue_disposition: &'static str,
    settlement_disposition: &'static str,
    resulting_posture: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacyDeliveryObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    turn_terminal: &'static str,
    queue_len: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedDeliveryCandidate {
    schema_version: u32,
    action: &'static str,
    delivery_disposition: &'static str,
    resulting_posture: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LegacyOperatorInterjectionObservation {
    schema_version: u32,
    boundary: &'static str,
    input_identity: String,
    interjection_boundary: &'static str,
    queue_len: usize,
    turn_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestrictedOperatorInterjectionCandidate {
    schema_version: u32,
    action: &'static str,
    interjection_boundary: &'static str,
    queue_disposition: &'static str,
    resulting_posture: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(crate) enum LegacySchedulerObservation {
    MessageAdmission(LegacyMessageAdmissionObservation),
    WorkQueueTick(LegacyWorkQueueTickObservation),
    WaitResume(LegacyWaitResumeObservation),
    Settlement(LegacySettlementObservation),
    Delivery(LegacyDeliveryObservation),
    OperatorInterjection(LegacyOperatorInterjectionObservation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(crate) enum RestrictedSchedulerCandidate {
    MessageAdmission(RestrictedMessageAdmissionCandidate),
    WorkQueueTick(RestrictedWorkQueueTickCandidate),
    WaitResume(RestrictedWaitResumeCandidate),
    Settlement(RestrictedSettlementCandidate),
    Delivery(RestrictedDeliveryCandidate),
    OperatorInterjection(RestrictedOperatorInterjectionCandidate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerShadowComparison {
    pub scenario_class: &'static str,
    pub comparison_identity: String,
    pub boundary: &'static str,
    pub input_identity: String,
    pub legacy_observation: LegacySchedulerObservation,
    pub shadow_candidate: RestrictedSchedulerCandidate,
    pub matched: bool,
    pub divergence_code: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub(crate) struct SchedulerSemanticShadowDecision {
    pub input: crate::domain::scheduler_semantic::SemanticDecisionInput,
    pub provider: SemanticProposalProviderConfig,
    pub response: SemanticProposalResponse,
    pub policy: SemanticValidationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerBoundary {
    RunLoop,
    RunLoopIdle,
    LifecycleSleep,
    MessageProcessing,
    IdleTick,
}

impl SchedulerBoundary {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RunLoop => "run_loop",
            Self::RunLoopIdle => "run_loop_idle",
            Self::LifecycleSleep => "lifecycle_sleep",
            Self::MessageProcessing => "message_processing",
            Self::IdleTick => "idle_tick",
        }
    }
}

/// Typed boundary for operator interjection drainage within a turn.
/// Replaces the previous single string-labeled drain path so each boundary
/// gets its own shadow comparison facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterjectionBoundary {
    AfterProviderRound,
    BeforeToolExecution,
    AfterToolResults,
    BeforeProviderContinuation,
}

impl InterjectionBoundary {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AfterProviderRound => "after_provider_round",
            Self::BeforeToolExecution => "before_tool_execution",
            Self::AfterToolResults => "after_tool_results",
            Self::BeforeProviderContinuation => "before_provider_continuation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulerDuplicateEvidence {
    ContinueActiveBrief(String),
    QueuedAvailableMessage(String),
    WakeHintMessage(String),
}

#[derive(Debug, Clone)]
pub(crate) enum SchedulerIdleSignal<'a> {
    ContinueActive {
        work_item: &'a WorkItemRecord,
        suppressed_after_model_reentry_continuation: bool,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
    QueuedAvailable {
        work_item: &'a WorkItemRecord,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
    WakeHint {
        pending: &'a PendingWakeHint,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
}

pub(crate) enum SchedulerInput<'a> {
    Idle,
    Message {
        message: &'a MessageEnvelope,
        model_turn_allowed: bool,
        continuation_resolution: Option<&'a ContinuationResolution>,
    },
    IdleSignal(SchedulerIdleSignal<'a>),
}

impl SchedulerDecision {
    pub(crate) fn new(kind: SchedulerDecisionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
            model_reentry: false,
            liveness_only: false,
            message_id: None,
            work_item_id: None,
            task_id: None,
            boundary: None,
            evidence: Vec::new(),
        }
    }

    pub(crate) fn model_reentry(mut self, value: bool) -> Self {
        self.model_reentry = value;
        self
    }

    pub(crate) fn liveness_only(mut self, value: bool) -> Self {
        self.liveness_only = value;
        self
    }

    pub(crate) fn message(mut self, message: &MessageEnvelope) -> Self {
        self.message_id = Some(message.id.clone());
        self.work_item_id = message.work_item_id.clone();
        self.task_id = message.task_id.clone();
        self
    }

    pub(crate) fn work_item_id(mut self, work_item_id: impl Into<String>) -> Self {
        self.work_item_id = Some(work_item_id.into());
        self
    }
    pub(crate) fn boundary(mut self, boundary: impl Into<String>) -> Self {
        self.boundary = Some(boundary.into());
        self
    }

    pub(crate) fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }
}

pub(crate) fn decide_next_action(
    projection: &SchedulerProjection,
    boundary: SchedulerBoundary,
    input: SchedulerInput<'_>,
) -> SchedulerDecision {
    let boundary_label = boundary.as_str();
    if matches!(projection.status, AgentStatus::Stopped) {
        return SchedulerDecision::new(SchedulerDecisionKind::Stop, "stopped")
            .boundary(boundary_label)
            .liveness_only(true)
            .evidence(format!("status={:?}", projection.status));
    }

    match input {
        SchedulerInput::Message {
            message,
            model_turn_allowed,
            continuation_resolution,
        } => message_processing_decision(message, model_turn_allowed, continuation_resolution)
            .boundary(boundary_label)
            .evidence(format!("queue_len={}", projection.queue_len))
            .evidence(format!("turn_in_progress={}", projection.turn_in_progress)),
        SchedulerInput::IdleSignal(signal) => {
            decide_idle_signal_action(projection, boundary_label, signal)
        }
        SchedulerInput::Idle => idle_boundary_decision(projection, boundary_label),
    }
}

fn decide_idle_signal_action(
    projection: &SchedulerProjection,
    boundary: &'static str,
    signal: SchedulerIdleSignal<'_>,
) -> SchedulerDecision {
    if projection.turn_in_progress {
        return SchedulerDecision::new(SchedulerDecisionKind::Noop, "turn_in_progress")
            .boundary(boundary)
            .liveness_only(true)
            .evidence(format!("active_run_id={:?}", projection.active_run_id));
    }

    match signal {
        SchedulerIdleSignal::WakeHint { pending, duplicate } => {
            if let Some(SchedulerDuplicateEvidence::WakeHintMessage(message_id)) = duplicate {
                return SchedulerDecision::new(SchedulerDecisionKind::Noop, "duplicate_wake_hint")
                    .boundary(boundary)
                    .liveness_only(true)
                    .evidence("duplicate_wake_hint_suppressed")
                    .evidence(format!("message_id={message_id}"))
                    .evidence(format!(
                        "idempotency_key={}",
                        wake_hint_idempotency_key(pending)
                    ));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "wake_hint")
                .boundary(boundary)
                .model_reentry(true)
                .evidence("runtime_idle")
                .evidence("pending_wake_hint")
                .evidence(format!(
                    "idempotency_key={}",
                    wake_hint_idempotency_key(pending)
                ))
        }
        SchedulerIdleSignal::ContinueActive {
            work_item,
            suppressed_after_model_reentry_continuation,
            duplicate,
        } => {
            if let Some(decision) = wait_decision_for_projection(projection) {
                return decision
                    .boundary(boundary)
                    .evidence("work_queue_tick_blocked_by_wait_fact");
            }
            if suppressed_after_model_reentry_continuation {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "continue_active_suppressed_after_model_reentry_continuation",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("model_reentry_continuation_suppresses_duplicate_continue_active");
            }
            if let Some(SchedulerDuplicateEvidence::ContinueActiveBrief(result_brief_id)) =
                duplicate
            {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "duplicate_continue_active",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("duplicate_tick_suppressed")
                .evidence(format!("result_brief_id={result_brief_id}"));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "continue_active")
                .boundary(boundary)
                .model_reentry(true)
                .work_item_id(work_item.id.clone())
                .evidence("runtime_idle")
                .evidence("work_item_runnable")
                .evidence(format!(
                    "idempotency_key={}",
                    work_queue_tick_idempotency_key(work_item, "continue_active")
                ))
        }
        SchedulerIdleSignal::QueuedAvailable {
            work_item,
            duplicate,
        } => {
            if let Some(decision) = wait_decision_for_projection(projection) {
                return decision
                    .boundary(boundary)
                    .evidence("work_queue_tick_blocked_by_wait_fact");
            }
            if let Some(SchedulerDuplicateEvidence::QueuedAvailableMessage(message_id)) = duplicate
            {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "duplicate_queued_available",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("duplicate_tick_suppressed")
                .evidence(format!("message_id={message_id}"));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "queued_available")
                .boundary(boundary)
                .model_reentry(true)
                .work_item_id(work_item.id.clone())
                .evidence("runtime_idle")
                .evidence("work_item_runnable")
                .evidence(format!(
                    "idempotency_key={}",
                    work_queue_tick_idempotency_key(work_item, "queued_available")
                ))
        }
    }
}

pub(crate) fn scheduler_decision_event(decision: &SchedulerDecision) -> AuditEvent {
    AuditEvent::legacy(
        "scheduler_decision",
        serde_json::json!({
            "decision": decision.kind.as_str(),
            "reason": &decision.reason,
            "model_reentry": decision.model_reentry,
            "liveness_only": decision.liveness_only,
            "message_id": &decision.message_id,
            "work_item_id": &decision.work_item_id,
            "task_id": &decision.task_id,
            "boundary": &decision.boundary,
            "evidence": &decision.evidence,
        }),
    )
}

pub(crate) fn append_scheduler_decision(
    storage: &AppStorage,
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<bool> {
    let event = scheduler_decision_event(decision);
    // Also emit the typed public diagnostic event alongside the legacy record.
    let _ = append_scheduler_diagnostic_event(storage, agent_id, decision, None)?;
    let duplicate = storage
        .read_recent_events(32)?
        .into_iter()
        .rev()
        .find(|latest| latest.kind == event.kind)
        .is_some_and(|latest| latest.data == event.data);
    if duplicate {
        return Ok(false);
    }
    storage.append_event(&event)?;
    Ok(true)
}

/// Construct a public `SchedulerDiagnosticAuditEvent` from a scheduler decision
/// and an optional shadow comparison. When a shadow comparison is present, the
/// event carries the scenario class, match status, and divergence code so the
/// diagnostic stream exposes both the decision and its protocol-path audit
/// outcome in one record.
pub(crate) fn scheduler_diagnostic_audit_event(
    agent_id: &str,
    decision: &SchedulerDecision,
    shadow: Option<&SchedulerShadowComparison>,
) -> crate::types::SchedulerDiagnosticAuditEvent {
    let (scenario_class, shadow_matched, divergence_code) = shadow
        .map(|sc| {
            (
                Some(sc.scenario_class.to_string()),
                Some(sc.matched),
                sc.divergence_code.map(|c| c.to_string()),
            )
        })
        .unwrap_or((None, None, None));

    crate::types::SchedulerDiagnosticAuditEvent {
        agent_id: agent_id.to_string(),
        decision: decision.kind.as_str().to_string(),
        reason: decision.reason.clone(),
        boundary: decision.boundary.clone(),
        scenario_class,
        shadow_matched,
        divergence_code,
        work_item_id: decision.work_item_id.clone(),
        message_id: decision.message_id.clone(),
        task_id: decision.task_id.clone(),
        evidence: decision.evidence.clone(),
    }
}

/// Emit a typed scheduler diagnostic event alongside the legacy audit record.
/// The legacy `scheduler_decision` event is retained for backward compatibility;
/// the typed event extends observability into the public event stream.
pub(crate) fn append_scheduler_diagnostic_event(
    storage: &AppStorage,
    agent_id: &str,
    decision: &SchedulerDecision,
    shadow: Option<&SchedulerShadowComparison>,
) -> Result<bool> {
    let payload = scheduler_diagnostic_audit_event(agent_id, decision, shadow);
    let event = crate::types::AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic,
        &payload,
    )?;
    let duplicate = storage
        .read_recent_events(32)?
        .into_iter()
        .rev()
        .find(|latest| latest.kind == event.kind)
        .is_some_and(|latest| latest.data == event.data);
    if duplicate {
        return Ok(false);
    }
    storage.append_event(&event)?;
    Ok(true)
}

pub(crate) fn message_processing_decision(
    message: &MessageEnvelope,
    model_turn_allowed: bool,
    continuation_resolution: Option<&ContinuationResolution>,
) -> SchedulerDecision {
    let model_reentry = model_turn_allowed
        && continuation_resolution.is_some_and(|resolution| resolution.model_reentry);
    let kind = if model_reentry {
        SchedulerDecisionKind::StartModelTurn
    } else {
        SchedulerDecisionKind::ReduceMessageOnly
    };
    let mut decision = SchedulerDecision::new(kind, format!("{:?}", message.kind))
        .message(message)
        .model_reentry(model_reentry)
        .liveness_only(!model_reentry)
        .evidence(format!("message_kind={:?}", message.kind))
        .evidence(format!("trigger_kind={:?}", message.trigger_kind));
    if !model_turn_allowed {
        decision = decision.evidence("model_turn_blocked_by_control_posture");
    }
    decision
}

pub(crate) fn shadow_comparison_for_message_admission(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    decision: &SchedulerDecision,
    continuation_resolution: Option<&ContinuationResolution>,
) -> Option<SchedulerShadowComparison> {
    if !matches!(
        continuation_resolution.map(|resolution| resolution.class),
        None | Some(
            crate::types::ContinuationClass::LocalContinuation
                | crate::types::ContinuationClass::LivenessOnly
        )
    ) || matches!(
        message.kind,
        MessageKind::OperatorPrompt | MessageKind::TaskResult | MessageKind::SystemTick
    ) {
        return None;
    }

    let input_identity = format!("message:{}", message.id);
    let observation =
        LegacySchedulerObservation::MessageAdmission(LegacyMessageAdmissionObservation {
            schema_version: 1,
            boundary: SchedulerBoundary::RunLoop.as_str(),
            input_identity: input_identity.clone(),
            input_kind: message.kind.clone(),
            legacy_decision: decision.kind.as_str(),
            model_reentry: decision.model_reentry,
            continuation_class: continuation_resolution.map(|resolution| resolution.class),
            work_item_id: decision.work_item_id.clone(),
            queue_len: projection.queue_len,
            active_waiting_intents: projection.active_waiting_intents,
            turn_in_progress: projection.turn_in_progress,
        });
    let candidate = RestrictedSchedulerCandidate::MessageAdmission(
        restricted_message_admission_candidate(projection, message),
    );
    let matched = match (&observation, &candidate) {
        (
            LegacySchedulerObservation::MessageAdmission(observation),
            RestrictedSchedulerCandidate::MessageAdmission(candidate),
        ) => {
            observation.legacy_decision
                == if candidate.model_reentry {
                    SchedulerDecisionKind::StartModelTurn.as_str()
                } else {
                    SchedulerDecisionKind::ReduceMessageOnly.as_str()
                }
                && observation.model_reentry == candidate.model_reentry
                && observation.work_item_id == candidate.binding_work_item_id
        }
        _ => unreachable!(),
    };

    Some(SchedulerShadowComparison {
        scenario_class: REDUCER_ONLY_CANDIDATES_SCENARIO,
        comparison_identity: format!("message_admission:{}", message.id),
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("message_admission_outcome_mismatch"),
    })
}
pub(crate) fn shadow_comparison_for_wait_resume(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    decision: &SchedulerDecision,
) -> Option<SchedulerShadowComparison> {
    if !matches!(
        message.kind,
        MessageKind::TaskResult | MessageKind::SystemTick
    ) {
        return None;
    }
    let matching_waits: Vec<&WaitConditionRecord> = projection
        .semantic_waits
        .iter()
        .filter(|condition| {
            condition.status == WaitConditionStatus::Active
                && message_matches_wait_condition(message, condition)
        })
        .collect();
    if matching_waits.is_empty() {
        return None;
    }
    let resolved_wait_condition_ids: Vec<String> = matching_waits
        .iter()
        .map(|condition| condition.id.clone())
        .collect();
    let binding_work_item_id = matching_waits
        .iter()
        .filter_map(|condition| condition.work_item_id.clone())
        .next();
    let wake_source = match message.kind {
        MessageKind::TaskResult => "task_result",
        MessageKind::SystemTick => "system_tick",
        _ => "unknown",
    }
    .to_string();
    let input_identity = format!("message:{}", message.id);
    let candidate_model_reentry = restricted_wait_resume_model_reentry(projection);
    let observation = LegacySchedulerObservation::WaitResume(LegacyWaitResumeObservation {
        schema_version: 1,
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity: input_identity.clone(),
        input_kind: message.kind.clone(),
        wake_source,
        resolved_wait_condition_ids: resolved_wait_condition_ids.clone(),
        legacy_decision: decision.kind.as_str(),
        model_reentry: decision.model_reentry,
        work_item_id: decision.work_item_id.clone(),
        queue_len: projection.queue_len,
        active_waiting_intents: projection.active_waiting_intents,
        turn_in_progress: projection.turn_in_progress,
    });
    let candidate = RestrictedSchedulerCandidate::WaitResume(RestrictedWaitResumeCandidate {
        schema_version: 1,
        action: "wait_resume",
        consumed_wait_condition_ids: resolved_wait_condition_ids,
        binding_work_item_id: binding_work_item_id.clone(),
        queue_disposition: "claim",
        resulting_posture: "running",
        model_reentry: candidate_model_reentry,
    });
    let matched = match (&observation, &candidate) {
        (
            LegacySchedulerObservation::WaitResume(obs),
            RestrictedSchedulerCandidate::WaitResume(cand),
        ) => {
            obs.model_reentry == cand.model_reentry && obs.work_item_id == cand.binding_work_item_id
        }
        _ => unreachable!(),
    };
    Some(SchedulerShadowComparison {
        scenario_class: WAIT_RESUME_SCENARIO,
        comparison_identity: format!("wait_resume:{}", message.id),
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("wait_resume_outcome_mismatch"),
    })
}

fn message_matches_wait_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> bool {
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => {
            condition.wake_sources.iter().any(
                |source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id),
            )
        }
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if subsystem == "work_queue" {
                return false;
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
        }
        _ => false,
    }
}

fn restricted_wait_resume_model_reentry(projection: &SchedulerProjection) -> bool {
    if matches!(projection.status, AgentStatus::Stopped) {
        return false;
    }
    !projection.turn_in_progress
}

pub(crate) fn shadow_comparison_for_settlement(
    projection: &SchedulerProjection,
    record: &QueueEntryRecord,
) -> Option<SchedulerShadowComparison> {
    let legacy_status = match record.status {
        QueueEntryStatus::Processed => "complete",
        QueueEntryStatus::Aborted => "failed",
        QueueEntryStatus::Interrupted => "interrupted",
        QueueEntryStatus::Interjected => "interjected",
        QueueEntryStatus::Dropped => "dropped",
        QueueEntryStatus::Queued | QueueEntryStatus::Dequeued => return None,
    };
    let candidate_disposition = restricted_settlement_disposition(projection);
    let input_identity = format!("message:{}", record.message_id);
    let observation = LegacySchedulerObservation::Settlement(LegacySettlementObservation {
        schema_version: 1,
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity: input_identity.clone(),
        settlement_status: legacy_status,
        queue_len: projection.queue_len,
        turn_in_progress: projection.turn_in_progress,
    });
    let candidate = RestrictedSchedulerCandidate::Settlement(RestrictedSettlementCandidate {
        schema_version: 1,
        action: "settle",
        queue_disposition: "settle",
        settlement_disposition: candidate_disposition,
        resulting_posture: "open",
    });
    let matched = legacy_status == candidate_disposition;
    Some(SchedulerShadowComparison {
        scenario_class: SETTLEMENT_SCENARIO,
        comparison_identity: format!("settlement:{}", record.message_id),
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("settlement_outcome_mismatch"),
    })
}

fn restricted_settlement_disposition(projection: &SchedulerProjection) -> &'static str {
    if matches!(projection.status, AgentStatus::Stopped) {
        return "failed";
    }
    if projection.turn_in_progress {
        return "pending";
    }
    "complete"
}

pub(crate) fn shadow_comparison_for_delivery(
    projection: &SchedulerProjection,
    record: &QueueEntryRecord,
) -> Option<SchedulerShadowComparison> {
    // Only produce a delivery comparison for terminal settlement statuses;
    // Queued/Dequeued are not settled and have no delivery to compare.
    if !matches!(
        record.status,
        QueueEntryStatus::Processed
            | QueueEntryStatus::Aborted
            | QueueEntryStatus::Interrupted
            | QueueEntryStatus::Interjected
            | QueueEntryStatus::Dropped
    ) {
        return None;
    }
    let legacy_terminal = projection
        .last_turn_terminal
        .map(|kind| match kind {
            TurnTerminalKind::Completed => "completed",
            TurnTerminalKind::Aborted => "aborted",
            TurnTerminalKind::BaselineOverBudget => "baseline_over_budget",
            TurnTerminalKind::DeferredToFallback => "deferred_to_fallback",
            TurnTerminalKind::ProviderFailedNeedsRecovery => "provider_failed_needs_recovery",
        })
        .unwrap_or("none");
    let candidate_disposition = restricted_delivery_disposition(projection, record);
    let input_identity = format!("message:{}", record.message_id);
    let observation = LegacySchedulerObservation::Delivery(LegacyDeliveryObservation {
        schema_version: 1,
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity: input_identity.clone(),
        turn_terminal: legacy_terminal,
        queue_len: projection.queue_len,
        turn_in_progress: projection.turn_in_progress,
    });
    let candidate = RestrictedSchedulerCandidate::Delivery(RestrictedDeliveryCandidate {
        schema_version: 1,
        action: "deliver",
        delivery_disposition: candidate_disposition,
        resulting_posture: "open",
    });
    let legacy_category = turn_terminal_delivery_category(projection.last_turn_terminal);
    let matched = legacy_category == candidate_disposition;
    Some(SchedulerShadowComparison {
        scenario_class: DELIVERY_SCENARIO,
        comparison_identity: format!("delivery:{}", record.message_id),
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("delivery_outcome_mismatch"),
    })
}

fn turn_terminal_delivery_category(kind: Option<TurnTerminalKind>) -> &'static str {
    match kind {
        Some(TurnTerminalKind::Completed) => "completed",
        Some(
            TurnTerminalKind::Aborted
            | TurnTerminalKind::ProviderFailedNeedsRecovery
            | TurnTerminalKind::BaselineOverBudget,
        ) => "failed",
        Some(TurnTerminalKind::DeferredToFallback) => "pending",
        None => "none",
    }
}

pub(crate) fn shadow_comparison_for_operator_interjection(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    boundary: InterjectionBoundary,
) -> Option<SchedulerShadowComparison> {
    // Operator interjections are always admitted at the boundary where they
    // were drained. The shadow comparison records the boundary, queue state,
    // and turn status so divergences between legacy and typed paths are
    // auditable per-boundary rather than through a single opaque drain.
    let boundary_str = boundary.as_str();
    let input_identity = format!("message:{}", message.id);
    let observation =
        LegacySchedulerObservation::OperatorInterjection(LegacyOperatorInterjectionObservation {
            schema_version: 1,
            boundary: SchedulerBoundary::RunLoop.as_str(),
            input_identity: input_identity.clone(),
            interjection_boundary: boundary_str,
            queue_len: projection.queue_len,
            turn_in_progress: projection.turn_in_progress,
        });
    let candidate = RestrictedSchedulerCandidate::OperatorInterjection(
        RestrictedOperatorInterjectionCandidate {
            schema_version: 1,
            action: "interject",
            interjection_boundary: boundary_str,
            queue_disposition: "consumed",
            resulting_posture: "running",
        },
    );
    // Both legacy and typed paths admit operator interjections at the same
    // boundary during turn execution, so they always match in shadow mode.
    let matched = true;
    Some(SchedulerShadowComparison {
        scenario_class: INTERJECTION_SCENARIO,
        comparison_identity: format!("operator_interjection:{}", message.id),
        boundary: SchedulerBoundary::RunLoop.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("operator_interjection_outcome_mismatch"),
    })
}

fn restricted_delivery_disposition(
    projection: &SchedulerProjection,
    record: &QueueEntryRecord,
) -> &'static str {
    match record.status {
        QueueEntryStatus::Aborted | QueueEntryStatus::Dropped => "failed",
        QueueEntryStatus::Interrupted | QueueEntryStatus::Interjected => "interrupted",
        QueueEntryStatus::Processed => {
            if matches!(projection.status, AgentStatus::Stopped) {
                "failed"
            } else if projection.turn_in_progress {
                "pending"
            } else {
                "completed"
            }
        }
        QueueEntryStatus::Queued | QueueEntryStatus::Dequeued => "none",
    }
}

pub(crate) fn semantic_shadow_decision_for_message_admission(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
) -> Result<Option<SchedulerSemanticShadowDecision>> {
    if message.kind != MessageKind::OperatorPrompt {
        return Ok(None);
    }

    let ingress = match TrustedSemanticIngress::from_persisted_message(message) {
        Ok(ingress) => ingress,
        Err(_) => return Ok(None),
    };
    let waits = projection
        .semantic_waits
        .iter()
        .filter_map(|wait| {
            let owner_work_item_id = wait.work_item_id.clone()?;
            Some(SemanticWaitCandidate {
                wait_id: wait.id.clone(),
                agent_id: wait.agent_id.clone(),
                generation: semantic_wait_generation(wait),
                state: SemanticWaitCandidateState::Active,
                owner_work_item_id,
                summary: wait.waiting_for.clone(),
                routing_keys: semantic_routing_keys(&wait.id, wait.subject_ref.as_deref()),
            })
        })
        .collect();
    let work_items = projection
        .semantic_work_items
        .iter()
        .map(|work_item| SemanticWorkItemCandidate {
            work_item_id: work_item.id.clone(),
            agent_id: work_item.agent_id.clone(),
            revision: work_item.revision,
            state: if work_item.state == WorkItemState::Completed {
                SemanticWorkItemCandidateState::Terminal
            } else if work_item.scheduling_state == WorkItemSchedulingState::Runnable {
                SemanticWorkItemCandidateState::Runnable
            } else {
                SemanticWorkItemCandidateState::Waiting
            },
            summary: work_item.objective.clone(),
            routing_keys: vec![work_item.id.clone()],
        })
        .collect();
    let input = ingress.decision_input(waits, work_items);
    let proposal = structural_semantic_proposal(&input);

    Ok(Some(SchedulerSemanticShadowDecision {
        input,
        provider: SemanticProposalProviderConfig {
            identity: SemanticProposalProviderIdentity {
                provider_id: "runtime-structural-shadow".into(),
                model_ref: "builtin/structural-semantic-proposal".into(),
                contract_revision: SEMANTIC_CONTRACT_REVISION,
            },
        },
        response: SemanticProposalResponse {
            proposal,
            confidence_bps: crate::domain::scheduler_semantic::MAX_CONFIDENCE_BPS,
            latency_ms: None,
        },
        policy: SemanticValidationPolicy::default(),
    }))
}

fn semantic_wait_generation(wait: &WaitConditionRecord) -> u64 {
    u64::try_from(wait.updated_at.timestamp_micros())
        .unwrap_or(1)
        .max(1)
}

fn semantic_routing_keys(id: &str, subject_ref: Option<&str>) -> Vec<String> {
    let mut keys = vec![id.to_string()];
    if let Some(subject_ref) = subject_ref
        .map(str::trim)
        .filter(|subject_ref| !subject_ref.is_empty() && *subject_ref != id)
    {
        keys.push(subject_ref.to_string());
    }
    keys
}

fn restricted_message_admission_candidate(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
) -> RestrictedMessageAdmissionCandidate {
    let model_reentry = restricted_message_model_reentry(projection, message);
    RestrictedMessageAdmissionCandidate {
        schema_version: 1,
        action: if model_reentry {
            "admit_model_turn"
        } else {
            "reduce_message_only"
        },
        binding_work_item_id: message.work_item_id.clone(),
        queue_disposition: "claim",
        resulting_posture: "running",
        model_reentry,
    }
}

fn restricted_message_model_reentry(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
) -> bool {
    if matches!(projection.status, AgentStatus::Stopped) {
        return false;
    }
    let Some(trigger_kind) = restricted_trigger_kind(message) else {
        return false;
    };
    let contentful = restricted_message_is_contentful(message);
    let task_terminal = matches!(message.kind, MessageKind::TaskResult)
        && message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_status"))
            .and_then(serde_json::Value::as_str)
            .is_none_or(|status| {
                matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
            });
    let task_work_item_id = message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("work_item_id"))
        .and_then(serde_json::Value::as_str)
        .or(message.work_item_id.as_deref());
    let same_work_item = task_work_item_id
        == projection
            .current_work_item
            .as_ref()
            .map(|work_item| work_item.id.as_str());

    let Some(waiting_reason) = restricted_waiting_reason(projection) else {
        return (trigger_kind == crate::types::ContinuationTriggerKind::TaskResult
            && task_terminal
            && same_work_item)
            || matches!(
                trigger_kind,
                crate::types::ContinuationTriggerKind::OperatorInput
                    | crate::types::ContinuationTriggerKind::TimerFire
                    | crate::types::ContinuationTriggerKind::InternalFollowup
            )
            || (matches!(
                trigger_kind,
                crate::types::ContinuationTriggerKind::ExternalEvent
                    | crate::types::ContinuationTriggerKind::SystemTick
            ) && contentful);
    };

    let expected = matches!(
        (waiting_reason, trigger_kind),
        (
            Some(crate::types::WaitingReason::AwaitingOperatorInput),
            crate::types::ContinuationTriggerKind::OperatorInput
        ) | (
            Some(crate::types::WaitingReason::AwaitingTaskResult),
            crate::types::ContinuationTriggerKind::TaskResult
        ) | (
            Some(crate::types::WaitingReason::AwaitingExternalChange),
            crate::types::ContinuationTriggerKind::ExternalEvent
                | crate::types::ContinuationTriggerKind::SystemTick
        ) | (
            Some(crate::types::WaitingReason::AwaitingTimer),
            crate::types::ContinuationTriggerKind::TimerFire
        )
    );
    if expected {
        return match trigger_kind {
            crate::types::ContinuationTriggerKind::TaskResult => task_terminal && same_work_item,
            crate::types::ContinuationTriggerKind::ExternalEvent
            | crate::types::ContinuationTriggerKind::SystemTick => contentful,
            _ => true,
        };
    }
    trigger_kind == crate::types::ContinuationTriggerKind::OperatorInput
        || (trigger_kind == crate::types::ContinuationTriggerKind::TaskResult
            && task_terminal
            && same_work_item)
}

fn restricted_waiting_reason(
    projection: &SchedulerProjection,
) -> Option<Option<crate::types::WaitingReason>> {
    if projection.runtime_error || projection.work_reactivation_signal().is_some() {
        return None;
    }
    if projection.turn_in_progress {
        return Some(Some(crate::types::WaitingReason::AwaitingExternalChange));
    }
    if projection.current_work_item_waits_for_operator() {
        return Some(Some(crate::types::WaitingReason::AwaitingOperatorInput));
    }
    if projection.active_agent_waiting_intents > 0 {
        return Some(Some(crate::types::WaitingReason::AwaitingExternalChange));
    }
    if projection.active_timers > 0 {
        return Some(Some(crate::types::WaitingReason::AwaitingTimer));
    }
    match projection.waiting_work_item_scheduling_state {
        Some(WorkItemSchedulingState::WaitingTask) => {
            Some(Some(crate::types::WaitingReason::AwaitingTaskResult))
        }
        Some(WorkItemSchedulingState::WaitingExternal) => {
            Some(Some(crate::types::WaitingReason::AwaitingExternalChange))
        }
        Some(WorkItemSchedulingState::WaitingTimer) => {
            Some(Some(crate::types::WaitingReason::AwaitingTimer))
        }
        Some(WorkItemSchedulingState::WaitingOperator) => {
            Some(Some(crate::types::WaitingReason::AwaitingOperatorInput))
        }
        Some(WorkItemSchedulingState::WaitingSystem) => Some(None),
        _ => None,
    }
}

fn restricted_trigger_kind(
    message: &MessageEnvelope,
) -> Option<crate::types::ContinuationTriggerKind> {
    match message.kind {
        MessageKind::TaskStatus
        | MessageKind::Control
        | MessageKind::BriefAck
        | MessageKind::BriefResult => None,
        _ => Some(crate::types::admission_trigger_kind_for_message_kind(
            &message.kind,
        )),
    }
}

fn restricted_message_is_contentful(message: &MessageEnvelope) -> bool {
    let body_is_contentful = |body: &crate::types::MessageBody| match body {
        crate::types::MessageBody::Text { text } => !text.trim().is_empty(),
        crate::types::MessageBody::Json { .. } => true,
        crate::types::MessageBody::Brief { text, .. } => !text.trim().is_empty(),
    };
    if matches!(message.kind, MessageKind::SystemTick) {
        if let Some(wake_hint) = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("wake_hint"))
        {
            return wake_hint
                .get("body")
                .cloned()
                .and_then(|body| serde_json::from_value(body).ok())
                .is_some_and(|body| body_is_contentful(&body));
        }
    }
    body_is_contentful(&message.body)
}

pub(crate) fn shadow_comparison_for_work_queue_tick(
    projection: &SchedulerProjection,
    work_item: &WorkItemRecord,
    reason: &str,
    decision: &SchedulerDecision,
    boundary: SchedulerBoundary,
) -> Option<SchedulerShadowComparison> {
    if !matches!(reason, "continue_active" | "queued_available") {
        return None;
    }
    let idempotency_key = work_queue_tick_idempotency_key(work_item, reason);
    let input_identity = format!("work_queue_tick:{idempotency_key}");
    let observation = LegacySchedulerObservation::WorkQueueTick(LegacyWorkQueueTickObservation {
        schema_version: 1,
        boundary: boundary.as_str(),
        input_identity: input_identity.clone(),
        reason: reason.into(),
        legacy_decision: decision.kind.as_str(),
        model_reentry: decision.model_reentry,
        work_item_id: work_item.id.clone(),
        work_item_revision: work_item.revision,
        queue_len: projection.queue_len,
        active_waiting_intents: projection.active_waiting_intents,
        turn_in_progress: projection.turn_in_progress,
    });
    let candidate = RestrictedSchedulerCandidate::WorkQueueTick(RestrictedWorkQueueTickCandidate {
        schema_version: 1,
        action: "emit_work_queue_tick",
        binding_work_item_id: work_item.id.clone(),
        binding_work_item_revision: work_item.revision,
        queue_disposition: "enqueue",
        resulting_posture: "awake",
        model_reentry: true,
    });
    let matched = matches!(decision.kind, SchedulerDecisionKind::EmitSystemTick)
        && decision.model_reentry
        && decision.work_item_id.as_deref() == Some(work_item.id.as_str());

    Some(SchedulerShadowComparison {
        scenario_class: WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO,
        comparison_identity: format!("work_queue_idle_tick:{idempotency_key}"),
        boundary: boundary.as_str(),
        input_identity,
        legacy_observation: observation,
        shadow_candidate: candidate,
        matched,
        divergence_code: (!matched).then_some("work_queue_tick_outcome_mismatch"),
    })
}

pub(crate) fn idle_noop_decision(projection: &SchedulerProjection) -> SchedulerDecision {
    let (kind, reason) = if matches!(projection.status, AgentStatus::Stopped) {
        (SchedulerDecisionKind::Stop, "stopped")
    } else if projection.queue_len > 0 {
        (SchedulerDecisionKind::Noop, "queue_not_empty")
    } else if projection.turn_in_progress {
        (SchedulerDecisionKind::Noop, "turn_in_progress")
    } else if matches!(projection.status, AgentStatus::Asleep) {
        (SchedulerDecisionKind::StayIdle, "already_asleep")
    } else {
        (SchedulerDecisionKind::Sleep, "no_pending_scheduler_facts")
    };
    SchedulerDecision::new(kind, reason)
        .liveness_only(true)
        .evidence(format!("status={:?}", projection.status))
        .evidence(format!("queue_len={}", projection.queue_len))
}

pub(crate) fn wait_decision_for_projection(
    projection: &SchedulerProjection,
) -> Option<SchedulerDecision> {
    if projection.work_reactivation_signal().is_some() {
        return None;
    }
    if projection.active_agent_waiting_intents > 0 {
        return Some(
            SchedulerDecision::new(
                SchedulerDecisionKind::WaitForExternalChange,
                "active_agent_waiting_intents",
            )
            .liveness_only(true)
            .evidence(format!(
                "active_waiting_intents={}",
                projection.active_waiting_intents
            ))
            .evidence(format!(
                "active_agent_waiting_intents={}",
                projection.active_agent_waiting_intents
            )),
        );
    }
    if projection.active_timers > 0 {
        return Some(
            SchedulerDecision::new(SchedulerDecisionKind::WaitForTimer, "active_timers")
                .liveness_only(true)
                .evidence(format!("active_timers={}", projection.active_timers)),
        );
    }
    projection.waiting_work_item.as_ref().and_then(|item| {
        match projection.waiting_work_item_scheduling_state {
            Some(WorkItemSchedulingState::WaitingOperator) => {
                // If recheck_at has expired and not been consumed, do not block on
                // WaitForOperator — let the agent wake up to re-evaluate. This
                // prevents permanent stalls when wake=operator_input is used with
                // a recheck_after_ms fallback. (#1989)
                if item
                    .recheck_at
                    .is_some_and(|recheck_at| recheck_at <= projection.now)
                    && item
                        .recheck_consumed_at
                        .zip(item.recheck_at)
                        .is_none_or(|(consumed, recheck_at)| consumed < recheck_at)
                {
                    return None;
                }
                Some(
                    SchedulerDecision::new(
                        SchedulerDecisionKind::WaitForOperator,
                        "work_item_needs_input",
                    )
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingOperator"),
                )
            }
            Some(WorkItemSchedulingState::WaitingTask) => Some(
                SchedulerDecision::new(SchedulerDecisionKind::WaitForTask, "work_item_task_wait")
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingTask"),
            ),
            Some(WorkItemSchedulingState::WaitingExternal) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForExternalChange,
                    "work_item_external_wait",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("work_item_scheduling_state=WaitingExternal"),
            ),
            Some(WorkItemSchedulingState::WaitingTimer) => Some(
                SchedulerDecision::new(SchedulerDecisionKind::WaitForTimer, "work_item_timer_wait")
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingTimer"),
            ),
            Some(WorkItemSchedulingState::WaitingSystem) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::EmitSystemTick,
                    "work_item_system_wait",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("work_item_scheduling_state=WaitingSystem"),
            ),
            _ => None,
        }
    })
}

pub(crate) fn idle_boundary_decision(
    projection: &SchedulerProjection,
    boundary: impl Into<String>,
) -> SchedulerDecision {
    let boundary = boundary.into();
    if matches!(projection.status, AgentStatus::Stopped) {
        return idle_noop_decision(projection).boundary(boundary);
    }
    if let Some(decision) = wait_decision_for_projection(projection) {
        return decision.boundary(boundary);
    }
    if let Some(signal) = projection.work_reactivation_signal() {
        return SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "runnable_work")
            .boundary(boundary)
            .model_reentry(true)
            .work_item_id(signal.work_item_id)
            .evidence("runtime_idle")
            .evidence("work_item_runnable");
    }
    idle_noop_decision(projection).boundary(boundary)
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(crate) fn projected_status_for_idle(
    state: &AgentState,
    _storage: &AppStorage,
) -> Result<AgentStatus> {
    if matches!(state.status, AgentStatus::Asleep | AgentStatus::Stopped) {
        return Ok(state.status.clone());
    }
    Ok(AgentStatus::AwakeIdle)
}

pub(crate) fn apply_idle_projection(state: &mut AgentState, storage: &AppStorage) -> Result<()> {
    state.status = projected_status_for_idle(state, storage)?;
    state.current_run_id = None;
    Ok(())
}

pub(crate) fn apply_running_projection(state: &mut AgentState, run_id: String) {
    state.status = AgentStatus::AwakeRunning;
    state.current_run_id = Some(run_id);
}

pub(crate) fn apply_message_wake_projection(state: &mut AgentState) -> bool {
    if matches!(state.status, AgentStatus::Asleep | AgentStatus::Booting) {
        state.status = AgentStatus::AwakeIdle;
        state.sleeping_until = None;
        return true;
    }
    false
}

pub(crate) fn apply_start_projection(state: &mut AgentState) {
    state.status = AgentStatus::AwakeIdle;
    state.current_run_id = None;
}

pub(crate) fn apply_stop_projection(state: &mut AgentState) {
    state.status = AgentStatus::Stopped;
    state.current_run_id = None;
    state.sleeping_until = None;
    state.pending_wake_hint = None;
}

pub(crate) fn apply_sleep_projection(
    state: &mut AgentState,
    sleeping_until: Option<DateTime<Utc>>,
) {
    state.status = AgentStatus::Asleep;
    state.current_run_id = None;
    state.sleeping_until = sleeping_until;
}

pub(crate) fn is_operator_interjection_message(message: &MessageEnvelope) -> bool {
    matches!(
        (
            &message.kind,
            &message.origin,
            &message.authority_class,
            &message.priority,
        ),
        (
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { .. },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
        )
    )
}

pub(crate) fn work_queue_tick_idempotency_key(work_item: &WorkItemRecord, reason: &str) -> String {
    format!(
        "work_queue:{}:{}:{}",
        reason, work_item.id, work_item.revision
    )
}

pub(crate) fn wake_hint_idempotency_key(pending: &PendingWakeHint) -> String {
    let scope = pending
        .external_trigger_id
        .as_deref()
        .or(pending.source.as_deref())
        .unwrap_or("unknown");
    format!(
        "wake_hint:{}:{}",
        scope,
        pending.created_at.timestamp_micros()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AdmissionContext, AgentState, MessageBody, MessageDeliverySurface};

    // --- apply_start_projection ---

    #[test]
    fn apply_start_sets_awake_idle_and_clears_run() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Stopped;
        state.current_run_id = Some("stale".into());
        apply_start_projection(&mut state);
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    // --- apply_stop_projection ---

    #[test]
    fn apply_stop_clears_all_runtime_state() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        state.current_run_id = Some("run-1".into());
        state.sleeping_until = Some(Utc::now());
        state.pending_wake_hint = Some(PendingWakeHint {
            reason: "test".into(),
            description: None,
            source: None,
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        apply_stop_projection(&mut state);
        assert_eq!(state.status, AgentStatus::Stopped);
        assert_eq!(state.current_run_id, None);
        assert_eq!(state.sleeping_until, None);
        assert_eq!(state.pending_wake_hint, None);
    }

    // --- apply_sleep_projection ---

    #[test]
    fn apply_sleep_sets_status_and_clears_run() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        state.current_run_id = Some("run-1".into());
        let until = Utc::now() + chrono::Duration::hours(1);
        apply_sleep_projection(&mut state, Some(until));
        assert_eq!(state.status, AgentStatus::Asleep);
        assert_eq!(state.current_run_id, None);
        assert_eq!(state.sleeping_until, Some(until));
    }

    #[test]
    fn apply_sleep_indefinite_clears_sleeping_until() {
        let mut state = AgentState::new("test");
        state.sleeping_until = Some(Utc::now());
        apply_sleep_projection(&mut state, None);
        assert_eq!(state.status, AgentStatus::Asleep);
        assert_eq!(state.sleeping_until, None);
    }

    // --- apply_running_projection ---

    #[test]
    fn apply_running_sets_awake_running_with_run_id() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeIdle;
        apply_running_projection(&mut state, "run-42".into());
        assert_eq!(state.status, AgentStatus::AwakeRunning);
        assert_eq!(state.current_run_id.as_deref(), Some("run-42"));
    }

    // --- apply_message_wake_projection ---

    #[test]
    fn apply_message_wake_from_asleep_returns_true() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Asleep;
        state.sleeping_until = Some(Utc::now());
        assert!(apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.sleeping_until, None);
    }

    #[test]
    fn apply_message_wake_from_booting_returns_true() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Booting;
        assert!(apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[test]
    fn apply_message_wake_from_running_returns_false() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        assert!(!apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeRunning);
    }

    // --- is_operator_interjection_message ---

    #[test]
    fn operator_interjection_detected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
            MessageBody::Text {
                text: "urgent".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(is_operator_interjection_message(&msg));
    }

    #[test]
    fn non_interjection_priority_rejected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "normal".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(!is_operator_interjection_message(&msg));
    }

    #[test]
    fn non_operator_kind_rejected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::SystemTick,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
            MessageBody::Text {
                text: "tick".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(!is_operator_interjection_message(&msg));
    }
}
