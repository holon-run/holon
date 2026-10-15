//! Joint accounting borrows the original domains' real guards, never their authority.
use super::*;
use crate::runtime::current_read_facts::{CurrentReadError, CurrentReadRequest};

impl InputReadMaterial<'_> {
    pub(in crate::runtime_db) fn narrow_current_request(
        &self,
        request: &mut CurrentReadRequest,
    ) -> Result<()> {
        request.byte_limit = request.byte_limit.min(
            self.byte_limit
                .checked_sub(self.bytes)
                .context(CurrentReadError::ResourceExhausted)?,
        );
        request.record_limit = request.record_limit.min(
            self.record_limit
                .checked_sub(self.records)
                .context(CurrentReadError::ResourceExhausted)?,
        );
        if let Some(memory) = self.memory.as_deref() {
            let remaining = memory.remaining_material_limits()?;
            request.byte_limit = request.byte_limit.min(remaining.0);
            request.record_limit = request.record_limit.min(remaining.1);
        }
        request.check_material(0, 0)
    }
    pub(in crate::runtime_db) fn validate_current_allowance(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        request: &CurrentReadRequest,
    ) -> Result<()> {
        self.validate_binding(scope)?;
        ensure!(
            self.ledger.same_ledger(ledger),
            "Current allowance belongs to another Input ledger"
        );
        let remaining_bytes = self
            .byte_limit
            .checked_sub(self.bytes)
            .context(CurrentReadError::ResourceExhausted)?;
        let remaining_records = self
            .record_limit
            .checked_sub(self.records)
            .context(CurrentReadError::ResourceExhausted)?;
        ensure!(
            request.byte_limit <= remaining_bytes && request.record_limit <= remaining_records,
            CurrentReadError::ResourceExhausted
        );
        if let Some(memory) = self.memory.as_deref() {
            let remaining = memory.remaining_material_limits()?;
            ensure!(
                request.byte_limit <= remaining.0 && request.record_limit <= remaining.1,
                CurrentReadError::ResourceExhausted
            );
        }
        Ok(())
    }
    pub(in crate::runtime_db) fn observe_current_prior(
        &mut self,
        observed: &crate::runtime_db::current_read_snapshot::ObservedCurrentReadMaterial<'_>,
    ) -> Result<()> {
        let usage = observed.validate_accounting(self.scope, self.ledger)?;
        let bytes = self
            .bytes
            .checked_add(usage.0)
            .context(CurrentReadError::ResourceExhausted)?;
        let records = self
            .records
            .checked_add(usage.1)
            .context(CurrentReadError::ResourceExhausted)?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            CurrentReadError::ResourceExhausted
        );
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.additional_observed_current_material(observed)?;
        }
        // The original Current snapshot keeps its guards; this is only the same
        // phase debit, with no new registration or unguarded encoded copy.
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }
}

#[cfg(test)]
#[path = "input_read_joint_tests.rs"]
mod tests;
