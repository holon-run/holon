//! One actual Input phase observes Native scratch before any original key copy.
use super::*;

impl InputReadMaterial<'_> {
    pub(in crate::runtime_db::input_evidence_snapshot) fn accounting_scope(&self) -> &InputEvidenceScope { self.scope }
    pub(in crate::runtime_db::input_evidence_snapshot) fn accounting_ledger(&self) -> &InputEvidenceBudgetLedger { self.ledger }

    pub(in crate::runtime_db::input_evidence_snapshot) fn original_native_limits(&self) -> (usize, usize) {
        (self.byte_limit, self.record_limit)
    }

    /// The enclosing original proofs still own their guards. These counters
    /// were already admitted to Memory by Work/Task/Scheduling; borrow them
    /// into Input without creating a guard or debiting Memory a second time.
    pub(in crate::runtime_db) fn observe_original_writer_prior(
        &mut self,
        usage: (usize, usize),
    ) -> Result<()> {
        let next_bytes = self.bytes.checked_add(usage.0)
            .context(InputEvidenceSnapshotError::ResourceExhausted("original writer prior"))?;
        let next_records = self.records.checked_add(usage.1)
            .context(InputEvidenceSnapshotError::ResourceExhausted("original writer prior"))?;
        ensure!(next_bytes <= self.byte_limit && next_records <= self.record_limit,
            InputEvidenceSnapshotError::ResourceExhausted("original writer prior"));
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    pub(in crate::runtime_db::input_evidence_snapshot) fn observe_native_footprint(
        &mut self,
        bytes: usize,
        records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        // Native already supplies the raw copy bound or complete SQLite tree
        // byte bound. Do not multiply it or re-register it through Memory.
        self.charge(bytes, 0, records)?;
        // charge created and admitted exactly this allocation into the actual
        // ledger and optional Memory. The proof shares it, never registers again.
        Ok(self.guards.last().context("Native Input guard missing")?.clone())
    }
}

impl<'a> InputReadMaterial<'a> {
    /// Re-borrow the original Native phase without copying or reobserving its
    /// admitted scope, source, proof priors or guard vector.
    pub(in crate::runtime_db::input_evidence_snapshot) fn resume_native_phase(
        scope: &'a InputEvidenceScope,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        usage: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        ensure!(limits.0 <= MAX_METADATA_BYTES && limits.1 <= MAX_RECORDS
            && usage.0 <= limits.0 && usage.1 <= limits.1 && !guards.is_empty(),
            "original Native Input phase is incomplete");
        for guard in &guards { guard.validate_owned_material_binding(scope, ledger, 0)?; }
        if let Some(memory) = memory.as_deref() { memory.validate_input_accounting(scope, ledger)?; }
        Ok(Self { scope, ledger, byte_limit: limits.0, record_limit: limits.1,
            bytes: usage.0, records: usage.1, guards, memory })
    }
}
