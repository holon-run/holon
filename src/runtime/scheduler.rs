use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::domain::scheduler_protocol::{SchedulerOwner, SchedulerScenarioClass};
use crate::runtime::closure::runtime_error_active;
use crate::storage::{AppStorage, WorkQueueReadModel};
use crate::types::{
    AdmissionContext, AgentPostureProjection, AgentSchedulingPosture, AgentStatus, AuthorityClass,
    ExternalWaitRecoverability, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, PendingWakeHint, Priority, TaskRecord, TaskStatus, TimerStatus,
    TurnTerminalKind, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
    WorkItemRecord, WorkItemSchedulingState, WorkReactivationMode, WorkReactivationSignal,
};
use anyhow::bail;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fmt};

#[cfg(test)]
pub(crate) const REDUCER_ONLY_CANDIDATES_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ReducerOnlyCandidates;
pub(crate) const WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::WorkItemAutonomousContinuation;
pub(crate) const EXACT_TASK_REJOIN_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExactTaskRejoin;
pub(crate) const EXACT_WAIT_RESUME_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExactWaitResume;
pub(crate) const EXPLICITLY_BOUND_OPERATOR_INPUT_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExplicitlyBoundOperatorInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalActivationScenario {
    WorkItemAutonomousContinuation {
        work_item_id: String,
    },
    ProviderRecovery {
        work_item_id: String,
    },
    InternalFollowup {
        work_item_id: String,
    },
    ExactTaskRejoin {
        task_id: String,
        work_item_id: String,
        wait_id: Option<String>,
    },
    ExactWaitResume {
        owner: SchedulerOwner,
        wait_id: String,
    },
    LifecycleExternalNudge {
        agent_id: String,
    },
    ExplicitlyBoundOperatorInput {
        work_item_id: String,
        wait_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalActivationCandidate {
    UnboundTaskResultWaitOrReduce,
    WorkItemAutonomousContinuation {
        work_item_id: String,
    },
    ProviderRecovery {
        work_item_id: String,
    },
    InternalFollowup {
        work_item_id: String,
    },
    ExactTaskRejoin {
        task_id: String,
        work_item_id: String,
    },
    ExactWaitResume {
        expected_work_item_id: Option<String>,
        correlated_wait: Option<String>,
    },
    LifecycleExternalNudge {
        agent_id: String,
    },
    ExplicitlyBoundOperatorInput {
        work_item_id: String,
    },
}

#[derive(Debug)]
pub(crate) struct AmbiguousCanonicalWaits {
    pub(crate) message_id: String,
    pub(crate) wait_condition_ids: Vec<String>,
}

impl fmt::Display for AmbiguousCanonicalWaits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "canonical activation message {} matches multiple active waits",
            self.message_id
        )
    }
}

impl std::error::Error for AmbiguousCanonicalWaits {}

impl CanonicalActivationScenario {
    pub(crate) fn work_item_id(&self) -> Option<&str> {
        match self {
            Self::WorkItemAutonomousContinuation { work_item_id }
            | Self::ProviderRecovery { work_item_id }
            | Self::InternalFollowup { work_item_id }
            | Self::ExactTaskRejoin { work_item_id, .. }
            | Self::ExplicitlyBoundOperatorInput { work_item_id, .. } => Some(work_item_id),
            Self::ExactWaitResume { owner, .. } => owner.work_item_id(),
            Self::LifecycleExternalNudge { .. } => None,
        }
    }
}

impl CanonicalActivationCandidate {
    pub(crate) fn scenario_class(&self) -> SchedulerScenarioClass {
        match self {
            Self::UnboundTaskResultWaitOrReduce => EXACT_WAIT_RESUME_SCENARIO,
            Self::WorkItemAutonomousContinuation { .. }
            | Self::ProviderRecovery { .. }
            | Self::InternalFollowup { .. } => WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO,
            Self::ExactTaskRejoin { .. } => EXACT_TASK_REJOIN_SCENARIO,
            Self::ExactWaitResume { .. } | Self::LifecycleExternalNudge { .. } => {
                EXACT_WAIT_RESUME_SCENARIO
            }
            Self::ExplicitlyBoundOperatorInput { .. } => EXPLICITLY_BOUND_OPERATOR_INPUT_SCENARIO,
        }
    }

    fn expected_work_item_id(&self) -> Option<&str> {
        match self {
            Self::UnboundTaskResultWaitOrReduce => None,
            Self::WorkItemAutonomousContinuation { work_item_id }
            | Self::ProviderRecovery { work_item_id }
            | Self::InternalFollowup { work_item_id }
            | Self::ExactTaskRejoin { work_item_id, .. }
            | Self::ExplicitlyBoundOperatorInput { work_item_id } => Some(work_item_id),
            Self::ExactWaitResume {
                expected_work_item_id,
                ..
            } => expected_work_item_id.as_deref(),
            Self::LifecycleExternalNudge { .. } => None,
        }
    }
}

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
    activation_waits: Vec<WaitConditionRecord>,
    canonical_work_states: Option<HashMap<String, CanonicalWorkExecutionState>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CanonicalWorkExecutionState {
    Waiting { wait_id: String },
    Other,
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
    pub(crate) fn without_canonical_authority(mut self) -> Self {
        // activation_waits remains shared input for legacy wait-to-WorkItem correlation.
        self.canonical_work_states = None;
        self
    }

    #[cfg(test)]
    pub(crate) fn enable_canonical_authority_for_test(&mut self) {
        self.canonical_work_states.get_or_insert_with(HashMap::new);
    }

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
        let activation_waits = storage
            .latest_wait_conditions()?
            .into_iter()
            .filter(|condition| condition.agent_id == snapshot.id)
            .filter(|condition| {
                condition.status == WaitConditionStatus::Active
                    || condition.status == WaitConditionStatus::Triggered
                    || (condition.status == WaitConditionStatus::Resolved
                        && condition.kind == WaitConditionKind::Task)
            })
            .collect();
        let execution_snapshot = storage
            .runtime_db()?
            .map(|runtime_db| {
                runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&snapshot.id)
            })
            .transpose()?
            .flatten();
        let canonical_work_states = execution_snapshot.as_ref().map(|snapshot| {
            snapshot
                .work_items
                .iter()
                .map(|(work_item_id, record)| {
                    let state = match &record.state {
                        WorkItemExecutionState::Waiting { wait, .. } => {
                            CanonicalWorkExecutionState::Waiting {
                                wait_id: wait.wait_id.clone(),
                            }
                        }
                        _ => CanonicalWorkExecutionState::Other,
                    };
                    (work_item_id.clone(), state)
                })
                .collect()
        });
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
            activation_waits,
            canonical_work_states,
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

pub(crate) fn append_ambiguous_wait_advisory(
    storage: &AppStorage,
    message: &MessageEnvelope,
    wait_condition_ids: &[String],
) -> Result<()> {
    let diagnostic = SchedulingAdvisory::warning(
        "ambiguous_canonical_wait_binding",
        "canonical activation input matches multiple active waits and remains queued",
    )
    .maybe_work_item_id(message.work_item_id.clone())
    .evidence(format!("message_id={}", message.id))
    .evidence(format!(
        "wait_condition_ids={}",
        wait_condition_ids.join(",")
    ));
    let event = scheduling_advisory_event(&diagnostic);
    let duplicate = storage
        .read_recent_events(64)?
        .iter()
        .any(|latest| latest.kind == event.kind && latest.data == event.data);
    if !duplicate {
        storage.append_event(&event)?;
    }
    Ok(())
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
        } => {
            let matching_wait_work_item_id = matching_wait_conditions(projection, message)
                .into_iter()
                .filter_map(|condition| condition.work_item_id.clone())
                .next();
            let mut decision =
                message_processing_decision(message, model_turn_allowed, continuation_resolution)
                    .boundary(boundary_label)
                    .evidence(format!("queue_len={}", projection.queue_len))
                    .evidence(format!("turn_in_progress={}", projection.turn_in_progress));
            if decision.work_item_id.is_none() {
                decision.work_item_id = matching_wait_work_item_id;
            }
            decision
        }
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

pub(crate) fn scheduler_diagnostic_event(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<AuditEvent> {
    let payload = scheduler_diagnostic_audit_event(agent_id, decision);
    AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic,
        &payload,
    )
}

pub(crate) fn scheduler_invariant_diagnostic_event(
    agent_id: &str,
    code: &str,
    work_item_id: Option<String>,
    message_id: Option<String>,
    evidence: Vec<String>,
) -> Result<AuditEvent> {
    AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic,
        &crate::types::SchedulerDiagnosticAuditEvent {
            agent_id: agent_id.to_string(),
            decision: "InvariantViolation".into(),
            reason: code.to_string(),
            boundary: Some("bootstrap_recovery".into()),
            scenario_class: None,
            work_item_id,
            message_id,
            task_id: None,
            evidence,
        },
    )
}

pub(crate) fn scheduler_decision_events(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<[AuditEvent; 2]> {
    Ok([
        scheduler_diagnostic_event(agent_id, decision)?,
        scheduler_decision_event(decision),
    ])
}

pub(crate) fn append_scheduler_decision(
    storage: &AppStorage,
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<bool> {
    let events = scheduler_decision_events(agent_id, decision)?;
    let legacy_event = &events[1];
    let duplicate = storage
        .read_recent_events(32)?
        .into_iter()
        .rev()
        .find(|latest| latest.kind == legacy_event.kind)
        .is_some_and(|latest| latest.data == legacy_event.data);
    if duplicate {
        return Ok(false);
    }
    storage.append_events(&events)?;
    Ok(true)
}

pub(crate) fn scheduler_diagnostic_audit_event(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> crate::types::SchedulerDiagnosticAuditEvent {
    crate::types::SchedulerDiagnosticAuditEvent {
        agent_id: agent_id.to_string(),
        decision: decision.kind.as_str().to_string(),
        reason: decision.reason.clone(),
        boundary: decision.boundary.clone(),
        scenario_class: None,
        work_item_id: decision.work_item_id.clone(),
        message_id: decision.message_id.clone(),
        task_id: decision.task_id.clone(),
        evidence: decision.evidence.clone(),
    }
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

#[cfg(test)]
pub(crate) fn authority_scenarios_for_message_claim(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    continuation_resolution: Option<&ContinuationResolution>,
) -> Vec<SchedulerScenarioClass> {
    let mut scenarios = Vec::with_capacity(1);
    if message_admission_scenario_applies(message, continuation_resolution) {
        scenarios.push(REDUCER_ONLY_CANDIDATES_SCENARIO);
    }
    if wait_resume_scenario_applies(projection, message) {
        scenarios.push(
            wait_resume_scenario_class(message)
                .expect("applicable wait resume has a registered scenario class"),
        );
    }
    scenarios
}

pub(crate) fn canonical_activation_candidate(
    message: &MessageEnvelope,
    _continuation_resolution: Option<&ContinuationResolution>,
    task: Option<&TaskRecord>,
) -> Result<Option<CanonicalActivationCandidate>> {
    if matches!(
        (&message.kind, &message.origin),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem })
            if subsystem == "work_queue"
    ) {
        return Ok(message.work_item_id.clone().map(|work_item_id| {
            CanonicalActivationCandidate::WorkItemAutonomousContinuation { work_item_id }
        }));
    }
    if message.kind == MessageKind::InternalFollowup {
        if let Some(work_item_id) = message.work_item_id.clone() {
            if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
                return Ok(Some(CanonicalActivationCandidate::ProviderRecovery {
                    work_item_id,
                }));
            }
        }
        return Ok(if let Some(work_item_id) = message.work_item_id.clone() {
            Some(CanonicalActivationCandidate::InternalFollowup { work_item_id })
        } else if runtime_owned_internal_followup(message) {
            Some(CanonicalActivationCandidate::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            })
        } else {
            None
        });
    }
    if message.kind == MessageKind::TaskResult {
        let MessageOrigin::Task { task_id } = &message.origin else {
            bail!("canonical task rejoin requires task message origin");
        };
        if message.task_id.as_deref() != Some(task_id.as_str()) {
            bail!("canonical task rejoin has inconsistent task identity");
        }
        let task = task.ok_or_else(|| anyhow!("canonical task rejoin is missing task record"))?;
        if task.id != *task_id || task.agent_id != message.agent_id {
            bail!("canonical task rejoin requires a same-agent task identity");
        }
        if !matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(None);
        }
        if let Some(work_item_id) = task.effective_work_item_id() {
            if message.work_item_id.as_deref() != Some(work_item_id) {
                bail!("canonical task rejoin has inconsistent WorkItem binding");
            }
            return Ok(Some(CanonicalActivationCandidate::ExactTaskRejoin {
                task_id: task_id.clone(),
                work_item_id: work_item_id.to_string(),
            }));
        }
        if task.terminal_reentry() {
            return Ok(Some(CanonicalActivationCandidate::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            }));
        }
        return Ok(Some(
            CanonicalActivationCandidate::UnboundTaskResultWaitOrReduce,
        ));
    }

    if message.kind == MessageKind::OperatorPrompt {
        if trusted_explicit_operator_binding(message) {
            let work_item_id = message
                .work_item_id
                .clone()
                .ok_or_else(|| anyhow!("explicit operator input requires a WorkItem binding"))?;
            return Ok(Some(
                CanonicalActivationCandidate::ExplicitlyBoundOperatorInput { work_item_id },
            ));
        }
        if message.authority_class == AuthorityClass::OperatorInstruction
            && matches!(message.origin, MessageOrigin::Operator { .. })
        {
            return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
                expected_work_item_id: None,
                correlated_wait: None,
            }));
        }
        return Ok(None);
    }

    if matches!(
        (&message.kind, &message.origin),
        (
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent,
            _
        ) | (MessageKind::SystemTick, MessageOrigin::System { .. })
    ) {
        if let Some(correlated_wait) = authoritative_wait_correlation(message) {
            return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
                expected_work_item_id: message.work_item_id.clone(),
                correlated_wait: Some(correlated_wait),
            }));
        }
        return Ok(Some(CanonicalActivationCandidate::LifecycleExternalNudge {
            agent_id: message.agent_id.clone(),
        }));
    }

    if matches!(
        (&message.kind, &message.origin),
        (MessageKind::TimerTick, MessageOrigin::Timer { .. })
    ) {
        return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
            expected_work_item_id: message.work_item_id.clone(),
            correlated_wait: None,
        }));
    }

    Ok(None)
}

pub(crate) fn runtime_owned_internal_followup(message: &MessageEnvelope) -> bool {
    message.kind == MessageKind::InternalFollowup
        && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && message.admission_context == Some(AdmissionContext::RuntimeOwned)
        && matches!(
            message.origin,
            MessageOrigin::System { .. } | MessageOrigin::Task { .. }
        )
}

pub(crate) fn resolve_canonical_activation_scenario(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    candidate: CanonicalActivationCandidate,
) -> Result<Option<CanonicalActivationScenario>> {
    if let CanonicalActivationCandidate::WorkItemAutonomousContinuation { work_item_id } = candidate
    {
        return Ok(Some(
            CanonicalActivationScenario::WorkItemAutonomousContinuation { work_item_id },
        ));
    }
    if let CanonicalActivationCandidate::ProviderRecovery { work_item_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::ProviderRecovery {
            work_item_id,
        }));
    }
    if let CanonicalActivationCandidate::InternalFollowup { work_item_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::InternalFollowup {
            work_item_id,
        }));
    }
    if let CanonicalActivationCandidate::LifecycleExternalNudge { agent_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::LifecycleExternalNudge {
            agent_id,
        }));
    }

    let mut matching_waits = match &candidate {
        CanonicalActivationCandidate::ExactWaitResume {
            correlated_wait: Some(wait_id),
            ..
        } => projection
            .activation_waits
            .iter()
            .filter(|condition| {
                condition.id == *wait_id
                    && (condition.status == WaitConditionStatus::Active
                        || (condition.status == WaitConditionStatus::Triggered
                            && condition.trigger_message_id() == Some(message.id.as_str())))
                    && condition.work_item_id.as_deref() == candidate.expected_work_item_id()
            })
            .collect(),
        _ => {
            let mut waits = matching_wait_conditions_for_work_item(
                projection,
                message,
                candidate.expected_work_item_id(),
            );
            if message.work_item_id.is_none()
                && matches!(
                    message.kind,
                    MessageKind::OperatorPrompt | MessageKind::TaskResult
                )
            {
                waits.retain(|wait| wait.work_item_id.is_none());
            }
            waits
        }
    };
    if matching_waits.len() > 1
        && matches!(
            candidate,
            CanonicalActivationCandidate::ExactTaskRejoin { .. }
        )
        && projection.canonical_work_states.is_none()
    {
        // The durable task rejoin fence is authoritative. Before the canonical
        // scheduler partition exists, duplicate legacy wait rows are mirrors
        // and must not make the exact task identity ambiguous.
        matching_waits.clear();
    } else if matching_waits.len() > 1 {
        return Err(anyhow::Error::new(AmbiguousCanonicalWaits {
            message_id: message.id.clone(),
            wait_condition_ids: matching_waits.iter().map(|wait| wait.id.clone()).collect(),
        }));
    }
    let matching_wait = matching_waits.first().copied();

    if let CanonicalActivationCandidate::ExactTaskRejoin {
        task_id,
        work_item_id,
    } = candidate
    {
        if matching_wait.is_none() {
            match projection
                .canonical_work_states
                .as_ref()
                .and_then(|states| states.get(&work_item_id))
            {
                Some(CanonicalWorkExecutionState::Other) => {}
                Some(CanonicalWorkExecutionState::Waiting { .. }) => {
                    return Ok(None);
                }
                None if projection.canonical_work_states.is_some() => return Ok(None),
                None => {}
            }
        }
        return Ok(Some(CanonicalActivationScenario::ExactTaskRejoin {
            task_id,
            work_item_id,
            wait_id: matching_wait.map(|wait| wait.id.clone()),
        }));
    }

    if let CanonicalActivationCandidate::ExplicitlyBoundOperatorInput { work_item_id } = candidate {
        return Ok(Some(
            CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                work_item_id,
                wait_id: matching_wait.map(|wait| wait.id.clone()),
            },
        ));
    }

    let Some(wait) = matching_wait else {
        if matches!(
            message.kind,
            MessageKind::OperatorPrompt | MessageKind::TimerTick
        ) {
            return Ok(Some(CanonicalActivationScenario::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            }));
        }
        return Ok(None);
    };
    Ok(Some(CanonicalActivationScenario::ExactWaitResume {
        owner: wait
            .work_item_id
            .clone()
            .map(|work_item_id| SchedulerOwner::WorkItem { work_item_id })
            .unwrap_or_else(|| SchedulerOwner::AgentLifecycle {
                agent_id: wait.agent_id.clone(),
            }),
        wait_id: wait.id.clone(),
    }))
}

fn authoritative_wait_correlation(message: &MessageEnvelope) -> Option<String> {
    let trusted = matches!(
        (message.delivery_surface, message.admission_context),
        (
            Some(MessageDeliverySurface::RuntimeSystem),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::TaskRejoin),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::HttpCallbackWake),
            Some(AdmissionContext::ExternalTriggerCapability)
        )
    ) && matches!(
        message.authority_class,
        AuthorityClass::RuntimeInstruction | AuthorityClass::IntegrationSignal
    );
    if !trusted {
        return None;
    }
    message.source_refs.get("wait_id").cloned()
}

fn matching_wait_conditions<'a>(
    projection: &'a SchedulerProjection,
    message: &MessageEnvelope,
) -> Vec<&'a WaitConditionRecord> {
    matching_wait_conditions_for_work_item(projection, message, None)
}

fn matching_wait_conditions_for_work_item<'a>(
    projection: &'a SchedulerProjection,
    message: &MessageEnvelope,
    expected_work_item_id: Option<&str>,
) -> Vec<&'a WaitConditionRecord> {
    projection
        .activation_waits
        .iter()
        .filter(|condition| {
            expected_work_item_id
                .is_none_or(|work_item_id| condition.work_item_id.as_deref() == Some(work_item_id))
                && (condition.status == WaitConditionStatus::Active
                    || (condition.status == WaitConditionStatus::Triggered
                        && condition.trigger_message_id() == Some(message.id.as_str()))
                    || (message.kind == MessageKind::TaskResult
                        && condition.status == WaitConditionStatus::Resolved
                        && condition.kind == WaitConditionKind::Task
                        && condition.work_item_id == message.work_item_id
                        && condition.trigger_message_id() == Some(message.id.as_str())
                        && resolved_task_wait_is_current(projection, condition)))
                && message_matches_wait_condition(message, condition)
                && (!(message.kind == MessageKind::TaskResult
                    && condition.kind == WaitConditionKind::Task)
                    || resolved_task_wait_is_current(projection, condition))
        })
        .collect()
}

fn resolved_task_wait_is_current(
    projection: &SchedulerProjection,
    condition: &WaitConditionRecord,
) -> bool {
    let Some(states) = &projection.canonical_work_states else {
        return true;
    };
    let Some(work_item_id) = condition.work_item_id.as_deref() else {
        return condition.trigger_message_id().is_some();
    };
    matches!(
        states.get(work_item_id),
        Some(CanonicalWorkExecutionState::Waiting { wait_id }) if wait_id == &condition.id
    )
}

fn trusted_explicit_operator_binding(message: &MessageEnvelope) -> bool {
    message
        .message_seq
        .is_some_and(|message_seq| message_seq > 0)
        && message.work_item_id.is_some()
        && message.authority_class == AuthorityClass::OperatorInstruction
        && matches!(message.origin, MessageOrigin::Operator { .. })
        && matches!(
            (message.delivery_surface, message.admission_context),
            (
                Some(MessageDeliverySurface::CliPrompt | MessageDeliverySurface::RunOnce),
                Some(AdmissionContext::LocalProcess)
            ) | (
                Some(MessageDeliverySurface::HttpControlPrompt),
                Some(AdmissionContext::ControlAuthenticated)
            ) | (
                Some(MessageDeliverySurface::RemoteOperatorTransport),
                Some(AdmissionContext::OperatorTransportAuthenticated)
            )
        )
}

#[cfg(test)]
fn message_admission_scenario_applies(
    message: &MessageEnvelope,
    continuation_resolution: Option<&ContinuationResolution>,
) -> bool {
    matches!(
        continuation_resolution.map(|resolution| resolution.class),
        None | Some(
            crate::types::ContinuationClass::LocalContinuation
                | crate::types::ContinuationClass::LivenessOnly
        )
    ) && !matches!(
        message.kind,
        MessageKind::OperatorPrompt | MessageKind::TaskResult | MessageKind::SystemTick
    )
}

#[cfg(test)]
fn wait_resume_scenario_class(message: &MessageEnvelope) -> Option<SchedulerScenarioClass> {
    match message.kind {
        MessageKind::TaskResult => Some(EXACT_TASK_REJOIN_SCENARIO),
        MessageKind::CallbackEvent
        | MessageKind::WebhookEvent
        | MessageKind::ChannelEvent
        | MessageKind::TimerTick
        | MessageKind::SystemTick => Some(EXACT_WAIT_RESUME_SCENARIO),
        _ => None,
    }
}

#[cfg(test)]
fn wait_resume_scenario_applies(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
) -> bool {
    matches!(
        message.kind,
        MessageKind::TaskResult
            | MessageKind::CallbackEvent
            | MessageKind::WebhookEvent
            | MessageKind::ChannelEvent
            | MessageKind::TimerTick
            | MessageKind::SystemTick
    ) && !matching_wait_conditions(projection, message).is_empty()
}

pub(super) fn message_matches_wait_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> bool {
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => {
            condition.wake_sources.iter().any(
                |source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id),
            )
        }
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput)),
        (MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            condition.wake_sources.iter().any(|source| {
                matches!(
                    source,
                    WakeSource::ExternalIngress {
                        external_trigger_id: expected,
                    } if expected.as_ref().is_none_or(|expected| {
                        external_trigger_id.is_some_and(|actual| actual == expected)
                    })
                )
            })
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => {
            condition
                .subject_ref
                .as_deref()
                .is_none_or(|subject_ref| subject_ref == timer_id)
                && condition.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::Timer { .. })
                        && message
                            .source_refs
                            .get("timer_id")
                            .is_none_or(|source_timer_id| source_timer_id == timer_id)
                })
        }
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if subsystem == "work_queue" {
                return false;
            }
            if let Some(external_trigger_id) = message.source_refs.get("external_trigger_id") {
                return condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::ExternalIngress {
                            external_trigger_id: expected,
                        } if expected.as_ref().is_none_or(|expected| {
                            expected == external_trigger_id
                        })
                    )
                });
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
        }
        _ => false,
    }
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
