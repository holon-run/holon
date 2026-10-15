//! One actual Scheduling decoder lends its new guards to the original Input phase.
use super::*;

pub(in crate::runtime_db) struct ObservedSchedulingReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    usage: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl ObservedSchedulingReadMaterial<'_> {
    pub(in crate::runtime_db) fn into_original_parts(
        self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<((usize, usize), Vec<Arc<BudgetViewGuard>>)> {
        ensure!(
            self.scope == scope
                && self.ledger.same_ledger(ledger)
                && !self.guards.is_empty()
                && self
                    .guards
                    .iter()
                    .all(|guard| guard.matches_resource_ledger(ledger)),
            "Scheduling material belongs to another original Input scope/ledger"
        );
        Ok((self.usage, self.guards))
    }
}

pub(in crate::runtime_db) fn prepare_scheduling_for_input_tx<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    original_recovery: bool,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<(
    SchedulingCurrentSnapshot,
    ObservedSchedulingReadMaterial<'a>,
)> {
    let mut current =
        loader::load_for_input(tx, scope, request, ledger, original_recovery, memory)?
            .require_current()?;
    ensure!(
        current.resident.scope() == scope,
        "actual Scheduling Input scope differs"
    );
    let observed = ObservedSchedulingReadMaterial {
        scope,
        ledger,
        usage: current.material_usage,
        guards: std::mem::take(&mut current._read_material),
    };
    Ok((current, observed))
}
