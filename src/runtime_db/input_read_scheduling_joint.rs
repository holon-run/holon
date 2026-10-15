//! The original Input allowance precedes Scheduling selector and body decoders.
use super::*;
impl InputReadMaterial<'_> {
    pub(in crate::runtime_db) fn prepare_original_scheduling_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        request: &mut crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        original_recovery: bool,
    ) -> Result<crate::runtime_db::scheduling_snapshot::SchedulingCurrentSnapshot> {
        self.validate_accounting(scope, ledger)?;
        let remaining = self.remaining_material_limits()?;
        ensure!(
            remaining.0 > 0 && remaining.1 > 0,
            crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ResourceExhausted(
                "joint Input Scheduling allowance"
            )
        );
        request.byte_limit = request.byte_limit.min(remaining.0);
        request.record_limit = request.record_limit.min(remaining.1);
        let (current, observed) =
            crate::runtime_db::scheduling_snapshot::prepare_scheduling_for_input_tx(
                tx,
                scope,
                request,
                ledger,
                original_recovery,
                self.memory.as_deref_mut(),
            )?;
        let (usage, guards) = observed.into_original_parts(self.scope, self.ledger)?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("joint Input Scheduling material");
        let bytes = self.bytes.checked_add(usage.0).context(exhausted())?;
        let records = self.records.checked_add(usage.1).context(exhausted())?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            exhausted()
        );
        self.guards.extend(guards);
        self.bytes = bytes;
        self.records = records;
        // Scheduling's actual charge already lent each guard to Memory.
        Ok(current)
    }
}
