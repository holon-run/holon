//! Pure Scheduling reads from one published root. Lookup never prepares.
use super::{
    mutation_owner::PublicationToken,
    resident_scheduling::{
        CanonicalSchedulingFact, CapturedScheduling, SchedulingCaptureRequest, SchedulingFamily,
        SchedulingKey, SchedulingReadError,
    },
    state_mutation::RuntimeMutation,
    RuntimeHandle,
};
use crate::types::{AgentState, ExternalTriggerRecord, TimerRecord};
use anyhow::{ensure, Result};
use std::sync::Arc;

pub(crate) struct SchedulingReadSnapshot {
    pub token: PublicationToken,
    pub state: Arc<AgentState>,
    pub scheduling: CapturedScheduling,
}
impl RuntimeMutation {
    pub(in crate::runtime) fn capture_scheduling_read(
        &self,
        request: &SchedulingCaptureRequest,
    ) -> Result<SchedulingReadSnapshot> {
        let (token, state, scheduling) = self.capture_scheduling_with_state(request)?;
        Ok(SchedulingReadSnapshot {
            token,
            state,
            scheduling,
        })
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) async fn capture_scheduling_read(
        &self,
        request: &SchedulingCaptureRequest,
    ) -> Result<SchedulingReadSnapshot> {
        let owner = self.begin_mutation().await?;
        owner.capture_scheduling_read(request)
    }
}
impl CapturedScheduling {
    pub(crate) fn timer(&self, id: &str) -> Result<Option<&TimerRecord>> {
        self.lookup(&SchedulingKey::new(SchedulingFamily::Timer, id))?
            .map(|record| match &record.fact {
                CanonicalSchedulingFact::Timer(timer) => Ok(timer),
                _ => anyhow::bail!("Scheduling Timer family differs"),
            })
            .transpose()
    }
}

#[cfg(test)]
#[path = "scheduling_reads_tests.rs"]
mod tests;

pub(in crate::runtime) struct OwnedSchedulingTriggers {
    pub records: Vec<ExternalTriggerRecord>,
    pub(in crate::runtime) material: Arc<super::input_evidence_budget::BudgetViewGuard>,
    pub(in crate::runtime) cost: (usize, usize),
}
impl CapturedScheduling {
    pub(in crate::runtime) fn clone_trigger_records(
        &self,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<OwnedSchedulingTriggers> {
        self.clone_trigger_records_with_prior(ledger, (byte_limit, record_limit), None)
    }
    pub(in crate::runtime) fn clone_trigger_records_with_prior(
        &self,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: Option<(u64, usize)>,
    ) -> Result<OwnedSchedulingTriggers> {
        let (byte_limit, record_limit) = if prior.is_some() {
            let configured = ledger.limits();
            (
                limits.0.min(64 * 1024 * 1024).min(usize::try_from(
                    configured
                        .history_per_agent_bytes
                        .checked_add(configured.pending_per_agent_bytes)
                        .ok_or_else(|| anyhow::anyhow!("Trigger phase byte limit overflow"))?,
                )?),
                limits
                    .1
                    .min(65536)
                    .min(usize::try_from(configured.record_limit)?),
            )
        } else {
            limits
        };
        use super::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        let mut count = 0usize;
        let mut bytes = 0u64;
        let mut native = 0usize;
        for record in &self.records {
            if let CanonicalSchedulingFact::ExternalTrigger(trigger) = &record.fact {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Trigger count overflow"))?;
                native = native
                    .checked_add(super::resource_refresh::borrowed_resource_record_count(
                        trigger,
                    )?)
                    .ok_or_else(|| anyhow::anyhow!("Trigger native count overflow"))?;
                bytes = bytes
                    .checked_add(serialized_bytes(trigger)?)
                    .ok_or_else(|| anyhow::anyhow!("Trigger copy bytes overflow"))?;
            }
        }
        let slot_bytes = u64::try_from(count)?
            .checked_mul(128)
            .ok_or_else(|| anyhow::anyhow!("Trigger slot bytes overflow"))?;
        let native_bytes = u64::try_from(native)?
            .checked_mul(256)
            .ok_or_else(|| anyhow::anyhow!("Trigger native bytes overflow"))?;
        let bytes = bytes
            .checked_add(slot_bytes)
            .and_then(|n| n.checked_add(native_bytes))
            .and_then(|n| n.checked_add(512))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("Trigger owned material overflow"))?;
        let (selected, selected_records) = match prior {
            Some(prior) => {
                self.validate_material_binding(&self.scope, ledger)?;
                prior
            }
            None => self.selected_material_estimate()?,
        };
        let output_records = native
            .checked_add(count)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("Trigger output records overflow"))?;
        ensure!(
            selected
                .checked_add(bytes)
                .is_some_and(|n| n <= u64::try_from(byte_limit.min(64 * 1024 * 1024)).unwrap_or(0))
                && selected_records
                    .checked_add(output_records)
                    .is_some_and(|n| n <= record_limit.min(65536)),
            SchedulingReadError::ResourceExhausted("selected Scheduling and Trigger output")
        );
        let material = ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut records: Vec<_> = self
            .records
            .iter()
            .filter_map(|record| match &record.fact {
                CanonicalSchedulingFact::ExternalTrigger(trigger) => Some(trigger.clone()),
                _ => None,
            })
            .collect();
        records.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
        Ok(OwnedSchedulingTriggers {
            records,
            material,
            cost: (usize::try_from(bytes)?, output_records),
        })
    }
}
