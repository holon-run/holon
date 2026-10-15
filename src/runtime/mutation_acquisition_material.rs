//! Actual candidate/base metadata; this allocation grants no read or mutation authority.
use super::*;
use crate::runtime::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use crate::runtime::input_queries::{actual_query_material_limits, InputQueryError};
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use anyhow::ensure;

#[derive(Debug)]
pub(crate) struct OriginalMutationAcquisitionMaterial {
    scope: InputEvidenceScope,
    epoch: u64,
    ledger: InputEvidenceBudgetLedger,
    cost: (u64, usize),
    guard: Arc<BudgetViewGuard>,
}
impl OriginalMutationAcquisitionMaterial {
    pub(crate) fn validate(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.epoch == epoch && self.ledger.same_ledger(ledger),
            "mutation acquisition material crossed original scope/epoch/ledger"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, self.cost.0)
            .map_err(Into::into)
    }
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.cost
    }
    pub(crate) fn visit_material(
        &self,
        visit: &mut impl FnMut(&Arc<BudgetViewGuard>, u64, usize) -> Result<()>,
    ) -> Result<()> {
        self.validate(&self.scope, self.epoch, &self.ledger)?;
        visit(&self.guard, self.cost.0, self.cost.1)
    }
    pub(super) fn for_agent(
        runtime: &RuntimeHandle,
        agent: &RuntimeAgent,
        registrations: usize,
        owners: usize,
        prior: (u64, usize),
        limits: (usize, usize),
    ) -> Result<Arc<Self>> {
        let ledger = &runtime.inner.input_evidence_budget;
        let limits = actual_query_material_limits(ledger, limits)?;
        // RuntimeAgent Clone copies two State values and the abort run ID;
        // Queue Clone copies only Arc slots. Canonical Message bodies stay shared.
        let (encoded, nodes) = borrowed_json_material(&(
            &agent.state,
            &agent.last_persisted_state,
            agent
                .current_run_abort
                .as_ref()
                .map(|abort| abort.run_id.as_str()),
        ))?;
        let scope_bytes = u64::try_from(runtime.instance_agent_id().len())?;
        let slots = registrations
            .checked_add(
                agent
                    .queue
                    .len()
                    .checked_mul(2)
                    .context("mutation queue slots overflow")?,
            )
            .and_then(|n| n.checked_add(owners.checked_mul(8)?))
            .and_then(|n| n.checked_add(64))
            .context(InputQueryError::ResourceExhausted(
                "mutation acquisition slots",
            ))?;
        let nodes = nodes
            .checked_mul(2)
            .and_then(|n| n.checked_add(slots.checked_mul(4)?))
            .context(InputQueryError::ResourceExhausted(
                "mutation acquisition nodes",
            ))?;
        let bytes = encoded
            .checked_mul(2)
            .and_then(|n| n.checked_add(scope_bytes.checked_mul(8)?))
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .and_then(|n| n.checked_add(8192))
            .context(InputQueryError::ResourceExhausted(
                "mutation acquisition bytes",
            ))?;
        ensure!(
            prior
                .0
                .checked_add(bytes)
                .is_some_and(|n| n <= limits.0 as u64)
                && prior.1.checked_add(nodes).is_some_and(|n| n <= limits.1),
            InputQueryError::ResourceExhausted("mutation acquisition first copy")
        );
        let address = ledger.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: scope_bytes
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(1024))
                    .context("mutation address overflow")?,
            },
        )?;
        let scope = InputEvidenceScope {
            agent_id: runtime.instance_agent_id().into(),
            identity_incarnation: runtime.instance_incarnation(),
        };
        let guard = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        drop(address);
        Ok(Arc::new(Self {
            scope,
            epoch: runtime.instance_owner_epoch(),
            ledger: ledger.clone(),
            cost: (bytes, nodes),
            guard,
        }))
    }
}

#[cfg(test)]
#[path = "mutation_acquisition_material_tests.rs"]
mod tests;
