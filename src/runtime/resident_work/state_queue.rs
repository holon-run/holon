//! Full State inventory shares qualified bodies; only projection metadata is owned.
use super::*;
use crate::work_item_scheduling::{
    compare_queue_display_order, derive_borrowed_queue_work_item_scheduling,
    WorkItemCandidateClass, WorkItemSchedulingProjection, WorkQueueReadModel,
};
use std::{borrow::Borrow, ops::Deref};

pub(crate) type StateWorkQueue = WorkQueueReadModel<StateWorkRecord>;

#[derive(Debug, Clone)]
enum StateWorkOwner {
    Resident(Arc<QualifiedWorkRecord>),
    Cold(Arc<WorkItemRecord>),
}

#[derive(Debug, Clone)]
pub(crate) struct StateWorkRecord {
    owner: StateWorkOwner,
    // Keep the captured pin accounting with every surviving record loan.
    _pins: Arc<Vec<Arc<BudgetViewGuard>>>,
    _acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
}

impl Deref for StateWorkRecord {
    type Target = WorkItemRecord;
    fn deref(&self) -> &Self::Target {
        match &self.owner {
            StateWorkOwner::Resident(record) => match &record.record {
                CanonicalWorkRecord::Work(work) => work,
                _ => unreachable!("State loan constructor accepts only Work records"),
            },
            StateWorkOwner::Cold(record) => record,
        }
    }
}

impl Borrow<WorkItemRecord> for StateWorkRecord {
    fn borrow(&self) -> &WorkItemRecord {
        self
    }
}

impl Serialize for StateWorkRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}

fn shared_slots<T>(count: usize) -> Result<Vec<T>> {
    super::context_queue::context_slots(count)
}

fn clone_projection(
    item: &WorkItemSchedulingProjection<StateWorkRecord>,
) -> Result<WorkItemSchedulingProjection<StateWorkRecord>> {
    // The source loan is shared. Admit only the metadata actually cloned here.
    crate::diagnostics::resource_io::admit_cloned_value(&(
        &item.current_todo,
        &item.active_wait_conditions,
        &item.diagnostics,
    ))?;
    Ok(item.clone())
}

impl CapturedWork {
    pub(crate) fn state_work_queue_with_material(
        &self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<StateWorkQueue> {
        let _entry = material.enter();
        let ranked = self.queue_work_ranks(state, triggers)?;
        let mut source = shared_slots(self.budget_views.len())?;
        source.extend(self.budget_views.iter().cloned());
        crate::diagnostics::resource_io::charge_owned_metadata(
            std::mem::size_of::<Vec<Arc<BudgetViewGuard>>>() as u64 + 2 * 16,
            1,
        )?;
        let pins = Arc::new(source);
        let mut sources = shared_slots(self.records.len())?;
        sources.extend(
            self.records
                .iter()
                .filter(|record| matches!(&record.record, CanonicalWorkRecord::Work(_))),
        );
        sources.sort_unstable_by(|left, right| left.key.id.cmp(&right.key.id));
        let yielded = |id: &str| {
            self.records.iter().any(|record| {
                matches!(&record.record, CanonicalWorkRecord::Frame(frame)
                    if self.active_frame_ids.contains(&frame.id)
                        && frame.suspended_work_item_id == id)
            })
        };
        let mut items = shared_slots(ranked.len())?;
        for rank in ranked {
            let index = sources
                .binary_search_by(|record| record.key.id.as_str().cmp(&rank.work_item.id))
                .map_err(|_| anyhow::anyhow!("State Work loan source differs"))?;
            let record = StateWorkRecord {
                owner: StateWorkOwner::Resident(sources[index].clone()),
                _pins: pins.clone(),
                _acquisition: material.clone(),
            };
            items.push(derive_borrowed_queue_work_item_scheduling(
                record,
                rank.work_item.state == WorkItemState::Open
                    && state.current_work_item_id.as_deref() == Some(rank.work_item.id.as_str()),
                rank.work_item.state == WorkItemState::Open && yielded(&rank.work_item.id),
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
            )?);
        }
        let select = |class, limit| -> Result<Vec<_>> {
            let rows = items
                .iter()
                .filter(|item| {
                    item.candidate_class == class
                        && (class != WorkItemCandidateClass::Blocked
                            || item.scheduling_state
                                != crate::types::WorkItemSchedulingState::WaitingTask)
                })
                .take(limit);
            let mut selected = shared_slots(rows.clone().count())?;
            for item in rows {
                selected.push(clone_projection(item)?);
            }
            Ok(selected)
        };
        let current = items
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.work_item.clone());
        let rows = items.iter().filter(|item| {
            item.state == WorkItemState::Open
                && state.current_work_item_id.as_deref() != Some(item.id.as_str())
                && !yielded(&item.id)
                && !item.has_active_task_waits
        });
        let mut queued_blocked = shared_slots(rows.clone().count())?;
        queued_blocked.extend(rows.map(|item| item.work_item.clone()));
        queued_blocked.sort_unstable_by(|left, right| compare_queue_display_order(left, right));
        crate::diagnostics::resource_io::charge_owned_metadata(
            std::mem::size_of::<StateWorkQueue>() as u64,
            1,
        )?;
        Ok(WorkQueueReadModel {
            current,
            queued_blocked,
            current_runnable: items
                .iter()
                .find(|item| item.candidate_class == WorkItemCandidateClass::CurrentRunnable)
                .map(clone_projection)
                .transpose()?,
            triggered_blocked: select(WorkItemCandidateClass::TriggeredBlocked, 3)?,
            queued_runnable: select(WorkItemCandidateClass::QueuedRunnable, 5)?,
            yielded: select(WorkItemCandidateClass::Yielded, 5)?,
            waiting_for_operator: select(WorkItemCandidateClass::WaitingForOperator, 3)?,
            blocked: select(WorkItemCandidateClass::Blocked, 3)?,
            completing: select(WorkItemCandidateClass::Completing, 3)?,
            completed_recent: select(WorkItemCandidateClass::CompletedRecent, 3)?,
            items,
        })
    }
}

impl WorkQueueReadModel {
    /// Cold adapters already own their records. Move them; do not decode or clone again.
    pub(crate) fn into_state_queue(
        self,
        acquisition: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<StateWorkQueue> {
        acquisition.read(|| {
            crate::diagnostics::resource_io::charge_owned_metadata(
                std::mem::size_of::<Vec<Arc<BudgetViewGuard>>>() as u64 + 2 * 16,
                1,
            )?;
            let pins = Arc::new(Vec::new());
            let record = |work: WorkItemRecord| -> Result<StateWorkRecord> {
                crate::diagnostics::resource_io::charge_owned_metadata(
                    std::mem::size_of::<WorkItemRecord>() as u64 + 2 * 16,
                    1,
                )?;
                Ok(StateWorkRecord {
                    owner: StateWorkOwner::Cold(Arc::new(work)),
                    _pins: pins.clone(),
                    _acquisition: acquisition.clone(),
                })
            };
            let projection =
                |item: WorkItemSchedulingProjection| -> Result<_> { item.try_map_record(&record) };
            let records = |rows: Vec<WorkItemRecord>| -> Result<Vec<_>> {
                let mut values = shared_slots(rows.len())?;
                for row in rows {
                    values.push(record(row)?);
                }
                Ok(values)
            };
            let projections = |rows: Vec<WorkItemSchedulingProjection>| -> Result<Vec<_>> {
                let mut values = shared_slots(rows.len())?;
                for row in rows {
                    values.push(projection(row)?);
                }
                Ok(values)
            };
            Ok(WorkQueueReadModel {
                current: self.current.map(&record).transpose()?,
                queued_blocked: records(self.queued_blocked)?,
                items: projections(self.items)?,
                current_runnable: self.current_runnable.map(projection).transpose()?,
                triggered_blocked: projections(self.triggered_blocked)?,
                queued_runnable: projections(self.queued_runnable)?,
                yielded: projections(self.yielded)?,
                waiting_for_operator: projections(self.waiting_for_operator)?,
                blocked: projections(self.blocked)?,
                completing: projections(self.completing)?,
                completed_recent: projections(self.completed_recent)?,
            })
        })
    }
}
