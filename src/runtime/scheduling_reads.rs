//! Pure Scheduling reads from one published root. Lookup never prepares.
use super::{
    mutation_owner::PublicationToken,
    resident_scheduling::{
        CanonicalSchedulingFact, CapturedScheduling, ResidentSchedulingDomain,
        SchedulingCaptureRequest, SchedulingFamily, SchedulingKey, SchedulingReadError,
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

/// Temporary planning witness; no domain map survives final context capture.
pub(in crate::runtime) struct ContextSchedulingPlan {
    pub read: SchedulingReadSnapshot,
    source: Arc<ResidentSchedulingDomain>,
}
impl ContextSchedulingPlan {
    pub(in crate::runtime) fn into_current_capture(
        self,
        state: &Arc<AgentState>,
        source: &Arc<ResidentSchedulingDomain>,
    ) -> Result<CapturedScheduling> {
        ensure!(
            self.read.state == *state,
            "context Scheduling State changed"
        );
        if Arc::ptr_eq(&self.source, source) || self.source.same_capture_source(source) {
            Ok(self.read.scheduling)
        } else {
            source.capture(&SchedulingCaptureRequest::current())
        }
    }
}
impl RuntimeMutation {
    pub(in crate::runtime) fn capture_context_scheduling_plan(
        &self,
    ) -> Result<ContextSchedulingPlan> {
        let root = self.capture_published_root()?;
        let source = root
            .scheduling
            .as_ref()
            .ok_or(SchedulingReadError::IncompleteLive)?;
        let read = self.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
        ensure!(
            read.token == root.token,
            "context Scheduling plan root changed"
        );
        crate::diagnostics::resource_io::charge_owned_metadata(128, 1)?;
        Ok(ContextSchedulingPlan {
            read,
            source: source.clone(),
        })
    }

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

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "scheduling_reads_tests.rs"]
mod tests;

pub(in crate::runtime) struct OwnedSchedulingTriggers {
    pub records: Vec<ExternalTriggerRecord>,
    _material: Arc<super::input_evidence_budget::BudgetViewGuard>,
}
impl CapturedScheduling {
    pub(in crate::runtime) fn clone_trigger_records(
        &self,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        _record_limit: usize,
        _byte_limit: usize,
    ) -> Result<OwnedSchedulingTriggers> {
        use super::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        let mut count = 0usize;
        let mut bytes = 0u64;
        for record in &self.records {
            if let CanonicalSchedulingFact::ExternalTrigger(trigger) = &record.fact {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Trigger count overflow"))?;
                bytes = bytes
                    .checked_add(serialized_bytes(trigger)?)
                    .ok_or_else(|| anyhow::anyhow!("Trigger copy bytes overflow"))?;
            }
        }
        let bytes = bytes
            .checked_add(
                u64::try_from(count)?
                    .checked_mul(128)
                    .ok_or_else(|| anyhow::anyhow!("Trigger slot bytes overflow"))?,
            )
            .and_then(|n| n.checked_add(512))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("Trigger owned material overflow"))?;
        let (_selected, _selected_records) = self.selected_material_estimate()?;
        let _output_records = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("Trigger output records overflow"))?;

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
            _material: material,
        })
    }
}
