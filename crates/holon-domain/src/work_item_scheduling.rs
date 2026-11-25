//! WorkItem scheduling projections derived from caller-supplied facts.
use crate::{
    readiness_for_scheduling_state, TodoItem, TodoItemState, WaitConditionKind,
    WaitConditionRecord, WaitConditionSummary, WakeSource, WorkItemReadiness, WorkItemRecord,
    WorkItemSchedulingState, WorkItemState,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Borrow,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemCandidateClass {
    CurrentRunnable,
    TriggeredBlocked,
    QueuedRunnable,
    WaitingForOperator,
    Yielded,
    Blocked,
    Completing,
    CompletedRecent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemFocus {
    Current,
    Queued,
    Yielded,
    Blocked,
    Completing,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemSchedulingReasonCode {
    Completing,
    Completed,
    ContinuationYielded,
    ActiveTaskWait,
    ActiveOperatorWait,
    ActiveTimerWait,
    ActiveExternalWait,
    ActiveSystemWait,
    ManualBlocker,
    PlanNeedsInput,
    Runnable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemSchedulingProjection<W = WorkItemRecord> {
    #[serde(flatten)]
    pub work_item: W,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub focus: WorkItemFocus,
    pub is_current: bool,
    pub is_runnable: bool,
    pub has_active_waits: bool,
    pub has_active_task_waits: bool,
    pub has_triggered_waits: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_triggered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_todo: Option<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_wait_conditions: Vec<WaitConditionSummary>,
    pub reason_code: WorkItemSchedulingReasonCode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl<W: Borrow<WorkItemRecord>> WorkItemSchedulingProjection<W> {
    pub fn record(&self) -> &WorkItemRecord {
        self.work_item.borrow()
    }

    pub fn posture_reason(&self) -> String {
        self.posture_reason_display().to_string()
    }

    pub fn posture_reason_display(&self) -> impl std::fmt::Display + '_ {
        WorkItemPostureReason {
            id: &self.record().id,
            is_current: self.is_current,
            scheduling_state: self.scheduling_state,
        }
    }
}

pub struct WorkItemPostureReason<'a> {
    pub id: &'a str,
    pub is_current: bool,
    pub scheduling_state: WorkItemSchedulingState,
}

impl std::fmt::Display for WorkItemPostureReason<'_> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = if self.is_current {
            "current WorkItem"
        } else {
            "queued WorkItem"
        };
        write!(output, "{label} {} is {:?}", self.id, self.scheduling_state)
    }
}

impl<W: Borrow<WorkItemRecord>> Deref for WorkItemSchedulingProjection<W> {
    type Target = WorkItemRecord;

    fn deref(&self) -> &Self::Target {
        self.record()
    }
}

impl DerefMut for WorkItemSchedulingProjection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.work_item
    }
}

impl<W> WorkItemSchedulingProjection<W> {
    #[doc(hidden)]
    pub fn try_map_record<R>(
        self,
        map: impl FnOnce(W) -> anyhow::Result<R>,
    ) -> anyhow::Result<WorkItemSchedulingProjection<R>> {
        Ok(WorkItemSchedulingProjection {
            work_item: map(self.work_item)?,
            scheduling_state: self.scheduling_state,
            readiness: self.readiness,
            candidate_class: self.candidate_class,
            focus: self.focus,
            is_current: self.is_current,
            is_runnable: self.is_runnable,
            has_active_waits: self.has_active_waits,
            has_active_task_waits: self.has_active_task_waits,
            has_triggered_waits: self.has_triggered_waits,
            last_triggered_at: self.last_triggered_at,
            current_todo: self.current_todo,
            active_wait_conditions: self.active_wait_conditions,
            reason_code: self.reason_code,
            diagnostics: self.diagnostics,
        })
    }
}

pub struct WorkItemSchedulingFacts<'a> {
    pub work_item: &'a WorkItemRecord,
    pub is_current: bool,
    pub is_yielded: bool,
    pub active_wait_conditions: &'a [WaitConditionRecord],
    pub trigger_delivery_by_id: &'a BTreeMap<String, DateTime<Utc>>,
}

/// Active wait kinds and the latest delivery for one WorkItem.
#[derive(Debug, Default)]
pub struct WorkItemWaitFacts {
    task: bool,
    external: bool,
    operator: bool,
    timer: bool,
    system: bool,
    last_triggered_at: Option<DateTime<Utc>>,
}

impl WorkItemWaitFacts {
    pub fn record(
        &mut self,
        condition: &WaitConditionRecord,
        trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
    ) {
        self.record_kind(&condition.kind);
        for wake_source in &condition.wake_sources {
            let WakeSource::ExternalIngress {
                external_trigger_id: Some(external_trigger_id),
            } = wake_source
            else {
                continue;
            };
            if let Some(delivered_at) = trigger_delivery(external_trigger_id) {
                self.last_triggered_at = Some(
                    self.last_triggered_at
                        .map_or(delivered_at, |current| current.max(delivered_at)),
                );
            }
        }
    }

    /// Record a kind before allocating a summary or resolving Trigger deliveries.
    pub fn record_kind(&mut self, kind: &WaitConditionKind) {
        match kind {
            WaitConditionKind::Task => self.task = true,
            WaitConditionKind::External => self.external = true,
            WaitConditionKind::Operator => self.operator = true,
            WaitConditionKind::Timer => self.timer = true,
            WaitConditionKind::System => self.system = true,
        }
    }

    fn scheduling_state(&self) -> Option<WorkItemSchedulingState> {
        if self.task {
            Some(WorkItemSchedulingState::WaitingTask)
        } else if self.operator {
            Some(WorkItemSchedulingState::WaitingOperator)
        } else if self.timer {
            Some(WorkItemSchedulingState::WaitingTimer)
        } else if self.external {
            Some(WorkItemSchedulingState::WaitingExternal)
        } else if self.system {
            Some(WorkItemSchedulingState::WaitingSystem)
        } else {
            None
        }
    }

    pub fn diagnostic_messages(
        &self,
        work_item: &WorkItemRecord,
        is_current: bool,
        is_yielded: bool,
    ) -> [Option<&'static str>; 4] {
        [
            (work_item.state != WorkItemState::Open && is_current)
                .then_some("terminalizing_work_item_marked_current"),
            (is_yielded && is_current).then_some("yielded_work_item_marked_current"),
            (work_item.state != WorkItemState::Open && self.scheduling_state().is_some())
                .then_some("terminalizing_work_item_has_active_wait"),
            (self.kind_count() > 1).then_some("multiple_active_wait_kinds"),
        ]
    }

    fn kind_count(&self) -> usize {
        [
            self.task,
            self.operator,
            self.timer,
            self.external,
            self.system,
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }
}

pub fn derive_work_item_scheduling(
    facts: WorkItemSchedulingFacts<'_>,
) -> WorkItemSchedulingProjection {
    project_work_item_scheduling(
        facts.work_item.clone(),
        facts.is_current,
        facts.is_yielded,
        facts.active_wait_conditions,
        &|id| facts.trigger_delivery_by_id.get(id).copied(),
    )
}

/// Derive an owned projection from the active wait records supplied by the caller.
pub fn project_work_item_scheduling(
    work_item: WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    active_wait_conditions: &[WaitConditionRecord],
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> WorkItemSchedulingProjection {
    let mut waits = active_wait_conditions
        .iter()
        .filter(|condition| condition.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .collect::<Vec<_>>();
    waits.sort_by(|left, right| compare_wait_condition_order(left, right));

    let mut wait_states = WorkItemWaitFacts::default();
    for wait in &waits {
        wait_states.record(wait, trigger_delivery);
    }
    let summaries = waits.into_iter().map(WaitConditionSummary::from).collect();
    project_work_item_with_wait_summaries(work_item, is_current, is_yielded, wait_states, summaries)
}

pub fn compare_wait_condition_order(
    left: &WaitConditionRecord,
    right: &WaitConditionRecord,
) -> std::cmp::Ordering {
    left.created_at
        .cmp(&right.created_at)
        .then_with(|| left.id.cmp(&right.id))
}

/// Project from the wait facts and summaries of the same scoped active waits.
pub fn project_work_item_with_wait_summaries<W: Borrow<WorkItemRecord>>(
    work_item: W,
    is_current: bool,
    is_yielded: bool,
    wait_states: WorkItemWaitFacts,
    summaries: Vec<WaitConditionSummary>,
) -> WorkItemSchedulingProjection<W> {
    let record = work_item.borrow();
    let messages = wait_states.diagnostic_messages(record, is_current, is_yielded);
    let mut diagnostics = Vec::with_capacity(messages.iter().flatten().count());
    diagnostics.extend(messages.into_iter().flatten().map(str::to_owned));

    let SchedulingHeader {
        scheduling_state,
        reason_code,
        readiness,
        is_current,
        focus,
        has_triggered_waits,
        candidate_class,
    } = scheduling_header(record, is_current, is_yielded, &wait_states);

    let current_todo = current_todo(record);
    WorkItemSchedulingProjection {
        current_todo,
        work_item,
        scheduling_state,
        readiness,
        candidate_class,
        focus,
        is_current,
        is_runnable: readiness == WorkItemReadiness::Runnable,
        has_active_waits: !summaries.is_empty(),
        has_active_task_waits: wait_states.task,
        has_triggered_waits,
        last_triggered_at: wait_states.last_triggered_at,
        active_wait_conditions: summaries,
        reason_code,
        diagnostics,
    }
}

#[derive(Clone, Copy)]
struct SchedulingHeader {
    scheduling_state: WorkItemSchedulingState,
    reason_code: WorkItemSchedulingReasonCode,
    readiness: WorkItemReadiness,
    is_current: bool,
    focus: WorkItemFocus,
    has_triggered_waits: bool,
    candidate_class: WorkItemCandidateClass,
}

fn scheduling_header(
    work_item: &WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    wait_states: &WorkItemWaitFacts,
) -> SchedulingHeader {
    let (scheduling_state, reason_code) = if work_item.state == WorkItemState::Completed {
        (
            WorkItemSchedulingState::Completed,
            WorkItemSchedulingReasonCode::Completed,
        )
    } else if work_item.state == WorkItemState::Completing {
        (
            WorkItemSchedulingState::Completing,
            WorkItemSchedulingReasonCode::Completing,
        )
    } else if is_yielded {
        (
            WorkItemSchedulingState::YieldedToWorkItem,
            WorkItemSchedulingReasonCode::ContinuationYielded,
        )
    } else if let Some(wait_state) = wait_states.scheduling_state() {
        let reason = match wait_state {
            WorkItemSchedulingState::WaitingTask => WorkItemSchedulingReasonCode::ActiveTaskWait,
            WorkItemSchedulingState::WaitingOperator => {
                WorkItemSchedulingReasonCode::ActiveOperatorWait
            }
            WorkItemSchedulingState::WaitingTimer => WorkItemSchedulingReasonCode::ActiveTimerWait,
            WorkItemSchedulingState::WaitingExternal => {
                WorkItemSchedulingReasonCode::ActiveExternalWait
            }
            WorkItemSchedulingState::WaitingSystem => {
                WorkItemSchedulingReasonCode::ActiveSystemWait
            }
            _ => unreachable!("active waits only project waiting states"),
        };
        (wait_state, reason)
    } else {
        let state = work_item.record_scheduling_state();
        let reason = match state {
            WorkItemSchedulingState::Blocked => WorkItemSchedulingReasonCode::ManualBlocker,
            WorkItemSchedulingState::WaitingOperator => {
                WorkItemSchedulingReasonCode::PlanNeedsInput
            }
            _ => WorkItemSchedulingReasonCode::Runnable,
        };
        (state, reason)
    };
    let readiness = readiness_for_scheduling_state(scheduling_state);
    let is_current = is_current && work_item.state == WorkItemState::Open;
    let focus = if work_item.state == WorkItemState::Completed {
        WorkItemFocus::Completed
    } else if work_item.state == WorkItemState::Completing {
        WorkItemFocus::Completing
    } else if is_current {
        WorkItemFocus::Current
    } else if is_yielded {
        WorkItemFocus::Yielded
    } else if work_item.blocked_by.is_some() {
        WorkItemFocus::Blocked
    } else {
        WorkItemFocus::Queued
    };
    let has_triggered_waits = wait_states.last_triggered_at.is_some();
    let candidate_class = if is_current && scheduling_state == WorkItemSchedulingState::Runnable {
        WorkItemCandidateClass::CurrentRunnable
    } else if work_item.state == WorkItemState::Completed {
        WorkItemCandidateClass::CompletedRecent
    } else if work_item.state == WorkItemState::Completing {
        WorkItemCandidateClass::Completing
    } else if has_triggered_waits && work_item.blocked_by.is_some() {
        WorkItemCandidateClass::TriggeredBlocked
    } else if scheduling_state == WorkItemSchedulingState::Runnable {
        WorkItemCandidateClass::QueuedRunnable
    } else if scheduling_state == WorkItemSchedulingState::YieldedToWorkItem {
        WorkItemCandidateClass::Yielded
    } else if scheduling_state == WorkItemSchedulingState::WaitingOperator {
        WorkItemCandidateClass::WaitingForOperator
    } else {
        WorkItemCandidateClass::Blocked
    };

    SchedulingHeader {
        scheduling_state,
        reason_code,
        readiness,
        is_current,
        focus,
        has_triggered_waits,
        candidate_class,
    }
}

pub fn record_only_readiness(record: &WorkItemRecord) -> WorkItemReadiness {
    record.readiness()
}

/// Derive readiness from the facts available to a transition or audit path.
///
/// Unlike [`WorkItemRecord::readiness`], this preserves active wait and
/// continuation-yield context when the caller has an in-memory snapshot.
pub fn readiness_from_facts(
    record: &WorkItemRecord,
    is_yielded: bool,
    active_wait_conditions: &[WaitConditionRecord],
) -> WorkItemReadiness {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: record,
        is_current: false,
        is_yielded,
        active_wait_conditions,
        trigger_delivery_by_id: &BTreeMap::new(),
    })
    .readiness
}

pub fn compare_scheduling_projection_order<W: Borrow<WorkItemRecord>>(
    left: &WorkItemSchedulingProjection<W>,
    right: &WorkItemSchedulingProjection<W>,
) -> std::cmp::Ordering {
    compare_queue_work_rank(
        &QueueWorkRank {
            work_item: left.work_item.borrow(),
            candidate_class: left.candidate_class,
            scheduling_state: left.scheduling_state,
            has_active_waits: left.has_active_waits,
            last_triggered_at: left.last_triggered_at,
        },
        &QueueWorkRank {
            work_item: right.work_item.borrow(),
            candidate_class: right.candidate_class,
            scheduling_state: right.scheduling_state,
            has_active_waits: right.has_active_waits,
            last_triggered_at: right.last_triggered_at,
        },
    )
}

pub struct QueueWorkRank<'a> {
    pub work_item: &'a WorkItemRecord,
    pub candidate_class: WorkItemCandidateClass,
    pub scheduling_state: WorkItemSchedulingState,
    pub has_active_waits: bool,
    pub last_triggered_at: Option<DateTime<Utc>>,
}

pub fn queue_work_rank<'a>(
    work_item: &'a WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    waits: impl Iterator<Item = &'a WaitConditionRecord>,
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> QueueWorkRank<'a> {
    let mut states = WorkItemWaitFacts::default();
    for wait in waits.filter(|wait| wait.work_item_id.as_deref() == Some(work_item.id.as_str())) {
        states.record(wait, trigger_delivery);
    }
    let header = scheduling_header(work_item, is_current, is_yielded, &states);
    QueueWorkRank {
        work_item,
        candidate_class: header.candidate_class,
        scheduling_state: header.scheduling_state,
        has_active_waits: states.scheduling_state().is_some(),
        last_triggered_at: states.last_triggered_at,
    }
}

pub fn compare_queue_work_rank(
    left: &QueueWorkRank<'_>,
    right: &QueueWorkRank<'_>,
) -> std::cmp::Ordering {
    candidate_class_rank(left.candidate_class)
        .cmp(&candidate_class_rank(right.candidate_class))
        .then_with(|| match left.candidate_class {
            WorkItemCandidateClass::TriggeredBlocked => {
                compare_timestamp_desc_option(left.last_triggered_at, right.last_triggered_at)
                    .then_with(|| {
                        compare_timestamp_desc(
                            left.work_item.updated_at,
                            right.work_item.updated_at,
                        )
                    })
            }
            WorkItemCandidateClass::QueuedRunnable => {
                left.work_item.created_at.cmp(&right.work_item.created_at)
            }
            WorkItemCandidateClass::Yielded
            | WorkItemCandidateClass::WaitingForOperator
            | WorkItemCandidateClass::Blocked
            | WorkItemCandidateClass::Completing
            | WorkItemCandidateClass::CompletedRecent => {
                compare_timestamp_desc(left.work_item.updated_at, right.work_item.updated_at)
            }
            WorkItemCandidateClass::CurrentRunnable => std::cmp::Ordering::Equal,
        })
        .then_with(|| left.work_item.id.cmp(&right.work_item.id))
}

pub fn compare_queue_display_order(
    left: &WorkItemRecord,
    right: &WorkItemRecord,
) -> std::cmp::Ordering {
    u8::from(left.blocked_by.is_some())
        .cmp(&u8::from(right.blocked_by.is_some()))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.updated_at.cmp(&right.updated_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub fn candidate_class_rank(class: WorkItemCandidateClass) -> u8 {
    match class {
        WorkItemCandidateClass::CurrentRunnable => 0,
        WorkItemCandidateClass::TriggeredBlocked => 1,
        WorkItemCandidateClass::QueuedRunnable => 2,
        WorkItemCandidateClass::Yielded => 3,
        WorkItemCandidateClass::WaitingForOperator => 4,
        WorkItemCandidateClass::Blocked => 5,
        WorkItemCandidateClass::Completing => 6,
        WorkItemCandidateClass::CompletedRecent => 7,
    }
}

fn compare_timestamp_desc(left: DateTime<Utc>, right: DateTime<Utc>) -> std::cmp::Ordering {
    right.cmp(&left)
}

fn compare_timestamp_desc_option(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> std::cmp::Ordering {
    right.cmp(&left)
}

pub fn current_todo(record: &WorkItemRecord) -> Option<TodoItem> {
    current_todo_ref(record).cloned()
}

pub fn current_todo_ref(record: &WorkItemRecord) -> Option<&TodoItem> {
    record
        .todo_list
        .iter()
        .find(|item| item.state == TodoItemState::InProgress)
        .or_else(|| {
            record
                .todo_list
                .iter()
                .find(|item| item.state == TodoItemState::Pending)
        })
}
