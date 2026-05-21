use super::*;
use crate::runtime::closure::runtime_error_active;
use crate::storage::{AppStorage, WorkQueuePromptProjection};
use crate::types::{
    AgentStatus, MessageEnvelope, MessageKind, MessageOrigin, PendingWakeHint, Priority,
    TaskRecord, TaskStatus, TimerStatus, TrustLevel, TurnTerminalKind, WaitingIntentStatus,
    WorkItemRecord, WorkItemSchedulingState,
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
        let active_waiting_intents = storage
            .latest_waiting_intents()?
            .into_iter()
            .filter(|intent| {
                intent.agent_id == snapshot.id && intent.status == WaitingIntentStatus::Active
            })
            .collect::<Vec<_>>();
        let active_work_item_waiting_intents = active_waiting_intents
            .iter()
            .filter(|intent| intent.scope == ExternalTriggerScope::WorkItem)
            .count();
        let active_agent_waiting_intents = active_waiting_intents
            .iter()
            .filter(|intent| intent.scope == ExternalTriggerScope::Agent)
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
            active_waiting_intents: active_waiting_intents.len(),
            active_work_item_waiting_intents,
            active_agent_waiting_intents,
            active_timers,
            last_turn_terminal: snapshot.last_turn_terminal.clone(),
            turn_in_progress: snapshot.active_run_id.is_some(),
            runtime_error: runtime_error_active(
                &storage.read_recent_events(64)?,
                &storage.read_recent_briefs(64)?,
            ),
        })
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
    if matches!(projection.status, AgentStatus::Paused) {
        return SchedulerDecision::new(SchedulerDecisionKind::Noop, "paused")
            .boundary(boundary)
            .liveness_only(true)
            .evidence(format!("status={:?}", projection.status));
    }
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
    } else if matches!(projection.status, AgentStatus::Paused) {
        (SchedulerDecisionKind::Noop, "paused")
    } else if matches!(projection.status, AgentStatus::Asleep) {
        (SchedulerDecisionKind::StayIdle, "already_asleep")
    } else if projection.queue_len > 0 {
        (SchedulerDecisionKind::Noop, "queue_not_empty")
    } else if projection.turn_in_progress {
        (SchedulerDecisionKind::Noop, "turn_in_progress")
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
    projection.current_work_item.as_ref().and_then(|item| {
        match projection.current_work_item_scheduling_state {
            Some(WorkItemSchedulingState::WaitingOperator) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForOperator,
                    "work_item_needs_input",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("current_work_item_scheduling_state=WaitingOperator"),
            ),
            Some(WorkItemSchedulingState::WaitingTask) => Some(
                SchedulerDecision::new(SchedulerDecisionKind::WaitForTask, "work_item_task_wait")
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("current_work_item_scheduling_state=WaitingTask"),
            ),
            Some(WorkItemSchedulingState::WaitingExternal) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForExternalChange,
                    "work_item_external_wait",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("current_work_item_scheduling_state=WaitingExternal"),
            ),
            _ => None,
        }
    })
}

pub(crate) fn idle_boundary_decision(
    projection: &SchedulerProjection,
    boundary: impl Into<String>,
) -> SchedulerDecision {
    if matches!(
        projection.status,
        AgentStatus::Stopped | AgentStatus::Paused | AgentStatus::Asleep
    ) {
        return idle_noop_decision(projection).boundary(boundary);
    }
    if let Some(decision) = wait_decision_for_projection(projection) {
        return decision.boundary(boundary);
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
    if matches!(
        state.status,
        AgentStatus::Asleep | AgentStatus::Paused | AgentStatus::Stopped
    ) {
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
            &message.trust,
            &message.priority,
        ),
        (
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { .. },
            TrustLevel::TrustedOperator,
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
