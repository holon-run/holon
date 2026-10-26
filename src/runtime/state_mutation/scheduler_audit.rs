use super::*;

impl RuntimeMutation {
    pub(in crate::runtime) async fn append_prepared_scheduling_advisories(
        &mut self,
        diagnostics: &[super::super::scheduler::SchedulingAdvisory],
    ) -> Result<usize> {
        let recent_events = self.runtime.inner.storage.read_recent_events(64)?;
        let mut appended = 0;
        for event in
            super::super::scheduler::pending_scheduling_advisory_events(&recent_events, diagnostics)
        {
            self.append_original_audit_event(event).await?;
            appended += 1;
        }
        Ok(appended)
    }
}
