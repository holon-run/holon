//! Original Timer producer copies retain no Root, storage or physical handle.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_scheduling::{CapturedScheduling, SchedulingReadError},
};
use crate::types::TimerRecord;
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(in crate::runtime) struct OwnedTimerProducerRead {
    pub timer: TimerRecord,
    pub material: Arc<BudgetViewGuard>,
}

impl OwnedTimerProducerRead {
    pub(in crate::runtime) fn from_selected(
        selected: &CapturedScheduling,
        id: &str,
        previous: Option<&TimerRecord>,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Self> {
        let (retained, retained_records) = selected.selected_material_estimate()?;
        let byte_limit = u64::try_from(byte_limit.min(64 * 1024 * 1024))?;
        let record_limit = record_limit.min(65536);
        let selector_bytes = serialized_bytes(&id)?
            .checked_add(512)
            .and_then(|bytes| bytes.checked_mul(8))
            .context("Timer producer selector overflow")?;
        ensure!(
            retained
                .checked_add(selector_bytes)
                .is_some_and(|bytes| bytes <= byte_limit)
                && retained_records
                    .checked_add(8)
                    .is_some_and(|count| count <= record_limit),
            SchedulingReadError::ResourceExhausted("selected Scheduling and Timer selector")
        );
        let _selector = ledger.register_owned_bytes(
            &selected.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        let timer = selected
            .timer(id)?
            .context("Timer producer has no qualified Timer")?;
        ensure!(
            timer.agent_id == selected.scope.agent_id,
            "Timer producer scope differs"
        );
        if let Some(previous) = previous {
            ensure!(
                previous.id == timer.id
                    && previous.agent_id == timer.agent_id
                    && previous.created_at == timer.created_at,
                "Timer producer belongs to another original generation"
            );
        }
        let previous_bytes = previous.map(serialized_bytes).transpose()?.unwrap_or(0);
        // Recovery normalization, registration and the live loop share these
        // copies. The selected view itself is dropped before the loop owns one.
        let copies = serialized_bytes(timer)?
            .checked_add(previous_bytes)
            .and_then(|bytes| bytes.checked_add(512))
            .and_then(|bytes| bytes.checked_mul(8))
            .context("Timer producer copy bytes overflow")?;
        let records = if previous.is_some() { 24 } else { 16 };
        ensure!(
            retained
                .checked_add(copies)
                .and_then(|bytes| bytes.checked_add(selector_bytes))
                .is_some_and(|bytes| bytes <= byte_limit)
                && retained_records
                    .checked_add(records)
                    .and_then(|count| count.checked_add(8))
                    .is_some_and(|count| count <= record_limit),
            SchedulingReadError::ResourceExhausted(
                "selected Scheduling and original Timer producer copies"
            )
        );
        let material = ledger.register_owned_bytes(
            &selected.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: copies,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(Self {
            timer: timer.clone(),
            material,
        })
    }
}
