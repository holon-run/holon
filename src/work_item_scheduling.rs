//! Shared WorkItem scheduling projection and queue read model.

use std::{
    borrow::{Borrow, Cow},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

mod context_queue;
pub(crate) use context_queue::ContextWorkQueue;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{
    TodoItem, TodoItemState, WaitConditionKind, WaitConditionRecord, WaitConditionSummary,
    WakeSource, WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord, WorkItemSchedulingState,
    WorkItemState,
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

    pub(crate) fn posture_reason_display(&self) -> impl std::fmt::Display + '_ {
        WorkItemPostureReason {
            id: &self.record().id,
            is_current: self.is_current,
            scheduling_state: self.scheduling_state,
        }
    }
}

pub(crate) struct WorkItemPostureReason<'a> {
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

#[derive(Debug, Clone)]
pub struct WorkQueueReadModel<W = WorkItemRecord> {
    pub current: Option<W>,
    pub queued_blocked: Vec<W>,
    pub items: Vec<WorkItemSchedulingProjection<W>>,
    pub current_runnable: Option<WorkItemSchedulingProjection<W>>,
    pub triggered_blocked: Vec<WorkItemSchedulingProjection<W>>,
    pub queued_runnable: Vec<WorkItemSchedulingProjection<W>>,
    pub yielded: Vec<WorkItemSchedulingProjection<W>>,
    pub waiting_for_operator: Vec<WorkItemSchedulingProjection<W>>,
    pub blocked: Vec<WorkItemSchedulingProjection<W>>,
    pub completing: Vec<WorkItemSchedulingProjection<W>>,
    pub completed_recent: Vec<WorkItemSchedulingProjection<W>>,
}

impl<W> Default for WorkQueueReadModel<W> {
    fn default() -> Self {
        Self {
            current: None,
            queued_blocked: Vec::new(),
            items: Vec::new(),
            current_runnable: None,
            triggered_blocked: Vec::new(),
            queued_runnable: Vec::new(),
            yielded: Vec::new(),
            waiting_for_operator: Vec::new(),
            blocked: Vec::new(),
            completing: Vec::new(),
            completed_recent: Vec::new(),
        }
    }
}

impl<W> WorkItemSchedulingProjection<W> {
    pub(crate) fn try_map_record<R>(
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

impl WorkItemRecord {
    pub fn readiness(&self) -> WorkItemReadiness {
        record_only_readiness(self)
    }

    pub fn is_runnable(&self) -> bool {
        self.readiness() == WorkItemReadiness::Runnable
    }

    pub fn is_waiting_for_operator(&self) -> bool {
        self.readiness() == WorkItemReadiness::WaitingForOperator
    }
}

pub struct WorkItemSchedulingFacts<'a> {
    pub work_item: &'a WorkItemRecord,
    pub is_current: bool,
    pub is_yielded: bool,
    pub active_wait_conditions: &'a [WaitConditionRecord],
    pub trigger_delivery_by_id: &'a BTreeMap<String, DateTime<Utc>>,
}

#[derive(Debug, Default)]
struct ActiveWaitConditionStates {
    task: bool,
    external: bool,
    operator: bool,
    timer: bool,
    system: bool,
    last_triggered_at: Option<DateTime<Utc>>,
}

impl ActiveWaitConditionStates {
    fn record(
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

    fn record_kind(&mut self, kind: &WaitConditionKind) {
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
    derive_work_item_scheduling_with_record(
        Cow::Borrowed(facts.work_item),
        facts.is_current,
        facts.is_yielded,
        facts.active_wait_conditions,
        &|id| facts.trigger_delivery_by_id.get(id).copied(),
    )
}

#[cfg(test)]
pub(crate) fn derive_owned_work_item_scheduling(
    work_item: WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    active_wait_conditions: &[WaitConditionRecord],
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> WorkItemSchedulingProjection {
    derive_work_item_scheduling_with_record(
        Cow::Owned(work_item),
        is_current,
        is_yielded,
        active_wait_conditions,
        trigger_delivery,
    )
}

pub(crate) fn derive_queue_work_item_scheduling(
    work_item: WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    active_wait_conditions: &[WaitConditionRecord],
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> anyhow::Result<WorkItemSchedulingProjection> {
    derive_borrowed_queue_work_item_scheduling(
        work_item,
        is_current,
        is_yielded,
        active_wait_conditions.iter(),
        trigger_delivery,
    )
}

pub(crate) fn derive_borrowed_queue_work_item_scheduling<'a, W: Borrow<WorkItemRecord>>(
    work_item: W,
    is_current: bool,
    is_yielded: bool,
    active_wait_conditions: impl Iterator<Item = &'a WaitConditionRecord> + Clone,
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> anyhow::Result<WorkItemSchedulingProjection<W>> {
    use anyhow::Context;

    let record = work_item.borrow();
    if let Some(todo) = current_todo_ref(record) {
        crate::diagnostics::resource_io::admit_cloned_value(todo)
            .context("queue current todo clone admission")?;
    }
    let mut wait_states = ActiveWaitConditionStates::default();
    let mut has_waits = false;
    for wait in active_wait_conditions
        .clone()
        .filter(|wait| wait.work_item_id.as_deref() == Some(record.id.as_str()))
    {
        has_waits = true;
        wait_states.record_kind(&wait.kind);
    }
    let messages =
        scheduling_diagnostic_messages(record, is_current, is_yielded, has_waits, &wait_states);
    let count = messages.iter().flatten().count();
    if count != 0 {
        // At most four static messages: one String slot and text allocation each.
        let bytes = count * std::mem::size_of::<String>()
            + messages
                .iter()
                .flatten()
                .map(|message| message.len())
                .sum::<usize>();
        crate::diagnostics::resource_io::charge_owned_metadata(bytes as u64, count * 2)
            .context("queue diagnostics admission")?;
    }
    let (wait_states, summaries) =
        queue_wait_summaries(&record.id, active_wait_conditions, trigger_delivery)?;
    Ok(derive_scheduling_with_shared_record(
        work_item,
        is_current,
        is_yielded,
        wait_states,
        summaries,
    ))
}

fn queue_wait_summaries<'a>(
    work_item_id: &str,
    conditions: impl Iterator<Item = &'a WaitConditionRecord> + Clone,
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> anyhow::Result<(ActiveWaitConditionStates, Vec<WaitConditionSummary>)> {
    use anyhow::Context;

    let count = conditions
        .clone()
        .filter(|wait| wait.work_item_id.as_deref() == Some(work_item_id))
        .count();
    let mut ordered = Vec::new();
    let mut summaries = Vec::new();
    if count == 0 {
        return Ok((ActiveWaitConditionStates::default(), summaries));
    }
    reserve_queue_wait_slots::<(usize, &WaitConditionRecord)>(
        &mut ordered,
        count,
        "queue wait references admission",
    )?;
    for (ordinal, wait) in conditions.enumerate() {
        if wait.work_item_id.as_deref() == Some(work_item_id) {
            ordered.push((ordinal, wait));
        }
    }
    // Input ordinal preserves the former stable sort even for equal keys.
    ordered.sort_unstable_by(|left, right| {
        left.1
            .created_at
            .cmp(&right.1.created_at)
            .then_with(|| left.1.id.cmp(&right.1.id))
            .then_with(|| left.0.cmp(&right.0))
    });
    reserve_queue_wait_slots(&mut summaries, count, "queue wait summary slots admission")?;
    let mut states = ActiveWaitConditionStates::default();
    for (_, wait) in ordered {
        // Only the owning fields retained by the summary are cloned. Scalars
        // and dropped record-only fields need no material allocation.
        crate::diagnostics::resource_io::admit_cloned_value(&(
            &wait.id,
            &wait.work_item_id,
            &wait.source,
            &wait.subject_ref,
            &wait.waiting_for,
            &wait.wake_sources,
            wait.no_fallback_reason_ref(),
            &wait.continuation,
        ))
        .context("queue wait summary clone admission")?;
        states.record(wait, trigger_delivery);
        summaries.push(WaitConditionSummary::from(wait));
    }
    Ok((states, summaries))
}

fn reserve_queue_wait_slots<T>(
    rows: &mut Vec<T>,
    count: usize,
    context: &'static str,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue wait slots"))?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, count).context(context)?;
    rows.try_reserve_exact(count)
        .map_err(|_| std::io::Error::other("ResourceExhausted: queue wait allocation"))
        .context(context)
}

fn scheduling_diagnostic_messages(
    work_item: &WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    has_waits: bool,
    wait_states: &ActiveWaitConditionStates,
) -> [Option<&'static str>; 4] {
    [
        (work_item.state != WorkItemState::Open && is_current)
            .then_some("terminalizing_work_item_marked_current"),
        (is_yielded && is_current).then_some("yielded_work_item_marked_current"),
        (work_item.state != WorkItemState::Open && has_waits)
            .then_some("terminalizing_work_item_has_active_wait"),
        (wait_states.kind_count() > 1).then_some("multiple_active_wait_kinds"),
    ]
}

fn derive_work_item_scheduling_with_record(
    work_item: Cow<'_, WorkItemRecord>,
    is_current: bool,
    is_yielded: bool,
    active_wait_conditions: &[WaitConditionRecord],
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> WorkItemSchedulingProjection {
    let mut waits = active_wait_conditions
        .iter()
        .filter(|condition| condition.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    waits.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut wait_states = ActiveWaitConditionStates::default();
    for wait in &waits {
        wait_states.record(wait, trigger_delivery);
    }
    let summaries = waits.into_iter().map(WaitConditionSummary::from).collect();
    derive_work_item_scheduling_with_summaries(
        work_item,
        is_current,
        is_yielded,
        wait_states,
        summaries,
    )
}

fn derive_work_item_scheduling_with_summaries(
    work_item: Cow<'_, WorkItemRecord>,
    is_current: bool,
    is_yielded: bool,
    wait_states: ActiveWaitConditionStates,
    summaries: Vec<WaitConditionSummary>,
) -> WorkItemSchedulingProjection {
    derive_scheduling_with_shared_record(
        work_item.into_owned(),
        is_current,
        is_yielded,
        wait_states,
        summaries,
    )
}

fn derive_scheduling_with_shared_record<W: Borrow<WorkItemRecord>>(
    work_item: W,
    is_current: bool,
    is_yielded: bool,
    wait_states: ActiveWaitConditionStates,
    summaries: Vec<WaitConditionSummary>,
) -> WorkItemSchedulingProjection<W> {
    let record = work_item.borrow();
    let messages = scheduling_diagnostic_messages(
        record,
        is_current,
        is_yielded,
        !summaries.is_empty(),
        &wait_states,
    );
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
    wait_states: &ActiveWaitConditionStates,
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
    } else if work_item.blocked_by.is_some() {
        (
            WorkItemSchedulingState::Blocked,
            WorkItemSchedulingReasonCode::ManualBlocker,
        )
    } else if work_item.plan_status == WorkItemPlanStatus::NeedsInput {
        (
            WorkItemSchedulingState::WaitingOperator,
            WorkItemSchedulingReasonCode::PlanNeedsInput,
        )
    } else {
        (
            WorkItemSchedulingState::Runnable,
            WorkItemSchedulingReasonCode::Runnable,
        )
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
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: record,
        is_current: false,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &BTreeMap::new(),
    })
    .readiness
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

pub fn readiness_for_scheduling_state(state: WorkItemSchedulingState) -> WorkItemReadiness {
    match state {
        WorkItemSchedulingState::Runnable => WorkItemReadiness::Runnable,
        WorkItemSchedulingState::YieldedToWorkItem => WorkItemReadiness::Yielded,
        WorkItemSchedulingState::WaitingOperator => WorkItemReadiness::WaitingForOperator,
        WorkItemSchedulingState::WaitingTask
        | WorkItemSchedulingState::WaitingExternal
        | WorkItemSchedulingState::WaitingTimer
        | WorkItemSchedulingState::WaitingSystem
        | WorkItemSchedulingState::Blocked => WorkItemReadiness::Blocked,
        WorkItemSchedulingState::Completing => WorkItemReadiness::Completing,
        WorkItemSchedulingState::Completed => WorkItemReadiness::Completed,
    }
}

pub fn compare_scheduling_projection_order(
    left: &WorkItemSchedulingProjection,
    right: &WorkItemSchedulingProjection,
) -> std::cmp::Ordering {
    compare_queue_work_rank(
        &QueueWorkRank {
            work_item: &left.work_item,
            candidate_class: left.candidate_class,
            scheduling_state: left.scheduling_state,
            has_active_waits: left.has_active_waits,
            last_triggered_at: left.last_triggered_at,
        },
        &QueueWorkRank {
            work_item: &right.work_item,
            candidate_class: right.candidate_class,
            scheduling_state: right.scheduling_state,
            has_active_waits: right.has_active_waits,
            last_triggered_at: right.last_triggered_at,
        },
    )
}

pub(crate) struct QueueWorkRank<'a> {
    pub work_item: &'a WorkItemRecord,
    pub candidate_class: WorkItemCandidateClass,
    pub scheduling_state: WorkItemSchedulingState,
    pub has_active_waits: bool,
    pub last_triggered_at: Option<DateTime<Utc>>,
}

pub(crate) fn queue_work_rank<'a>(
    work_item: &'a WorkItemRecord,
    is_current: bool,
    is_yielded: bool,
    waits: impl Iterator<Item = &'a WaitConditionRecord>,
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> QueueWorkRank<'a> {
    let mut states = ActiveWaitConditionStates::default();
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

pub(crate) fn compare_queue_work_rank(
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

fn current_todo_ref(record: &WorkItemRecord) -> Option<&TodoItem> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WaitConditionStatus, WakeSource};

    fn wait(record: &WorkItemRecord, kind: WaitConditionKind) -> WaitConditionRecord {
        let now = Utc::now();
        WaitConditionRecord {
            id: format!("wait-{kind:?}"),
            agent_id: record.agent_id.clone(),
            work_item_id: Some(record.id.clone()),
            status: WaitConditionStatus::Active,
            kind,
            source: Some("test".into()),
            subject_ref: None,
            waiting_for: "test".into(),
            wake_sources: vec![WakeSource::SystemTick],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        }
    }

    fn derive(
        record: &WorkItemRecord,
        waits: &[WaitConditionRecord],
    ) -> WorkItemSchedulingProjection {
        derive_work_item_scheduling(WorkItemSchedulingFacts {
            work_item: record,
            is_current: false,
            is_yielded: false,
            active_wait_conditions: waits,
            trigger_delivery_by_id: &BTreeMap::new(),
        })
    }

    #[test]
    fn golden_precedence_active_wait_outranks_needs_input_and_blocker() {
        let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
        record.plan_status = WorkItemPlanStatus::NeedsInput;
        record.blocked_by = Some("manual".into());
        let waits = [wait(&record, WaitConditionKind::Task)];

        let projection = derive(&record, &waits);

        assert_eq!(
            projection.scheduling_state,
            WorkItemSchedulingState::WaitingTask
        );
        assert_eq!(
            projection.reason_code,
            WorkItemSchedulingReasonCode::ActiveTaskWait
        );
    }

    #[test]
    fn record_only_readiness_is_distinct_from_fact_backed_readiness() {
        let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
        record.blocked_by = Some("manual".into());

        for (kind, expected) in [
            (
                WaitConditionKind::Operator,
                WorkItemReadiness::WaitingForOperator,
            ),
            (WaitConditionKind::Task, WorkItemReadiness::Blocked),
            (WaitConditionKind::Timer, WorkItemReadiness::Blocked),
            (WaitConditionKind::External, WorkItemReadiness::Blocked),
            (WaitConditionKind::System, WorkItemReadiness::Blocked),
        ] {
            let waits = [wait(&record, kind)];
            assert_eq!(record.readiness(), WorkItemReadiness::Blocked);
            assert_eq!(readiness_from_facts(&record, false, &waits), expected);
        }
    }

    #[test]
    fn golden_precedence_completed_is_terminal() {
        let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Completed);
        record.plan_status = WorkItemPlanStatus::Ready;
        let waits = [wait(&record, WaitConditionKind::Operator)];

        let projection = derive(&record, &waits);

        assert_eq!(
            projection.scheduling_state,
            WorkItemSchedulingState::Completed
        );
        assert_eq!(projection.readiness, WorkItemReadiness::Completed);
        assert!(!projection.is_runnable);
        assert!(projection
            .diagnostics
            .contains(&"terminalizing_work_item_has_active_wait".to_string()));
    }

    #[test]
    fn multiple_wait_kinds_are_deterministic_and_diagnostic() {
        let record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
        let waits = [
            wait(&record, WaitConditionKind::External),
            wait(&record, WaitConditionKind::Operator),
            wait(&record, WaitConditionKind::Task),
        ];

        let projection = derive(&record, &waits);

        assert_eq!(
            projection.scheduling_state,
            WorkItemSchedulingState::WaitingTask
        );
        assert!(projection
            .diagnostics
            .contains(&"multiple_active_wait_kinds".to_string()));
    }

    #[test]
    fn yielded_is_never_queued_runnable() {
        let record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
        let projection = derive_work_item_scheduling(WorkItemSchedulingFacts {
            work_item: &record,
            is_current: false,
            is_yielded: true,
            active_wait_conditions: &[],
            trigger_delivery_by_id: &BTreeMap::new(),
        });

        assert_eq!(
            projection.scheduling_state,
            WorkItemSchedulingState::YieldedToWorkItem
        );
        assert_eq!(projection.candidate_class, WorkItemCandidateClass::Yielded);
        assert!(!projection.is_runnable);
    }

    #[test]
    fn transition_matrix_preserves_terminal_and_wait_invariants() {
        let plan_statuses = [
            WorkItemPlanStatus::Draft,
            WorkItemPlanStatus::Ready,
            WorkItemPlanStatus::NeedsInput,
        ];
        let wait_kinds = [
            None,
            Some(WaitConditionKind::Task),
            Some(WaitConditionKind::Operator),
            Some(WaitConditionKind::Timer),
            Some(WaitConditionKind::External),
            Some(WaitConditionKind::System),
        ];

        for state in [WorkItemState::Open, WorkItemState::Completed] {
            for plan_status in plan_statuses {
                for blocked in [false, true] {
                    for yielded in [false, true] {
                        for wait_kind in &wait_kinds {
                            let mut record = WorkItemRecord::new("agent", "matrix", state.clone());
                            record.plan_status = plan_status;
                            record.blocked_by = blocked.then(|| "blocked".into());
                            let waits = wait_kind
                                .clone()
                                .map(|kind| vec![wait(&record, kind)])
                                .unwrap_or_default();
                            let projection = derive_work_item_scheduling(WorkItemSchedulingFacts {
                                work_item: &record,
                                is_current: false,
                                is_yielded: yielded,
                                active_wait_conditions: &waits,
                                trigger_delivery_by_id: &BTreeMap::new(),
                            });

                            if state == WorkItemState::Completed {
                                assert_eq!(
                                    projection.scheduling_state,
                                    WorkItemSchedulingState::Completed
                                );
                                assert_eq!(projection.readiness, WorkItemReadiness::Completed);
                                assert!(!projection.is_runnable);
                            } else if yielded {
                                assert_eq!(
                                    projection.scheduling_state,
                                    WorkItemSchedulingState::YieldedToWorkItem
                                );
                                assert_eq!(
                                    projection.candidate_class,
                                    WorkItemCandidateClass::Yielded
                                );
                            } else if let Some(kind) = wait_kind.as_ref() {
                                let expected = match kind {
                                    WaitConditionKind::Task => WorkItemSchedulingState::WaitingTask,
                                    WaitConditionKind::Operator => {
                                        WorkItemSchedulingState::WaitingOperator
                                    }
                                    WaitConditionKind::Timer => {
                                        WorkItemSchedulingState::WaitingTimer
                                    }
                                    WaitConditionKind::External => {
                                        WorkItemSchedulingState::WaitingExternal
                                    }
                                    WaitConditionKind::System => {
                                        WorkItemSchedulingState::WaitingSystem
                                    }
                                };
                                assert_eq!(projection.scheduling_state, expected);
                            }
                        }
                    }
                }
            }
        }
    }
}
