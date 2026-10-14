//! Scheduling observations follow the original summary DTO until serialization.
use super::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use super::resident_scheduling::CapturedScheduling;
use super::scheduling_output::{project_selected_scheduling_records, SchedulingReadMaterial};
use super::*;
use std::ops::Deref;

/// The JSON representation is the original `AgentSummary`.
pub struct OwnedAgentSummaryRead {
    summary: AgentSummary,
    material: AgentSummaryReadMaterial,
}
pub(crate) struct AgentSummaryReadMaterial {
    _scheduling: SchedulingReadMaterial,
    _owned: Vec<Arc<BudgetViewGuard>>,
}
impl OwnedAgentSummaryRead {
    pub(crate) fn new(
        summary: AgentSummary,
        scheduling: SchedulingReadMaterial,
        owned: Vec<Arc<BudgetViewGuard>>,
    ) -> Self {
        Self {
            summary,
            material: AgentSummaryReadMaterial {
                _scheduling: scheduling,
                _owned: owned,
            },
        }
    }

    // The caller keeps `material` through the synchronous DTO serialization.
    pub(crate) fn into_parts(self) -> (AgentSummary, AgentSummaryReadMaterial) {
        (self.summary, self.material)
    }
}
impl Deref for OwnedAgentSummaryRead {
    type Target = AgentSummary;
    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}
impl std::fmt::Debug for OwnedAgentSummaryRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.summary, formatter)
    }
}
impl serde::Serialize for OwnedAgentSummaryRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.summary, serializer)
    }
}

pub(crate) fn active_trigger_summaries(
    records: Vec<ExternalTriggerRecord>,
) -> Vec<ExternalTriggerSummary> {
    records
        .into_iter()
        .filter(|record| record.status == ExternalTriggerStatus::Active)
        .map(|record| ExternalTriggerSummary {
            external_trigger_id: record.external_trigger_id,
            target_agent_id: record.target_agent_id,
            scope: record.scope,
            delivery_mode: record.delivery_mode,
            status: record.status,
            delivery_count: record.delivery_count,
            created_at: record.created_at,
            revoked_at: record.revoked_at,
            last_delivered_at: record.last_delivered_at,
        })
        .collect()
}
impl RuntimeHandle {
    pub(super) fn summary_triggers_from_capture(
        &self,
        selected: CapturedScheduling,
    ) -> Result<(Vec<ExternalTriggerSummary>, SchedulingReadMaterial)> {
        let ledger: &InputEvidenceBudgetLedger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let (timers, triggers, material) = project_selected_scheduling_records(
            selected,
            0,
            ledger,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("summary Scheduling byte limit overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
            (0, 0),
            Vec::new(),
        )?;
        debug_assert!(timers.is_empty());
        // The existing output allowance covers four full record copies;
        // this scalar subset moves strings from one of those copies.
        Ok((active_trigger_summaries(triggers), material))
    }
}

#[cfg(test)]
#[path = "agent_summary_read_tests.rs"]
mod tests;
