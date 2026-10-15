//! Borrow one Input phase's real allowance for Task and Work decoders.
use super::*;

impl InputReadMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self.validate_binding(scope)?;
        ensure!(
            self.ledger.same_ledger(ledger),
            "Input material belongs to another original ledger"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn remaining_material_limits(&self) -> Result<(usize, usize)> {
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("joint Input remaining material");
        let mut remaining = (
            self.byte_limit
                .checked_sub(self.bytes)
                .context(exhausted())?,
            self.record_limit
                .checked_sub(self.records)
                .context(exhausted())?,
        );
        if let Some(memory) = self.memory.as_deref() {
            let actual = memory.remaining_material_limits()?;
            remaining.0 = remaining.0.min(actual.0);
            remaining.1 = remaining.1.min(actual.1);
        }
        Ok(remaining)
    }
    /// Prior Input guards stay here. Task borrows their usage, and returns only
    /// its new guards; the original Memory phase observes each new allocation.
    pub(in crate::runtime_db) fn with_task_material<T>(
        &mut self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        action: impl FnOnce(
            &mut crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        self.with_task_material_limits(scope, ledger, None, action)
    }
    /// A supplemental Task reader keeps its own stricter new-material policy.
    /// Other domains' already-held Input material is not Task-owned allocation.
    pub(in crate::runtime_db) fn with_task_material_limits<T>(
        &mut self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        task_limits: Option<(usize, usize)>,
        action: impl FnOnce(
            &mut crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        self.validate_accounting(scope, ledger)?;
        let limits = match task_limits {
            Some((bytes, records)) => (
                self.byte_limit.min(self.bytes.checked_add(bytes.min(64*1024*1024))
                    .context("joint Task own bytes overflow")?),
                self.record_limit.min(self.records.checked_add(records.min(65_536))
                    .context("joint Task own nodes overflow")?),
            ),
            None => (self.byte_limit, self.record_limit),
        };
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            byte_limit: limits.0,
            record_limit: limits.1,
        };
        let prior = crate::runtime_db::task_snapshot::TaskMaintenanceMaterial {
            bytes: self.bytes,
            records: self.records,
        };
        let mut task = crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial::new(
            self.scope,
            &request,
            Some(self.ledger),
        )
        .with_memory_budget(self.memory.as_deref_mut())
        .with_admitted_material(prior)?;
        let result = action(&mut task);
        let actual = task.material_estimate();
        let guards = task.into_guards();
        self.bytes = actual.bytes;
        self.records = actual.records;
        self.guards.extend(guards);
        result
    }
    /// The Work loader starts with this phase's true remaining limit. It owns
    /// each new guard and lends the same allocation to Memory before decode.
    pub(in crate::runtime_db) fn prepare_original_work_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        state: &crate::types::AgentState,
        capture: &crate::runtime::resident_work::WorkCaptureRequest,
        task_roots: Option<&BTreeSet<String>>,
        pending: &[crate::types::MessageEnvelope],
        current_roots_only: bool,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(
        crate::runtime_db::work_snapshot::WorkCurrentSnapshot,
        BTreeSet<String>,
    )> {
        self.validate_accounting(scope, ledger)?;
        let remaining = self.remaining_material_limits()?;
        let (work, selected) =
            crate::runtime_db::work_snapshot::prepare_original_work_for_input_recovery_tx(
                tx,
                scope,
                state,
                capture,
                task_roots,
                pending,
                current_roots_only,
                (remaining.1, remaining.0),
                ledger,
                self.memory.as_deref_mut(),
            )?;
        self.observe_work_prior(&work)?;
        Ok((work, selected))
    }
}

#[cfg(test)]
#[path = "input_read_domain_joint_tests.rs"]
mod tests;
