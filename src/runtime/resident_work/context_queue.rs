use super::*;
use crate::work_item_scheduling::{
    compare_queue_work_rank, derive_borrowed_queue_work_item_scheduling, queue_work_rank,
    ContextWorkQueue, QueueWorkRank, WorkItemCandidateClass, WorkItemSchedulingProjection,
};

impl CapturedWork {
    pub(crate) fn context_work_queue_with_material(
        &self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<ContextWorkQueue> {
        let _entry = material.enter();
        let ranked = self.queue_work_ranks(state, triggers)?;
        let materialize =
            |rank: &QueueWorkRank<'_>| self.materialize_queue_work(state, triggers, rank.work_item);
        let select = |class, limit| -> Result<Vec<WorkItemSchedulingProjection>> {
            let rows = ranked
                .iter()
                .filter(|rank| {
                    rank.candidate_class == class
                        && (class != WorkItemCandidateClass::Blocked
                            || rank.scheduling_state
                                != crate::types::WorkItemSchedulingState::WaitingTask)
                })
                .take(limit);
            let mut selected = context_slots(rows.clone().count())?;
            for rank in rows {
                selected.push(materialize(rank)?);
            }
            Ok(selected)
        };
        Ok(ContextWorkQueue {
            current: ranked
                .iter()
                .find(|rank| {
                    rank.work_item.state == WorkItemState::Open
                        && state.current_work_item_id.as_deref() == Some(rank.work_item.id.as_str())
                })
                .map(|rank| clone_queue_work_record(rank.work_item))
                .transpose()?,
            current_runnable: ranked
                .iter()
                .find(|rank| rank.candidate_class == WorkItemCandidateClass::CurrentRunnable)
                .map(materialize)
                .transpose()?,
            triggered_blocked: select(WorkItemCandidateClass::TriggeredBlocked, 3)?,
            queued_runnable: select(WorkItemCandidateClass::QueuedRunnable, 5)?,
            yielded: select(WorkItemCandidateClass::Yielded, 5)?,
            waiting_for_operator: select(WorkItemCandidateClass::WaitingForOperator, 3)?,
            blocked: select(WorkItemCandidateClass::Blocked, 3)?,
            completing: select(WorkItemCandidateClass::Completing, 3)?,
            completed_recent: select(WorkItemCandidateClass::CompletedRecent, 3)?,
        })
    }

    pub(crate) fn queue_work_ranks<'a>(
        &'a self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
    ) -> Result<Vec<QueueWorkRank<'a>>> {
        ensure!(
            state.id == self.scope.agent_id,
            "Work projection State scope mismatch"
        );
        if !self.live_complete {
            return Err(WorkReadError::IncompleteLive.into());
        }
        if self.nonopen.limit < WORK_NONOPEN_WINDOW && !self.nonopen.is_exhaustive {
            return Err(WorkReadError::WindowExpansion {
                requested: WORK_NONOPEN_WINDOW,
                prepared: self.nonopen.limit,
            }
            .into());
        }
        if let Some(id) = &state.current_work_item_id {
            self.lookup(&WorkKey::new(WorkFamily::Work, id))?;
        }
        ensure!(
            triggers
                .iter()
                .all(|trigger| trigger.target_agent_id == self.scope.agent_id),
            "Work trigger adapter scope mismatch"
        );
        let displayed = queue_displayed_work_ids(self)?;
        let works = self
            .records
            .iter()
            .filter_map(|evidence| match &evidence.record {
                CanonicalWorkRecord::Work(work)
                    if displayed.binary_search(&work.id.as_str()).is_ok() =>
                {
                    Some(work)
                }
                _ => None,
            });
        let active_waits = self.queue_active_waits();
        let mut waits = context_slots(active_waits.clone().count())?;
        waits.extend(active_waits);
        let yielded = |id: &str| {
            self.records.iter().any(|evidence| {
                matches!(&evidence.record, CanonicalWorkRecord::Frame(frame)
                    if self.active_frame_ids.contains(&frame.id)
                        && frame.suspended_work_item_id == id)
            })
        };
        let delivery = |id: &str| {
            triggers
                .iter()
                .filter(|trigger| {
                    trigger.external_trigger_id == id
                        && trigger.status == crate::types::ExternalTriggerStatus::Active
                })
                .filter_map(|trigger| trigger.last_delivered_at)
                .max()
        };
        // Rank the complete qualified inventory without owning its bodies.
        let mut ranked = context_slots(works.clone().count())?;
        for work in works {
            ranked.push(queue_work_rank(
                work,
                state.current_work_item_id.as_deref() == Some(work.id.as_str()),
                work.state == WorkItemState::Open && yielded(&work.id),
                waits.iter().copied(),
                &delivery,
            ));
        }
        ranked.sort_unstable_by(compare_queue_work_rank);
        Ok(ranked)
    }

    pub(super) fn queue_active_waits(&self) -> impl Iterator<Item = &WaitConditionRecord> + Clone {
        self.records.iter().filter_map(|evidence| {
            let CanonicalWorkRecord::Wait(wait) = &evidence.record else {
                return None;
            };
            (self.unresolved_wait_ids.contains(&wait.id)
                && wait.status == WaitConditionStatus::Active
                && self.records.iter().any(|evidence| {
                    matches!(&evidence.record, CanonicalWorkRecord::Work(work)
                        if work.state == WorkItemState::Open
                            && wait.work_item_id.as_deref() == Some(work.id.as_str()))
                }))
            .then_some(wait)
        })
    }

    pub(super) fn materialize_queue_work(
        &self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
        work: &WorkItemRecord,
    ) -> Result<WorkItemSchedulingProjection> {
        let yielded = work.state == WorkItemState::Open
            && self.records.iter().any(|evidence| {
                matches!(&evidence.record, CanonicalWorkRecord::Frame(frame)
                    if self.active_frame_ids.contains(&frame.id)
                        && frame.suspended_work_item_id == work.id)
            });
        derive_borrowed_queue_work_item_scheduling(
            clone_queue_work_record(work)?,
            state.current_work_item_id.as_deref() == Some(work.id.as_str()),
            yielded,
            self.queue_active_waits(),
            &|id| {
                triggers
                    .iter()
                    .filter(|trigger| {
                        trigger.external_trigger_id == id
                            && trigger.status == crate::types::ExternalTriggerStatus::Active
                    })
                    .filter_map(|trigger| trigger.last_delivered_at)
                    .max()
            },
        )
    }
}

pub(super) fn context_slots<T>(count: usize) -> Result<Vec<T>> {
    let bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| std::io::Error::other("ResourceExhausted: context queue slots"))?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, count)
        .context("context queue slots admission")?;
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(count)
        .map_err(|_| std::io::Error::other("ResourceExhausted: context queue allocation"))?;
    Ok(slots)
}
