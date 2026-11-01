//! The original accepted Wait prepares Scheduling under its already-held owner.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_scheduling::{SchedulingCaptureRequest, SchedulingReadError},
    waiting::{PrepareWaitForOutcome, WaitForWakeKind},
};
use crate::runtime_db::TimerWakeRecord;
use crate::runtime_error::RuntimeError;

pub(in crate::runtime) struct WaitSchedulingRead {
    wake_at: Option<chrono::DateTime<chrono::Utc>>,
    pending: Option<TimerWakeRecord>,
    trigger_id: Option<String>,
    material: Arc<BudgetViewGuard>,
}
impl WaitSchedulingRead {
    pub(in crate::runtime) fn into_parts(
        self,
    ) -> (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<TimerWakeRecord>,
        Option<String>,
        Arc<BudgetViewGuard>,
    ) {
        (self.wake_at, self.pending, self.trigger_id, self.material)
    }
}
impl RuntimeMutation {
    #[inline(never)]
    pub(in crate::runtime) fn prepare_wait_for_outcome_with_id<'a>(
        &'a mut self,
        agent_id: &'a str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
        condition_id: Option<String>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PrepareWaitForOutcome>> + Send + 'a>,
    > {
        Box::pin(async move {
            anyhow::ensure!(
                agent_id == self.candidate.state.id,
                "wait_for agent mismatch: {agent_id}"
            );
            let scheduling = self
                .prepare_wait_scheduling_read(wake, resource.as_deref())
                .await?;
            let runtime = self.runtime.clone();
            runtime
                .prepare_wait_for_outcome_from_owned_read(
                    self,
                    agent_id,
                    work_item_id,
                    wake,
                    resource,
                    reason,
                    recheck_after_ms,
                    condition_id,
                    scheduling,
                )
                .await
        })
    }
    async fn prepare_wait_scheduling_read(
        &mut self,
        wake: WaitForWakeKind,
        resource: Option<&str>,
    ) -> Result<Option<WaitSchedulingRead>> {
        match wake {
            WaitForWakeKind::Timer => {
                let timer_id = resource
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("wait_for {:?} requires non-empty resource", wake))?;
                let read = match self.prepare_timer_mutation_read(timer_id, None, false).await {
                    Ok(read) => read,
                    Err(error) if matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
                        Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ForeignRecord(key))
                        if key.family == super::super::resident_scheduling::SchedulingFamily::Timer && key.id == timer_id) => {
                        return Err(RuntimeError::policy("timer_agent_mismatch", "wait_for timer is not owned by this agent").into());
                    }
                    Err(error) => return Err(error),
                }.ok_or_else(|| RuntimeError::not_found("timer_not_found", "wait_for timer does not exist"))?;
                let (wake_at, pending) = match read.timer.status {
                    TimerStatus::Active => (
                        read.timer.next_fire_at.ok_or_else(|| {
                            RuntimeError::validation(
                                "timer_invalid_state",
                                "wait_for timer has no next fire time",
                            )
                        })?,
                        None,
                    ),
                    TimerStatus::Completed => {
                        let pending = read.pending.ok_or_else(|| {
                            RuntimeError::validation(
                                "timer_wake_unavailable",
                                "wait_for timer wake is unavailable",
                            )
                        })?;
                        (pending.created_at, Some(pending))
                    }
                    TimerStatus::Cancelled => {
                        return Err(RuntimeError::validation(
                            "timer_cancelled",
                            "wait_for timer is cancelled",
                        )
                        .into())
                    }
                };
                Ok(Some(WaitSchedulingRead {
                    wake_at: Some(wake_at),
                    pending,
                    trigger_id: None,
                    material: read.material,
                }))
            }
            WaitForWakeKind::External => {
                let request = SchedulingCaptureRequest {
                    include_live: true,
                    history_limit: 0,
                    required_keys: Default::default(),
                };
                match self.capture_scheduling_read(&request) {
                    Ok(_) => {}
                    Err(error) if error.downcast_ref::<SchedulingReadError>().is_some() => {
                        self.prepare_owner_scheduling_request(&request).await?
                    }
                    Err(error) => return Err(error),
                }
                let read = self.capture_scheduling_read(&request)?;
                let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
                external_wait_read(
                    &read.scheduling,
                    &self.runtime.inner.input_evidence_budget,
                    record_limit,
                    byte_limit,
                )
                .map(Some)
            }
            _ => Ok(None),
        }
    }
}

fn external_wait_read(
    scheduling: &crate::runtime::resident_scheduling::CapturedScheduling,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<WaitSchedulingRead> {
    let trigger = scheduling.active_default_trigger()?;
    let bytes = serialized_bytes(&trigger.map(|trigger| &trigger.external_trigger_id))?
        .checked_add(512)
        .and_then(|n| n.checked_mul(32))
        .context("Wait Trigger material overflow")?;
    let (selected, records) = scheduling.selected_material_estimate()?;
    anyhow::ensure!(
        selected
            .checked_add(bytes)
            .is_some_and(|n| n <= byte_limit.min(64 * 1024 * 1024) as u64)
            && records
                .checked_add(32)
                .is_some_and(|n| n <= record_limit.min(65_536)),
        SchedulingReadError::ResourceExhausted("selected Scheduling and Wait Trigger copies")
    );
    let material = ledger.register_owned_bytes(
        &scheduling.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: bytes,
            metadata_estimate_bytes: 0,
        },
    )?;
    let trigger_id = trigger.map(|trigger| trigger.external_trigger_id.clone());
    Ok(WaitSchedulingRead {
        wake_at: None,
        pending: None,
        trigger_id,
        material,
    })
}

#[cfg(test)]
#[path = "wait_preparation_tests.rs"]
mod tests;
