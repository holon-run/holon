//! Own exactly the extra Input-preparation selector before entering Scheduling.
#[cfg(test)]
use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
use crate::runtime::{
    input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::InputEvidenceScope,
    resident_scheduling::{SchedulingCaptureRequest, SchedulingKey},
};
use crate::runtime_db::scheduling_snapshot::{
    SchedulingDomainLoadRequest, SchedulingSnapshotError,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(super) struct PreparedSchedulingRequest {
    pub(super) request: SchedulingDomainLoadRequest,
    // Field order drops the owned request before its allocation guard.
    _selector: Option<Arc<BudgetViewGuard>>,
}
impl PreparedSchedulingRequest {
    #[cfg(test)]
    pub(super) fn new(
        scope: &InputEvidenceScope,
        capture: Option<&SchedulingCaptureRequest>,
        record_limit: usize,
        byte_limit: usize,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let record_limit = record_limit.min(65_536);
        let byte_limit = byte_limit.min(64 * 1024 * 1024);
        let (capture, selector, records, bytes) = if let Some(capture) = capture {
            let (bytes, records) = selector_material(capture)?;
            ensure!(
                bytes < byte_limit && records < record_limit,
                SchedulingSnapshotError::ResourceExhausted("Input-owned Scheduling selector")
            );
            let guard = ledger.register_owned_bytes(
                scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: u64::try_from(bytes)?,
                    metadata_estimate_bytes: 0,
                },
            )?;
            (capture.clone(), Some(guard), records, bytes)
        } else {
            (SchedulingCaptureRequest::current(), None, 0, 0)
        };
        Ok(Self {
            request: SchedulingDomainLoadRequest {
                capture,
                // The nested loader accumulates all of its own materials against
                // what remains after this still-live additional selector copy.
                record_limit: record_limit - records,
                byte_limit: byte_limit - bytes,
            },
            _selector: selector,
        })
    }
    pub(super) fn using_input_material(
        scope: &InputEvidenceScope,
        capture: Option<&SchedulingCaptureRequest>,
        ledger: &InputEvidenceBudgetLedger,
        input: &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<
            '_,
        >,
    ) -> Result<Self> {
        input.validate_accounting(scope, ledger)?;
        let remaining = input.remaining_material_limits()?;
        let (capture, bytes, records) = if let Some(capture) = capture {
            let (bytes, records) = selector_material(capture)?;
            let bytes = bytes
                .checked_add(
                    records
                        .checked_mul(256)
                        .context("Scheduling selector native floor overflow")?,
                )
                .context("Scheduling selector combined native bytes overflow")?;
            ensure!(
                bytes < remaining.0 && records < remaining.1,
                SchedulingSnapshotError::ResourceExhausted("Input-owned Scheduling selector")
            );
            // Input owns the sole new selector guard and lends that same
            // allocation to Memory before cloning any required key.
            input.observe_projected(u64::try_from(bytes)?, records)?;
            (capture.clone(), bytes, records)
        } else {
            (SchedulingCaptureRequest::current(), 0, 0)
        };
        Ok(Self {
            request: SchedulingDomainLoadRequest {
                capture,
                byte_limit: remaining.0 - bytes,
                record_limit: remaining.1 - records,
            },
            _selector: None,
        })
    }
}

fn selector_material(capture: &SchedulingCaptureRequest) -> Result<(usize, usize)> {
    let bytes = usize::try_from(serialized_bytes(&capture.required_keys)?)?
        .checked_add(std::mem::size_of::<SchedulingCaptureRequest>())
        .and_then(|n| {
            capture
                .required_keys
                .len()
                .checked_mul(std::mem::size_of::<SchedulingKey>())
                .and_then(|keys| n.checked_add(keys))
        })
        .and_then(|n| n.checked_add(128))
        .and_then(|n| n.checked_mul(8))
        .context("Scheduling preparation selector bytes overflow")?;
    let records = capture
        .required_keys
        .len()
        .checked_mul(8)
        .context("Scheduling preparation selector records overflow")?;

    Ok((bytes, records))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "scheduling_capture/joint_tests.rs"]
mod joint_tests;
