//! Original Timer mutation routing from selected records in one published root.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_scheduling::{CapturedScheduling, SchedulingReadError, SchedulingWakeStatus},
    resident_work::CapturedWork,
};
use crate::{
    runtime_db::{TimerWakeRecord, TimerWakeStatus},
    types::TimerRecord,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(in crate::runtime) struct TimerMutationRead {
    pub timer: TimerRecord,
    pub pending: Option<TimerWakeRecord>,
    pub work_item_id: Option<String>,
    pub material: Arc<BudgetViewGuard>,
}
impl TimerMutationRead {
    pub(in crate::runtime) fn from_selected(
        scheduling: &CapturedScheduling,
        work: Option<&CapturedWork>,
        timer_id: &str,
        previous: Option<&TimerRecord>,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Option<Self>> {
        let (mut selected_bytes, mut selected_records) = scheduling.selected_material_estimate()?;
        if let Some(work) = work {
            ensure!(
                work.scope == scheduling.scope,
                "Timer routing domains have different scopes"
            );
            let (bytes, records) = selected_work_material(work)?;
            selected_bytes = selected_bytes
                .checked_add(bytes)
                .context("Timer routing selected bytes overflow")?;
            selected_records = selected_records
                .checked_add(records)
                .context("Timer routing selected records overflow")?;
        }
        let byte_limit = u64::try_from(byte_limit.min(64 * 1024 * 1024))?;
        let record_limit = record_limit.min(65_536);
        let selector_bytes = serialized_bytes(&timer_id)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .context("Timer routing selector overflow")?;
        let fits = |bytes: u64, records: usize| {
            selected_bytes
                .checked_add(bytes)
                .is_some_and(|n| n <= byte_limit)
                && selected_records
                    .checked_add(records)
                    .is_some_and(|n| n <= record_limit)
        };
        ensure!(
            fits(selector_bytes, 8),
            SchedulingReadError::ResourceExhausted("selected domains and Timer route selector")
        );
        let _selector = ledger.register_owned_bytes(
            &scheduling.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        let Some(timer) = scheduling.timer(timer_id)? else {
            return Ok(None);
        };
        if let Some(previous) = previous {
            ensure!(
                previous.id == timer.id
                    && previous.agent_id == timer.agent_id
                    && previous.created_at == timer.created_at,
                "Timer fire belongs to another original generation"
            );
        }
        let pending = scheduling.pending_wake(timer_id)?;
        ensure!(
            pending.is_none_or(|wake| wake.status == SchedulingWakeStatus::Pending
                && wake.agent_id == scheduling.scope.agent_id),
            "Timer route has another pending Wake owner/state"
        );
        let work_item_id = work
            .map(|work| work.timer_wait_work_item_id(timer_id))
            .transpose()?
            .flatten();
        let work_item_bytes = work_item_id
            .map(|id| serialized_bytes(&id))
            .transpose()?
            .unwrap_or(0);
        let bytes = serialized_bytes(timer)?
            .checked_add(previous.map(serialized_bytes).transpose()?.unwrap_or(0))
            .and_then(|n| n.checked_add(512))
            .context("Timer route body estimate overflow")?
            .checked_add(pending.map(serialized_bytes).transpose()?.unwrap_or(0))
            .and_then(|n| n.checked_add(work_item_bytes))
            .and_then(|n| n.checked_mul(32))
            .context("Timer route command copies overflow")?;
        let records = 128usize;
        ensure!(
            fits(
                bytes
                    .checked_add(selector_bytes)
                    .context("Timer route combined bytes overflow")?,
                records + 8
            ),
            SchedulingReadError::ResourceExhausted(
                "selected domains and original Timer command copies"
            )
        );
        let material = ledger.register_owned_bytes(
            &scheduling.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(Some(Self {
            timer: timer.clone(),
            pending: pending.map(|wake| TimerWakeRecord {
                timer_id: wake.timer_id.clone(),
                message_id: wake.message_id.clone(),
                fire_count: wake.fire_count,
                status: TimerWakeStatus::Pending,
                created_at: wake.created_at,
                updated_at: wake.updated_at,
                incorporated_at: wake.incorporated_at,
                cancelled_at: wake.cancelled_at,
            }),
            work_item_id: work_item_id.map(str::to_owned),
            material,
        }))
    }
}

/// Only a complete pending-Wake relation selects incorporation for this claim.
/// The original Queue TX remains responsible for the actual write and ACK.
pub(in crate::runtime) struct TimerWakeClaimRead {
    pub claim: crate::runtime_db::transitions::TimerWakeClaim,
    pub material: Arc<BudgetViewGuard>,
}
impl TimerWakeClaimRead {
    pub(in crate::runtime) fn from_selected(
        scheduling: &CapturedScheduling,
        work: &CapturedWork,
        message: &crate::types::MessageEnvelope,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Option<Self>> {
        let crate::types::MessageOrigin::Timer { timer_id } = &message.origin else {
            return Ok(None);
        };
        ensure!(
            work.scope == scheduling.scope && message.agent_id == scheduling.scope.agent_id,
            "Timer claim domains/source have different scopes"
        );
        let Some(wake) = scheduling.pending_wake(timer_id)? else {
            return Ok(None);
        };
        ensure!(
            wake.agent_id == scheduling.scope.agent_id
                && wake.status == SchedulingWakeStatus::Pending,
            "Timer claim selected another Wake owner/state"
        );
        if wake.message_id != message.id {
            return Ok(None);
        }
        let (scheduling_bytes, scheduling_records) = scheduling.selected_material_estimate()?;
        let (work_bytes, work_records) = selected_work_material(work)?;
        let bytes = serialized_bytes(&(timer_id, &message.id, wake.fire_count))?
            .checked_add(512)
            .and_then(|n| n.checked_mul(32))
            .context("Timer claim owned tuple material overflow")?;
        ensure!(
            scheduling_bytes
                .checked_add(work_bytes)
                .and_then(|n| n.checked_add(bytes))
                .is_some_and(|n| n <= byte_limit.min(64 * 1024 * 1024) as u64)
                && scheduling_records
                    .checked_add(work_records)
                    .and_then(|n| n.checked_add(32))
                    .is_some_and(|n| n <= record_limit.min(65_536)),
            SchedulingReadError::ResourceExhausted("selected domains and Timer claim copies")
        );
        let material = ledger.register_owned_bytes(
            &scheduling.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(Some(Self {
            claim: crate::runtime_db::transitions::TimerWakeClaim {
                timer_id: timer_id.clone(),
                message_id: message.id.clone(),
                fire_count: wake.fire_count,
            },
            material,
        }))
    }
}

// Borrowed estimates include the selected view and its nested relation metadata;
// no JSON Vec, WorkKey map encoding, Root or storage handle is constructed.
pub(in crate::runtime) fn selected_work_material(work: &CapturedWork) -> Result<(u64, usize)> {
    let mut bytes = serialized_bytes(&(
        &work.scope,
        work.revision,
        work.live_complete,
        &work.live_work_ids,
        &work.unresolved_wait_ids,
        &work.active_frame_ids,
        &work.turn_wait_keys,
        &work.task_wait_keys,
        &work.trigger_wait_keys,
        &work.nonopen,
        &work.latest_results,
    ))?;
    for pair in &work.observations {
        bytes = bytes
            .checked_add(serialized_bytes(&pair)?)
            .context("Timer route Work metadata overflow")?;
    }
    let mut records = work
        .records
        .len()
        .checked_add(work.observations.len())
        .and_then(|n| n.checked_add(work.live_work_ids.len()))
        .and_then(|n| n.checked_add(work.unresolved_wait_ids.len()))
        .and_then(|n| n.checked_add(work.active_frame_ids.len()))
        .and_then(|n| n.checked_add(work.nonopen.keys.len()))
        .and_then(|n| n.checked_add(work.latest_results.len()))
        .context("Timer route Work record estimate overflow")?;
    for edges in [
        &work.turn_wait_keys,
        &work.task_wait_keys,
        &work.trigger_wait_keys,
    ] {
        records = records
            .checked_add(edges.len())
            .context("Timer route relation count overflow")?;
        for values in edges.values() {
            records = records
                .checked_add(values.len())
                .context("Timer route relation count overflow")?;
        }
    }
    bytes = bytes
        .checked_add(
            u64::try_from(records)?
                .checked_mul(128)
                .context("Timer route Work slots overflow")?,
        )
        .context("Timer route Work metadata overflow")?;
    for record in &work.records {
        bytes = bytes
            .checked_add(u64::try_from(record.resident_bytes)?)
            .context("Timer route Work bytes overflow")?;
    }
    Ok((bytes, records))
}

#[cfg(test)]
#[path = "timer_mutation_reads_tests.rs"]
mod tests;
