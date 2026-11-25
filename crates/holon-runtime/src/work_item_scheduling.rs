//! Runtime assembly of WorkItem scheduling projections within read budgets.

use std::borrow::Borrow;

use chrono::{DateTime, Utc};

use crate::types::{WaitConditionRecord, WaitConditionSummary, WorkItemRecord};
use holon_domain::work_item_scheduling::{
    compare_wait_condition_order, current_todo_ref, project_work_item_with_wait_summaries,
    WorkItemWaitFacts,
};

mod context_queue;
pub(crate) use context_queue::ContextWorkQueue;

pub use holon_domain::work_item::readiness_for_scheduling_state;
#[cfg(test)]
pub(crate) use holon_domain::work_item_scheduling::project_work_item_scheduling as derive_owned_work_item_scheduling;
pub use holon_domain::work_item_scheduling::{
    candidate_class_rank, compare_queue_display_order, compare_scheduling_projection_order,
    current_todo, derive_work_item_scheduling, readiness_from_facts, record_only_readiness,
    WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingFacts, WorkItemSchedulingProjection,
    WorkItemSchedulingReasonCode,
};
pub(crate) use holon_domain::work_item_scheduling::{
    compare_queue_work_rank, queue_work_rank, QueueWorkRank, WorkItemPostureReason,
};

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
        crate::diagnostics::resource_io::admit_typed_cloned_value(todo)
            .context("queue current todo clone admission")?;
    }
    let active_wait_conditions = active_wait_conditions
        .filter(|wait| wait.work_item_id.as_deref() == Some(record.id.as_str()));
    let mut wait_states = WorkItemWaitFacts::default();
    for wait in active_wait_conditions.clone() {
        wait_states.record_kind(&wait.kind);
    }
    let messages = wait_states.diagnostic_messages(record, is_current, is_yielded);
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
    let (wait_states, summaries) = queue_wait_summaries(active_wait_conditions, trigger_delivery)?;
    Ok(project_work_item_with_wait_summaries(
        work_item,
        is_current,
        is_yielded,
        wait_states,
        summaries,
    ))
}

fn queue_wait_summaries<'a>(
    conditions: impl Iterator<Item = &'a WaitConditionRecord> + Clone,
    trigger_delivery: &impl Fn(&str) -> Option<DateTime<Utc>>,
) -> anyhow::Result<(WorkItemWaitFacts, Vec<WaitConditionSummary>)> {
    use anyhow::Context;

    let count = conditions.clone().count();
    let mut ordered = Vec::new();
    let mut summaries = Vec::new();
    if count == 0 {
        return Ok((WorkItemWaitFacts::default(), summaries));
    }
    reserve_queue_wait_slots::<(usize, &WaitConditionRecord)>(
        &mut ordered,
        count,
        "queue wait references admission",
    )?;
    ordered.extend(conditions.enumerate());
    // Input ordinal preserves the former stable sort even for equal keys.
    ordered.sort_unstable_by(|left, right| {
        compare_wait_condition_order(left.1, right.1).then_with(|| left.0.cmp(&right.0))
    });
    reserve_queue_wait_slots(&mut summaries, count, "queue wait summary slots admission")?;
    let mut states = WorkItemWaitFacts::default();
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
