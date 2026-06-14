use super::*;
use crate::runtime::closure::runtime_error_active;
use crate::storage::{AppStorage, WorkQueuePromptProjection};
use crate::types::{
    AgentPostureProjection, AgentSchedulingPosture, AgentStatus, AuthorityClass,
    ExternalWaitRecoverability, MessageEnvelope, MessageKind, MessageOrigin, PendingWakeHint,
    Priority, TaskRecord, TaskStatus, TimerStatus, TurnTerminalKind, WaitConditionKind,
    WaitConditionRecord, WaitConditionStatus, WorkItemRecord, WorkItemSchedulingState,
    WorkReactivationMode, WorkReactivationSignal,
};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SchedulerProjection {
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
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len(storage, &snapshot, queue_len)
    }

    pub(crate) fn from_snapshot_with_queue_len(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
    ) -> Result<Self> {
        let work_queue = storage.work_queue_prompt_projection()?;
        Self::from_snapshot_with_queue_len_and_work_queue(storage, snapshot, queue_len, work_queue)
    }

    pub(crate) fn from_state_with_work_queue(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: WorkQueuePromptProjection,
    ) -> Result<Self> {
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len_and_work_queue(
            storage,
            &snapshot,
            state.pending,
            work_queue,
        )
    }

    pub(crate) fn from_snapshot_with_queue_len_and_work_queue(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: WorkQueuePromptProjection,
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
            .readiness
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.scheduling_state);
        let waiting_work_item_projection = work_queue.readiness.iter().find(|item| {
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
        let active_wait_conditions = storage
            .latest_wait_conditions()?
            .into_iter()
            .filter(|condition| {
                condition.agent_id == snapshot.id && condition.status == WaitConditionStatus::Active
            })
            .collect::<Vec<_>>();
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
pub(crate) struct SchedulerDiagnostic {
    pub kind: String,
    pub severity: SchedulerDiagnosticSeverity,
    pub message: String,
    pub work_item_id: Option<String>,
    pub waiting_intent_id: Option<String>,
    pub wait_condition_id: Option<String>,
    pub evidence: Vec<String>,
}

impl SchedulerDiagnostic {
    fn warning(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            severity: SchedulerDiagnosticSeverity::Warning,
            message: message.into(),
            work_item_id: None,
            waiting_intent_id: None,
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
pub(crate) enum SchedulerDiagnosticSeverity {
    Warning,
}

impl SchedulerDiagnosticSeverity {
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
pub(crate) fn scheduling_diagnostics(
    storage: &AppStorage,
    agent: &AgentState,
) -> Result<Vec<SchedulerDiagnostic>> {
    scheduling_diagnostics_with_queue_len(storage, agent, agent.pending)
}

pub(crate) fn scheduling_diagnostics_with_queue_len(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<Vec<SchedulerDiagnostic>> {
    let projection = SchedulerProjection::from_state_with_queue_len(storage, agent, queue_len)?;
    let posture = storage.agent_posture_projection(agent)?;
    let work_queue = storage.work_queue_prompt_projection()?;
    let wait_conditions = storage.latest_wait_conditions()?;

    Ok(scheduling_diagnostics_for_facts(
        agent,
        &projection,
        &posture,
        &work_queue,
        &wait_conditions,
    ))
}

pub(crate) fn scheduling_diagnostics_for_facts(
    agent: &AgentState,
    projection: &SchedulerProjection,
    posture: &AgentPostureProjection,
    work_queue: &WorkQueuePromptProjection,
    wait_conditions: &[WaitConditionRecord],
) -> Vec<SchedulerDiagnostic> {
    let mut diagnostics = Vec::new();

    if posture.posture == AgentSchedulingPosture::Idle {
        if let Some(signal) = projection.work_reactivation_signal() {
            diagnostics.push(
                SchedulerDiagnostic::warning(
                    "idle_posture_has_runnable_work",
                    "agent posture is idle while scheduler facts contain runnable work",
                )
                .work_item_id(signal.work_item_id)
                .evidence("posture=Idle")
                .evidence(format!("reactivation_mode={:?}", signal.reactivation_mode)),
            );
        } else if projection.queue_len > 0 {
            diagnostics.push(
                SchedulerDiagnostic::warning(
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
                    SchedulerDiagnostic::warning(
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
                let mut diagnostic = SchedulerDiagnostic::warning(
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

    for item in work_queue.readiness.iter().filter(|item| {
        item.scheduling_state == WorkItemSchedulingState::Blocked
            && item.work_item.agent_id == agent.id
            && item.work_item.blocked_by.is_some()
            && item.work_item.recheck_at.is_none()
            && !item.has_active_waits
            && !item.has_active_task_waits
    }) {
        diagnostics.push(
            SchedulerDiagnostic::warning(
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

pub(crate) fn scheduler_diagnostic_event(diagnostic: &SchedulerDiagnostic) -> AuditEvent {
    AuditEvent::new(
        "scheduler_diagnostic",
        serde_json::json!({
            "kind": &diagnostic.kind,
            "severity": diagnostic.severity.as_str(),
            "message": &diagnostic.message,
            "work_item_id": &diagnostic.work_item_id,
            "waiting_intent_id": &diagnostic.waiting_intent_id,
            "wait_condition_id": &diagnostic.wait_condition_id,
            "evidence": &diagnostic.evidence,
        }),
    )
}

pub(crate) fn append_scheduling_diagnostics(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<usize> {
    let diagnostics = scheduling_diagnostics_with_queue_len(storage, agent, queue_len)?;
    let recent_events = storage.read_recent_events(64)?;
    let mut seen_data = Vec::new();
    let mut appended = 0;

    for diagnostic in diagnostics {
        let event = scheduler_diagnostic_event(&diagnostic);
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

trait SchedulerDiagnosticExt {
    fn maybe_work_item_id(self, work_item_id: Option<String>) -> Self;
}

impl SchedulerDiagnosticExt for SchedulerDiagnostic {
    fn maybe_work_item_id(mut self, work_item_id: Option<String>) -> Self {
        self.work_item_id = work_item_id;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
    #[allow(dead_code)]
    MessageProcessing,
    IdleTick,
}

impl SchedulerBoundary {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RunLoop => "run_loop",
            Self::RunLoopIdle => "run_loop_idle",
            Self::MessageProcessing => "message_processing",
            Self::IdleTick => "idle_tick",
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

    #[allow(dead_code)]
    pub(crate) fn task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
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
    AuditEvent::new(
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
    decision: &SchedulerDecision,
) -> Result<bool> {
    let event = scheduler_decision_event(decision);
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
            Some(WorkItemSchedulingState::WaitingOperator) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForOperator,
                    "work_item_needs_input",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("work_item_scheduling_state=WaitingOperator"),
            ),
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
        .waiting_intent_id
        .as_deref()
        .or(pending.external_trigger_id.as_deref())
        .or(pending.source.as_deref())
        .unwrap_or("unknown");
    format!(
        "wake_hint:{}:{}",
        scope,
        pending.created_at.timestamp_micros()
    )
}
