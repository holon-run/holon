//! Selected Work reads. Preparation and canonical writes stay at named owners.
use std::sync::Arc;

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};

use super::{
    resident_work::{
        CanonicalWorkRecord, CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey, WorkReadError,
    },
    state_mutation::RuntimeMutation,
    RuntimeHandle,
};
use crate::types::{
    AgentState, WaitConditionRecord, WaitConditionStatus, WorkItemRecord, WorkItemState,
};

/// Recheck readers require the complete live set, not optional history bodies.
pub(super) fn live_work_request() -> WorkCaptureRequest {
    let mut request = WorkCaptureRequest::current();
    request.nonopen_limit = 0;
    request
}

/// The same published root's State and selected records, never a RuntimeAgent
/// guard, storage handle or complete resident map. Deliberately not Clone.
pub(crate) struct WorkReadSnapshot {
    pub state: Arc<AgentState>,
    pub work: CapturedWork,
}
impl RuntimeMutation {
    pub(super) fn capture_work_read(
        &self,
        request: &WorkCaptureRequest,
    ) -> Result<WorkReadSnapshot> {
        let (_, state, work) = self.capture_work_with_state(request)?;
        ensure!(
            state.id == work.scope.agent_id,
            "Work read State scope differs"
        );
        Ok(WorkReadSnapshot { state, work })
    }
}
impl RuntimeHandle {
    /// Ordinary readers never prepare a missing key or expand a window here.
    /// Planning callers already holding the owner use capture_work_read instead.
    pub(crate) async fn capture_work_read(
        &self,
        request: &WorkCaptureRequest,
    ) -> Result<WorkReadSnapshot> {
        let mutation = self.begin_mutation().await?;
        mutation.capture_work_read(request)
    }
}
impl CapturedWork {
    pub(crate) fn work_item(&self, id: &str) -> Result<Option<&WorkItemRecord>> {
        self.lookup(&WorkKey::new(WorkFamily::Work, id))?
            .map(|record| match &record.record {
                CanonicalWorkRecord::Work(work) => Ok(work),
                _ => anyhow::bail!("captured Work key has a different family"),
            })
            .transpose()
    }
    pub(crate) fn wait_condition(&self, id: &str) -> Result<Option<&WaitConditionRecord>> {
        self.lookup(&WorkKey::new(WorkFamily::Wait, id))?
            .map(|record| match &record.record {
                CanonicalWorkRecord::Wait(wait) => Ok(wait),
                _ => anyhow::bail!("captured Wait key has a different family"),
            })
            .transpose()
    }
    pub(crate) fn live_work_items(&self) -> Result<Vec<&WorkItemRecord>> {
        ensure_live(self)?;
        self.live_work_ids
            .iter()
            .map(|id| {
                let work = self.work_item(id)?.ok_or_else(|| {
                    WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, id))
                })?;
                ensure!(
                    matches!(work.state, WorkItemState::Open | WorkItemState::Completing),
                    "captured live Work has a non-live state"
                );
                Ok(work)
            })
            .collect()
    }
    /// Original raw-unresolved predicate/order, including Triggered. This is
    /// not the Open-owner Active-only context work-queue filter.
    pub(crate) fn unresolved_wait_conditions(&self) -> Result<Vec<&WaitConditionRecord>> {
        ensure_live(self)?;
        let mut waits = self
            .unresolved_wait_ids
            .iter()
            .map(|id| {
                let wait = self.wait_condition(id)?.ok_or_else(|| {
                    WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Wait, id))
                })?;
                ensure!(
                    matches!(
                        wait.status,
                        WaitConditionStatus::Active | WaitConditionStatus::Triggered
                    ),
                    "captured unresolved Wait has a terminal state"
                );
                Ok(wait)
            })
            .collect::<Result<Vec<_>>>()?;
        waits.sort_by(|a, b| {
            (
                std::cmp::Reverse(a.updated_at),
                std::cmp::Reverse(a.created_at),
                &a.id,
            )
                .cmp(&(
                    std::cmp::Reverse(b.updated_at),
                    std::cmp::Reverse(b.created_at),
                    &b.id,
                ))
        });
        Ok(waits)
    }
    /// Original Active-only/Open-owner context predicate and canonical order.
    /// A missing preparation is never interpreted as an absent owner.
    /// Preserve the original unique Active Timer wait / Open owner predicate.
    pub(in crate::runtime) fn timer_wait_work_item_id(
        &self,
        timer_id: &str,
    ) -> Result<Option<&str>> {
        ensure_live(self)?;
        let mut selected = None;
        let mut count = 0usize;
        for id in &self.unresolved_wait_ids {
            let wait = self
                .records
                .iter()
                .find_map(|record| match &record.record {
                    CanonicalWorkRecord::Wait(wait) if &wait.id == id => Some(wait),
                    _ => None,
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("complete Work Timer route lacks selected Wait body")
                })?;
            if wait.status != WaitConditionStatus::Active
                || wait.kind != crate::types::WaitConditionKind::Timer
                || wait.subject_ref.as_deref() != Some(timer_id)
            {
                continue;
            }
            if let Some(owner) = wait.work_item_id.as_deref() {
                let is_open = self.records.iter().any(|record| {
                    matches!(&record.record, CanonicalWorkRecord::Work(work)
                        if work.id == owner && work.state == WorkItemState::Open)
                });
                if !is_open {
                    continue;
                }
            }
            count += 1;
            selected = wait.work_item_id.as_deref();
        }
        Ok((count == 1).then_some(selected).flatten())
    }

    pub(crate) fn context_active_wait_conditions(&self) -> Result<Vec<&WaitConditionRecord>> {
        let mut selected = Vec::new();
        for wait in self.unresolved_wait_conditions()? {
            if wait.status != WaitConditionStatus::Active {
                continue;
            }
            let live_owner = match wait.work_item_id.as_deref() {
                Some(id) => self
                    .work_item(id)?
                    .is_some_and(|work| work.state == WorkItemState::Open),
                None => true,
            };
            if live_owner {
                selected.push(wait);
            }
        }
        Ok(selected)
    }
    /// Work repository SQL compares its normalized millisecond columns. The
    /// returned typed Work remains unchanged, including its original payload.
    pub(crate) fn blocked_work_rechecks(&self) -> Result<Vec<&WorkItemRecord>> {
        let mut works = self
            .live_work_items()?
            .into_iter()
            .filter(|work| {
                work.state == WorkItemState::Open
                    && work.blocked_by.is_some()
                    && work.recheck_at.is_some_and(|at| {
                        work.recheck_consumed_at.is_none_or(|consumed| {
                            consumed.timestamp_millis() < at.timestamp_millis()
                        })
                    })
            })
            .collect::<Vec<_>>();
        works.sort_by(|a, b| {
            (a.recheck_at.unwrap().timestamp_millis(), &a.id)
                .cmp(&(b.recheck_at.unwrap().timestamp_millis(), &b.id))
        });
        Ok(works)
    }
    pub(crate) fn next_work_recheck_at(&self) -> Result<Option<DateTime<Utc>>> {
        self.blocked_work_rechecks()?
            .first()
            .map(|work| {
                DateTime::from_timestamp_millis(work.recheck_at.unwrap().timestamp_millis())
                    .ok_or_else(|| anyhow::anyhow!("Work recheck timestamp is invalid"))
            })
            .transpose()
    }
    pub(crate) fn due_work_rechecks(&self, now: DateTime<Utc>) -> Result<Vec<&WorkItemRecord>> {
        Ok(self
            .blocked_work_rechecks()?
            .into_iter()
            .filter(|work| work.recheck_at.unwrap().timestamp_millis() <= now.timestamp_millis())
            .collect())
    }
    pub(crate) fn next_agent_wait_recheck_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|wait| {
                wait.work_item_id.is_none() && wait.status == WaitConditionStatus::Active
            })
            .filter_map(WaitConditionRecord::recheck_at)
            .min())
    }
    pub(crate) fn due_agent_wait_recheck(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<&WaitConditionRecord>> {
        Ok(self
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|wait| {
                wait.work_item_id.is_none()
                    && wait.status == WaitConditionStatus::Active
                    && wait.recheck_at().is_some_and(|at| at <= now)
            })
            .min_by_key(|wait| wait.recheck_at()))
    }
}
fn ensure_live(work: &CapturedWork) -> Result<()> {
    if !work.live_complete {
        return Err(WorkReadError::IncompleteLive.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "work_reads_tests.rs"]
mod tests;

/// The ordinary list query uses the original full-live/128-history policy.
/// Trigger facts remain an explicit D6 adapter observed under this same owner.
pub(crate) struct WorkQuerySnapshot {
    scope: super::resident_input_evidence::InputEvidenceScope,
    pub state: Arc<AgentState>,
    pub items: Vec<crate::work_item_scheduling::WorkItemSchedulingProjection>,
    _owned: Arc<super::input_evidence_budget::BudgetViewGuard>,
}
impl RuntimeHandle {
    /// A by-ID historical result belongs to this request's Pins, never its current domain.
    pub(crate) fn account_historical_work_query_record(
        &self,
        snapshot: &WorkQuerySnapshot,
        record: &WorkItemRecord,
    ) -> Result<Arc<super::input_evidence_budget::BudgetViewGuard>> {
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &snapshot.scope,
            super::input_evidence_budget::InputMemoryCategory::Pins,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: super::input_evidence_budget::serialized_bytes(record)?
                    .saturating_mul(3),
                metadata_estimate_bytes: 0,
            },
        )?)
    }
    pub(crate) async fn capture_work_query_snapshot(&self) -> Result<WorkQuerySnapshot> {
        let mutation = self.begin_mutation().await?;
        let request = super::context_capture::context_work_capture_request(&mutation.state);
        let captured = mutation.capture_work_read(&request)?;
        let scheduling = mutation.capture_scheduling_read(
            &super::resident_scheduling::SchedulingCaptureRequest::current(),
        )?;
        ensure!(
            scheduling.state == captured.state
                && scheduling.scheduling.scope == captured.work.scope,
            "Work and Scheduling publication differ"
        );
        let (_, records, bytes) = mutation.context_input_evidence_policy()?;
        let triggers = scheduling.scheduling.clone_trigger_records(
            &self.inner.input_evidence_budget,
            records,
            bytes,
        )?;
        let work_queue = captured
            .work
            .work_queue(&captured.state, triggers.records)?;
        let items = work_queue.items;
        let owned = self.inner.input_evidence_budget.register_owned_bytes(
            &captured.work.scope,
            super::input_evidence_budget::InputMemoryCategory::Pins,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: super::input_evidence_budget::serialized_bytes(&items)?
                    .saturating_mul(3),
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(WorkQuerySnapshot {
            scope: captured.work.scope.clone(),
            state: captured.state,
            items,
            _owned: owned,
        })
    }
}
