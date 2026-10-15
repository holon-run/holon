//! The scheduler's original owned projections share one finite selection phase.
use super::*;
use crate::runtime::current_read_facts::CurrentReadError;
use crate::runtime::input_evidence_budget::{
    borrowed_json_material, InputEvidenceBudgetLedger, OwnedCompletionBudget,
};
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::runtime::run_birth_sessions::add_material;

/// This is only the existing output guard's admission counter. It neither selects
/// records nor creates another ledger or mutation owner. Peak scratch stays charged.
pub(super) struct SchedulerOutputMaterial<'a> {
    ledger: &'a InputEvidenceBudgetLedger,
    scope: &'a InputEvidenceScope,
    limits: (u64, usize),
    prior: (u64, usize),
    own: (u64, usize),
    guard: OwnedCompletionBudget,
}
impl<'a> SchedulerOutputMaterial<'a> {
    pub(super) fn new(
        ledger: &'a InputEvidenceBudgetLedger,
        scope: &'a InputEvidenceScope,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<Self> {
        let configured = ledger.limits();
        let limits = (
            u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
                configured
                    .history_per_agent_bytes
                    .checked_add(configured.pending_per_agent_bytes)
                    .context(CurrentReadError::ResourceExhausted)?,
            ),
            limits
                .1
                .min(65536)
                .min(usize::try_from(configured.record_limit)?),
        );
        // Includes the output carrier, its guard/ledger headers, selectors and
        // fixed stack-to-owned projection fields, before their first allocation.
        let own = (8192, 64);
        Self::check(limits, add_material(prior, own)?)?;
        let guard = ledger.reserve_owned_completion(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: own.0,
            },
        )?;
        Ok(Self {
            ledger,
            scope,
            limits,
            prior,
            own,
            guard,
        })
    }
    fn check(limits: (u64, usize), total: (u64, usize)) -> Result<()> {
        anyhow::ensure!(
            total.0 <= limits.0 && total.1 <= limits.1,
            CurrentReadError::ResourceExhausted
        );
        Ok(())
    }
    pub(super) fn prior(&self) -> Result<(u64, usize)> {
        add_material(self.prior, self.own)
    }
    pub(super) fn observe(&mut self, bytes: u64, nodes: usize) -> Result<()> {
        let actual = (
            bytes
                .checked_add(
                    u64::try_from(nodes)?
                        .checked_mul(256)
                        .context(CurrentReadError::ResourceExhausted)?,
                )
                .context(CurrentReadError::ResourceExhausted)?,
            nodes,
        );
        let next = add_material(self.own, actual)?;
        Self::check(self.limits, add_material(self.prior, next)?)?;
        // The sole original guard grows before the caller performs its clone.
        self.guard.observe_material(bytes, actual.0 - bytes);
        self.own = next;
        Ok(())
    }
    pub(super) fn json(&mut self, value: &impl serde::Serialize) -> Result<()> {
        let (bytes, nodes) = borrowed_json_material(value)?;
        self.observe(bytes, nodes)
    }
    pub(super) fn slots(&mut self, count: usize) -> Result<()> {
        self.observe(0, count)
    }
    /// The Trigger reader already registered its original guard after checking
    /// this exact prior, before copying. Borrow it once; never register it again.
    pub(super) fn borrowed_original(
        &mut self,
        guard: &Arc<BudgetViewGuard>,
        cost: (usize, usize),
    ) -> Result<()> {
        let cost = (u64::try_from(cost.0)?, cost.1);
        guard.validate_owned_material_binding(self.scope, self.ledger, cost.0)?;
        let next = add_material(self.own, cost)?;
        Self::check(self.limits, add_material(self.prior, next)?)?;
        self.own = next;
        Ok(())
    }
    pub(super) fn queue(&mut self, queue: &WorkQueueReadModel) -> Result<()> {
        self.json(&(
            &queue.current,
            &queue.queued_blocked,
            &queue.items,
            &queue.current_runnable,
            &queue.triggered_blocked,
            &queue.queued_runnable,
            &queue.yielded,
            &queue.waiting_for_operator,
            &queue.blocked,
            &queue.completing,
            &queue.completed_recent,
        ))
    }
    pub(super) fn finish(self) -> ((u64, usize), Arc<BudgetViewGuard>) {
        (self.own, self.guard.guard())
    }
}
