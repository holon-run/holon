use super::*;
use crate::runtime::closure::runtime_error_active;
use crate::storage::AppStorage;
use crate::types::{
    AgentStatus, MessageEnvelope, TaskRecord, TaskStatus, TimerStatus, WaitingIntentStatus,
    WorkItemRecord,
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
    pub queued_runnable_work_items: Vec<WorkItemRecord>,
    pub queued_work_items: usize,
    pub pending_wake_hint: bool,
    pub active_waiting_intents: usize,
    pub active_work_item_waiting_intents: usize,
    pub active_agent_waiting_intents: usize,
    pub active_timers: usize,
    pub turn_in_progress: bool,
    pub runtime_error: bool,
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
        let latest_tasks = storage.latest_task_records()?;
        let active_tasks = state
            .active_task_ids
            .iter()
            .filter_map(|task_id| latest_tasks.iter().find(|task| &task.id == task_id))
            .filter(|task| is_active_task_status(&task.status))
            .cloned()
            .collect::<Vec<_>>();
        let has_blocking_active_tasks = active_tasks.iter().any(TaskRecord::is_blocking);
        let work_queue = storage.work_queue_prompt_projection()?;
        let queued_runnable_work_items = work_queue
            .queued_blocked
            .iter()
            .filter(|item| item.is_runnable())
            .cloned()
            .collect::<Vec<_>>();
        let active_waiting_intents = storage
            .latest_waiting_intents()?
            .into_iter()
            .filter(|intent| intent.status == WaitingIntentStatus::Active)
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
            .filter(|timer| timer.status == TimerStatus::Active)
            .count();
        Ok(Self {
            status: state.status.clone(),
            queue_len,
            active_run_id: state.current_run_id.clone(),
            active_tasks,
            has_blocking_active_tasks,
            current_work_item: work_queue.current,
            queued_work_items: queued_runnable_work_items.len(),
            queued_runnable_work_items,
            pending_wake_hint: state.pending_wake_hint.is_some(),
            active_waiting_intents: active_waiting_intents.len(),
            active_work_item_waiting_intents,
            active_agent_waiting_intents,
            active_timers,
            turn_in_progress: state.current_run_id.is_some(),
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
    pub model_visible: bool,
    pub liveness_only: bool,
    pub message_id: Option<String>,
    pub work_item_id: Option<String>,
    pub task_id: Option<String>,
    pub boundary: Option<String>,
    pub evidence: Vec<String>,
}

impl SchedulerDecision {
    pub(crate) fn new(kind: SchedulerDecisionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
            model_visible: false,
            liveness_only: false,
            message_id: None,
            work_item_id: None,
            task_id: None,
            boundary: None,
            evidence: Vec::new(),
        }
    }

    pub(crate) fn model_visible(mut self, value: bool) -> Self {
        self.model_visible = value;
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

pub(crate) fn scheduler_decision_event(decision: &SchedulerDecision) -> AuditEvent {
    AuditEvent::new(
        "scheduler_decision",
        serde_json::json!({
            "decision": decision.kind.as_str(),
            "reason": &decision.reason,
            "model_visible": decision.model_visible,
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
    model_visible: bool,
) -> SchedulerDecision {
    let kind = if model_visible {
        SchedulerDecisionKind::StartModelTurn
    } else {
        SchedulerDecisionKind::ReduceMessageOnly
    };
    let mut decision = SchedulerDecision::new(kind, format!("{:?}", message.kind))
        .message(message)
        .model_visible(model_visible)
        .liveness_only(!model_visible)
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
    if projection.has_blocking_active_tasks {
        let mut decision =
            SchedulerDecision::new(SchedulerDecisionKind::WaitForTask, "blocking_active_tasks")
                .liveness_only(true)
                .evidence(format!("active_tasks={}", projection.active_tasks.len()));
        if let Some(task) = projection
            .active_tasks
            .iter()
            .find(|task| task.is_blocking())
        {
            decision = decision.task_id(task.id.clone());
        }
        return Some(decision);
    }
    if projection.active_waiting_intents > 0 {
        return Some(
            SchedulerDecision::new(
                SchedulerDecisionKind::WaitForExternalChange,
                "active_waiting_intents",
            )
            .liveness_only(true)
            .evidence(format!(
                "active_waiting_intents={}",
                projection.active_waiting_intents
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
        if item.is_waiting_for_operator() {
            Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForOperator,
                    "work_item_needs_input",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone()),
            )
        } else {
            None
        }
    })
}

pub(crate) fn idle_boundary_decision(
    projection: &SchedulerProjection,
    boundary: impl Into<String>,
) -> SchedulerDecision {
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

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    !is_terminal_task_status(status)
}

pub(crate) fn projected_status_for_idle(
    state: &AgentState,
    storage: &AppStorage,
) -> Result<AgentStatus> {
    if matches!(
        state.status,
        AgentStatus::Asleep | AgentStatus::Paused | AgentStatus::Stopped
    ) {
        return Ok(state.status.clone());
    }
    if SchedulerProjection::from_state(storage, state)?.has_blocking_active_tasks {
        Ok(AgentStatus::AwaitingTask)
    } else {
        Ok(AgentStatus::AwakeIdle)
    }
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

pub(crate) fn apply_sleep_projection(
    state: &mut AgentState,
    sleeping_until: Option<DateTime<Utc>>,
) {
    state.status = AgentStatus::Asleep;
    state.current_run_id = None;
    state.sleeping_until = sleeping_until;
}

pub(crate) fn active_task_blocks_work_item_completion(
    task: &TaskRecord,
    work_item_id: &str,
) -> bool {
    if !task.is_blocking() || is_terminal_task_status(&task.status) {
        return false;
    }
    match task.effective_work_item_id() {
        Some(id) => id == work_item_id,
        None => true,
    }
}

pub(crate) fn has_completion_blocking_task_for_work_item(
    storage: &AppStorage,
    active_task_ids: &[String],
    work_item_id: &str,
) -> Result<bool> {
    if active_task_ids.is_empty() {
        return Ok(false);
    }
    let tasks = storage.latest_task_records()?;
    Ok(active_task_ids.iter().any(|task_id| {
        tasks
            .iter()
            .find(|task| &task.id == task_id)
            .is_some_and(|task| active_task_blocks_work_item_completion(task, work_item_id))
    }))
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
