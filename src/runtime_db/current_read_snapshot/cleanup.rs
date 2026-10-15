//! Finite existing cleanup observations are metadata, not business permission.
use super::*;
pub(in crate::runtime_db) struct ExistingCurrentReadCleanup {
    pub keys: BTreeSet<CurrentReadKey>,
    pub material: (usize, usize),
    pub guards: Vec<Arc<BudgetViewGuard>>,
}
pub(in crate::runtime_db) fn qualified_existing_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<ExistingCurrentReadCleanup> {
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    material.charge(
        usize::try_from(serialized_bytes(keys)?)?,
        keys.len()
            .checked_add(8)
            .context("CurrentRead cleanup count overflow")?,
    )?;
    let mut qualified = BTreeSet::new();
    for key in keys {
        if let Some((_, availability, _, _)) = loader::eligible(tx, scope, key, &mut material)? {
            ensure!(
                availability == CurrentReadAvailability::Present,
                CurrentReadError::Pruned(key.clone())
            );
            qualified.insert(key.clone());
        }
    }
    Ok(ExistingCurrentReadCleanup {
        keys: qualified,
        material: (material.bytes, material.records),
        guards: material.guards,
    })
}

/// Validate saved metadata even when a named original receipt cannot install
/// complete bodies. Corrupt coverage must not be hidden by an unknown raw row.
pub(in crate::runtime_db) fn observe_original_incomplete_recovery_using_input_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    input: &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    input.validate_current_allowance(scope, ledger, request)?;
    let mut material = Material::new(scope, request, ledger);
    let (identity, _) = loader::scope_facts(tx, scope, state, &mut material, true)?;
    if let Some(saved) = loader::saved(tx, scope, &mut material)? {
        ensure!(
            saved.binding_invalidated
                || saved.records_invalidated
                || (saved.state_digest == typed_digest(state)?
                    && saved.identity_digest == typed_digest(&identity)?),
            CurrentReadError::CanonicalChanged
        );
    }
    input.observe_current_prior(&ObservedCurrentReadMaterial {
        scope,
        ledger,
        usage: (material.bytes, material.records),
        guards: &material.guards,
    })?;
    Ok(material.guards)
}
