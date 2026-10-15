//! Actual selected-model allocation material; no catalog or publication is retained.
use super::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate, OwnedCompletionBudget,
};
use super::resident_input_evidence::InputEvidenceScope;
use super::*;
use anyhow::ensure;
use crate::model_catalog::SelectedModelObserver;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SelectedModelReadError {
    #[error("ResourceExhausted: selected model read material")]
    ResourceExhausted,
    #[error("selected model read material belongs to another scope or ledger")]
    ForeignMaterial,
}
/// Own material excludes the already guarded caller prior. The one reservation
/// covers the actual resolution temporaries and its result until its last user.
pub(crate) struct SelectedModelReadMaterial {
    own: (u64, usize),
    guard: Arc<BudgetViewGuard>,
}
impl SelectedModelReadMaterial {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.own
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self.guard
            .validate_owned_material_binding(scope, ledger, self.own.0)
            .context(SelectedModelReadError::ForeignMaterial)
    }
}
struct ModelReadPhase {
    limits: (u64, usize),
    prior: (u64, usize),
    used: (u64, usize),
    owned: OwnedCompletionBudget,
}
impl ModelReadPhase {
    fn new(
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        original: &BudgetViewGuard,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<Self> {
        original
            .validate_owned_material_binding(scope, ledger, 0)
            .context(SelectedModelReadError::ForeignMaterial)?;
        ensure!(
            scope.identity_incarnation > 0 && !scope.agent_id.is_empty(),
            SelectedModelReadError::ForeignMaterial
        );
        let configured = ledger.limits();
        let limits = (
            u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
                configured
                    .history_per_agent_bytes
                    .checked_add(configured.pending_per_agent_bytes)
                    .context(SelectedModelReadError::ResourceExhausted)?,
            ),
            limits
                .1
                .min(65_536)
                .min(usize::try_from(configured.record_limit)?),
        );
        let used = (1024, 4);
        Self::admit(limits, prior, used)?;
        let owned = ledger.reserve_owned_completion(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: used.0,
            },
        )?;
        Ok(Self {
            limits,
            prior,
            used,
            owned,
        })
    }
    fn admit(limits: (u64, usize), prior: (u64, usize), used: (u64, usize)) -> Result<()> {
        let bytes = prior
            .0
            .checked_add(used.0)
            .context(SelectedModelReadError::ResourceExhausted)?;
        let nodes = prior
            .1
            .checked_add(used.1)
            .context(SelectedModelReadError::ResourceExhausted)?;
        ensure!(
            bytes <= limits.0 && nodes <= limits.1,
            SelectedModelReadError::ResourceExhausted
        );
        Ok(())
    }
    fn charge(&mut self, payload: u64, nodes: usize) -> Result<()> {
        let bytes = payload
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context(SelectedModelReadError::ResourceExhausted)?,
            )
            .context(SelectedModelReadError::ResourceExhausted)?;
        let next = (
            self.used
                .0
                .checked_add(bytes)
                .context(SelectedModelReadError::ResourceExhausted)?,
            self.used
                .1
                .checked_add(nodes)
                .context(SelectedModelReadError::ResourceExhausted)?,
        );
        Self::admit(self.limits, self.prior, next)?;
        self.owned.observe_material(0, bytes);
        self.used = next;
        Ok(())
    }
    fn finish(self) -> SelectedModelReadMaterial {
        SelectedModelReadMaterial {
            own: self.used,
            guard: self.owned.guard(),
        }
    }
}
impl SelectedModelObserver for ModelReadPhase {
    fn observe<T: serde::Serialize + ?Sized>(&mut self, value: &T, copies: usize) -> Result<()> {
        let (payload, nodes) = borrowed_json_material(&value)?;
        let nodes = nodes
            .checked_add(1)
            .and_then(|n| n.checked_mul(copies))
            .context(SelectedModelReadError::ResourceExhausted)?;
        let payload = payload
            .checked_mul(u64::try_from(copies)?)
            .context(SelectedModelReadError::ResourceExhausted)?;
        self.charge(payload, nodes)
    }
    fn observe_raw_bytes(&mut self, bytes: u64, native_slots: usize) -> Result<()> {
        self.charge(bytes, native_slots)
    }
}
/// The caller supplies one held, qualified scheduler State and its actual guard.
/// Config is one immutable selected snapshot for this entire synchronous read.
pub(super) fn capture_selected_model_state(
    catalog: &RuntimeModelCatalog,
    config: &ContextConfig,
    state: &AgentState,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    original: &BudgetViewGuard,
    limits: (usize, usize),
    prior: (u64, usize),
) -> Result<(AgentModelState, SelectedModelReadMaterial)> {
    ensure!(
        state.id == scope.agent_id,
        SelectedModelReadError::ForeignMaterial
    );
    let mut phase = ModelReadPhase::new(scope, ledger, original, limits, prior)?;
    let selected =
        super::agent_model_state_for_catalog_observed(catalog, config, state, &mut phase)?;
    Ok((selected, phase.finish()))
}
#[cfg(test)]
#[path = "selected_model_read_tests.rs"]
mod tests;
