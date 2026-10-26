//! The original writer's cumulative Input allowance, not body read authority.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};

pub(in crate::runtime_db) struct ObservedInputReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    guard: Arc<BudgetViewGuard>,
    bytes: usize,
    records: usize,
}

impl ObservedInputReadMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Input material differs from original Memory scope/ledger"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, u64::try_from(self.bytes)?)?;
        Ok((self.bytes, self.records))
    }
}

pub(in crate::runtime_db) struct InputReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    limits: (usize, usize),
    usage: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
    memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
}

impl<'a> InputReadMaterial<'a> {
    pub(in crate::runtime_db) fn new(
        scope: &'a InputEvidenceScope,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        ensure!(
            !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
            "invalid Input material scope"
        );
        if let Some(memory) = memory.as_deref() {
            memory.validate_input_accounting(scope, ledger)?;
        }
        let prior = memory
            .as_deref()
            .map(|memory| memory.input_material_usage())
            .unwrap_or_default();
        let mut material = Self {
            scope,
            ledger,
            limits: (limits.0.min(MAX_METADATA_BYTES), limits.1.min(MAX_RECORDS)),
            usage: (0, 0),
            guards: Vec::new(),
            memory,
        };
        let metadata = usize::try_from(serialized_bytes(scope)?)?
            .checked_mul(8)
            .and_then(|n| n.checked_add(4096 + std::mem::size_of::<Self>()))
            .context(InputEvidenceSnapshotError::ResourceExhausted(
                "Input scope material",
            ))?;
        material.charge(0, metadata, 1)?;
        material.observe_original_writer_prior(prior)?;
        Ok(material)
    }

    /// The other original domain proofs still retain these guards and have
    /// already debited Memory. Input observes them, never registers them again.
    pub(in crate::runtime_db) fn observe_original_writer_prior(
        &mut self,
        usage: (usize, usize),
    ) -> Result<()> {
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("original writer prior");
        let next = (
            self.usage.0.checked_add(usage.0).context(exhausted())?,
            self.usage.1.checked_add(usage.1).context(exhausted())?,
        );
        ensure!(
            next.0 <= self.limits.0 && next.1 <= self.limits.1,
            exhausted()
        );
        self.usage = next;
        Ok(())
    }

    fn charge(&mut self, payload: usize, metadata: usize, records: usize) -> Result<()> {
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input read material");
        let metadata = metadata
            .checked_add(std::mem::size_of::<BudgetViewGuard>())
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<Arc<BudgetViewGuard>>()))
            .context(exhausted())?;
        let records = records.checked_add(1).context(exhausted())?;
        let bytes = payload.checked_add(metadata).context(exhausted())?;
        let next = (
            self.usage.0.checked_add(bytes).context(exhausted())?,
            self.usage.1.checked_add(records).context(exhausted())?,
        );
        if next.0 > self.limits.0 || next.1 > self.limits.1 {
            return Err(anyhow::Error::new(exhausted()).context(format!(
                "Input read material exceeds original allowance: used={:?}, requested=({bytes}, {records}), limits={:?}",
                self.usage, self.limits,
            )));
        }
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(payload)?,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?;
        let observed = ObservedInputReadMaterial {
            scope: self.scope,
            ledger: self.ledger,
            guard,
            bytes,
            records,
        };
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.additional_observed_input_material(&observed)?;
        }
        self.guards.push(observed.guard);
        self.usage = next;
        Ok(())
    }

    pub(in crate::runtime_db) fn observe_native_footprint(
        &mut self,
        bytes: usize,
        records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        self.charge(bytes, 0, records)?;
        Ok(self
            .guards
            .last()
            .context("Native Input guard missing")?
            .clone())
    }

    pub(in crate::runtime_db) fn validate_binding(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.scope == scope,
            "Input material belongs to another scope"
        );
        Ok(())
    }

    pub(in crate::runtime_db) fn validate_guard_binding(
        &self,
        guard: &BudgetViewGuard,
    ) -> Result<()> {
        guard.validate_owned_material_binding(self.scope, self.ledger, 0)?;
        Ok(())
    }

    pub(in crate::runtime_db) fn remaining_limits(&self) -> (usize, usize) {
        (self.limits.0 - self.usage.0, self.limits.1 - self.usage.1)
    }

    pub(in crate::runtime_db) fn scope_guard(&self) -> Result<Arc<BudgetViewGuard>> {
        self.guards
            .first()
            .cloned()
            .context("Input material scope guard missing")
    }

    pub(in crate::runtime_db) fn into_guards(self) -> Vec<Arc<BudgetViewGuard>> {
        self.guards
    }

    pub(in crate::runtime_db::input_evidence_snapshot) fn retain(
        self,
    ) -> ((usize, usize), (usize, usize), Vec<Arc<BudgetViewGuard>>) {
        (self.limits, self.usage, self.guards)
    }

    pub(in crate::runtime_db::input_evidence_snapshot) fn resume_native_phase(
        scope: &'a InputEvidenceScope,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        usage: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        ensure!(
            limits.0 <= MAX_METADATA_BYTES
                && limits.1 <= MAX_RECORDS
                && usage.0 <= limits.0
                && usage.1 <= limits.1
                && !guards.is_empty(),
            "original Native Input phase is incomplete"
        );
        for guard in &guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        if let Some(memory) = memory.as_deref() {
            memory.validate_input_accounting(scope, ledger)?;
        }
        Ok(Self {
            scope,
            ledger,
            limits,
            usage,
            guards,
            memory,
        })
    }
}
