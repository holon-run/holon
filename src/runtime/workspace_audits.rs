//! Private accounting for the original owned Workspace request. No authority.
use super::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::*;

#[derive(Debug)]
pub(crate) struct OriginalWorkspaceRequestMaterial {
    scope: resident_input_evidence::InputEvidenceScope,
    owner_epoch: u64,
    ledger: InputEvidenceBudgetLedger,
    records: usize,
    guard: Arc<BudgetViewGuard>,
    task_copy: Option<(Arc<BudgetViewGuard>, usize)>,
}
impl OriginalWorkspaceRequestMaterial {
    pub(crate) fn validate_binding(
        &self,
        scope: &resident_input_evidence::InputEvidenceScope,
        owner_epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            self.scope == *scope
                && self.owner_epoch == owner_epoch
                && self.ledger.same_ledger(ledger)
                && self.guard.matches_resource_ledger(ledger),
            "Workspace material belongs to another original scope/epoch/ledger"
        );
        if let Some((guard, _)) = &self.task_copy {
            guard.validate_owned_material_binding(scope, ledger, guard.owned_material_bytes()?)?;
        }
        Ok(())
    }
    /// Borrow an already registered actual Task copy. No second registration.
    pub(in crate::runtime) fn with_original_task_copy(
        &self,
        guard: Arc<BudgetViewGuard>,
        records: usize,
    ) -> Result<Arc<Self>> {
        anyhow::ensure!(
            self.task_copy.is_none(),
            "Workspace Task copy already supplied"
        );
        guard.validate_owned_material_binding(
            &self.scope,
            &self.ledger,
            guard.owned_material_bytes()?,
        )?;
        Ok(Arc::new(Self {
            scope: self.scope.clone(),
            owner_epoch: self.owner_epoch,
            ledger: self.ledger.clone(),
            records: self.records,
            guard: self.guard.clone(),
            task_copy: Some((guard, records)),
        }))
    }
    pub(crate) fn material_estimate(&self) -> Result<(usize, usize)> {
        let mut total = (
            usize::try_from(self.guard.owned_material_bytes()?)?,
            self.records,
        );
        if let Some((guard, records)) = &self.task_copy {
            total.0 = total
                .0
                .checked_add(usize::try_from(guard.owned_material_bytes()?)?)
                .context("Workspace Task copy bytes overflow")?;
            total.1 = total
                .1
                .checked_add(*records)
                .context("Workspace Task copy records overflow")?;
        }
        Ok(total)
    }
    pub(crate) fn visit_actual_guards(
        &self,
        mut visitor: impl FnMut(&Arc<BudgetViewGuard>) -> Result<()>,
    ) -> Result<()> {
        visitor(&self.guard)?;
        if let Some((guard, _)) = &self.task_copy {
            visitor(guard)?;
        }
        Ok(())
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_workspace_request_material(
        &self,
        source: &impl serde::Serialize,
    ) -> Result<Arc<OriginalWorkspaceRequestMaterial>> {
        let (encoded, nodes) = borrowed_json_material(&(
            self.instance_agent_id(),
            self.inner.identity_incarnation,
            self.instance_owner_epoch(),
            source,
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Workspace request native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(16384))
            .and_then(|n| n.checked_mul(4))
            .context("Workspace request copies overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("Workspace request records overflow")?;
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Workspace request cap overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            current_read_facts::CurrentReadError::ResourceExhausted
        );
        let guard = ledger.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(Arc::new(OriginalWorkspaceRequestMaterial {
            scope: resident_input_evidence::InputEvidenceScope {
                agent_id: self.instance_agent_id().into(),
                identity_incarnation: self.inner.identity_incarnation,
            },
            owner_epoch: self.instance_owner_epoch(),
            ledger: ledger.clone(),
            records,
            guard,
            task_copy: None,
        }))
    }
}
