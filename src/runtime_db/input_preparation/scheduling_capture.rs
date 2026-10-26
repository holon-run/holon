//! Own exactly the extra Input-preparation selector before entering Scheduling.
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
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
    _context_material: Option<Arc<BudgetViewGuard>>,
}
impl PreparedSchedulingRequest {
    pub(super) fn new(
        scope: &InputEvidenceScope,
        capture: Option<&SchedulingCaptureRequest>,
        record_limit: usize,
        byte_limit: usize,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let record_limit = record_limit.min(65_536);
        let byte_limit = byte_limit.min(64 * 1024 * 1024);
        let (capture, selector, context_material, records, bytes) = if let Some(capture) = capture {
            // Serialize into a counting sink; the caller's original set remains borrowed.
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
            ensure!(
                bytes < byte_limit && records < record_limit,
                SchedulingSnapshotError::ResourceExhausted("Input-owned Scheduling selector")
            );
            crate::diagnostics::resource_io::admit_cloned_value(&capture.required_keys)?;
            let context_material =
                crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?;
            let guard = ledger.register_owned_bytes(
                scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: u64::try_from(bytes)?,
                    metadata_estimate_bytes: 0,
                },
            )?;
            (
                capture.clone(),
                Some(guard),
                context_material,
                records,
                bytes,
            )
        } else {
            (SchedulingCaptureRequest::current(), None, None, 0, 0)
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
            _context_material: context_material,
        })
    }
}

#[cfg(test)]
mod tests;
