//! Original scheduler observations keep their independent transaction boundaries.
use super::*;

impl RuntimeHandle {
    pub(super) async fn append_scheduler_decision(
        &self,
        decision: &scheduler::SchedulerDecision,
    ) -> Result<bool> {
        let events = scheduler::scheduler_decision_events(&self.inner.default_agent_id, decision)?;
        let recent = self.inner.storage.read_recent_events(32)?;
        if scheduler::recent_scheduler_decision_is_duplicate(&recent, &events[1]) {
            return Ok(false);
        }
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of_val(&events))?,
            events.len(),
        )?;
        let mut mutation = self.begin_mutation().await?;
        mutation
            .append_original_audit_events(Vec::from(events))
            .await?;
        Ok(true)
    }

    pub(super) async fn append_prepared_scheduling_advisories(
        &self,
        diagnostics: &[scheduler::SchedulingAdvisory],
    ) -> Result<usize> {
        let recent = self.inner.storage.read_recent_events(64)?;
        let mut appended = 0;
        for event in scheduler::pending_scheduling_advisory_events(&recent, diagnostics) {
            self.append_original_audit_event(event).await?;
            appended += 1;
        }
        Ok(appended)
    }
}
