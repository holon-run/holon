//! Explicit inactive-instance query; qualification never activates a Runtime.
use super::*;

pub(crate) enum ColdSchedulingQuery {
    Current(Box<SchedulingCurrentSnapshot>),
    ProvenEmptyNoState {
        identity: crate::types::AgentIdentityRecord,
        material_usage: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
    },
}
pub(crate) fn prepare_cold_scheduling_query_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ColdSchedulingQuery> {
    bound(0, 0, request)?;
    let mut material = Material::new(scope, request, ledger);
    let identity = loader::scope_identity(tx, scope, &mut material, false)?
        .ok_or(SchedulingSnapshotError::RetiredScope)?;
    let has_state: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1)",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    if has_state {
        let mut current = loader::load_with_revision(
            tx,
            scope,
            request,
            ledger,
            true,
            false,
            None,
            Some((material.bytes, material.records)),
            None,
            None,
        )?
        .require_current()?;
        current._read_material.extend(material.guards);
        return Ok(ColdSchedulingQuery::Current(Box::new(current)));
    }
    // No State is not proof of emptiness. No canonical/fixed/current/import
    // member may disappear behind the old stopped display fallback.
    let manifests = qualification::manifests(tx, scope, &mut material)?;
    let fresh:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1 AND mode='fresh_empty' AND seed_id IS NULL)",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    let original_empty = fresh
        || (!manifests.is_empty()
            && manifests.iter().all(|manifest| manifest.member_count == 0)
            && [
                SchedulingFamily::Timer,
                SchedulingFamily::TimerWake,
                SchedulingFamily::ExternalTrigger,
            ]
            .iter()
            .all(|family| {
                manifests
                    .iter()
                    .any(|manifest| manifest.observed_tables.contains(family))
            }));
    let empty: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM timers WHERE agent_id=?1)
          AND NOT EXISTS(SELECT 1 FROM external_triggers WHERE target_agent_id=?1)
          AND NOT EXISTS(SELECT 1 FROM timer_wakes w
            LEFT JOIN timers t ON t.timer_id=w.timer_id
            LEFT JOIN messages m ON m.message_id=w.message_id
            WHERE t.agent_id=?1 OR m.agent_id=?1)
          AND NOT EXISTS(SELECT 1 FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2)
          AND NOT EXISTS(SELECT 1 FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2)
          AND NOT EXISTS(SELECT 1 FROM scheduling_fixed_members s
            JOIN scheduling_fixed_manifests f ON f.inventory_id=s.inventory_id WHERE f.agent_id=?1)
          AND NOT EXISTS(SELECT 1 FROM scheduling_import_inventory WHERE agent_id=?1)",
        params![scope.agent_id, scope.identity_incarnation], |row|row.get(0))?;
    ensure!(original_empty && empty, SchedulingReadError::IncompleteLive);
    Ok(ColdSchedulingQuery::ProvenEmptyNoState {
        identity,
        material_usage: (material.bytes, material.records),
        guards: material.guards,
    })
}

/// A cold Host may still own a previously published ready root. Read only its
/// State/identity header; selected Scheduling remains that root's pure capture.
pub(crate) fn read_cold_scheduling_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(ContextScopeFacts, (usize, usize), Vec<Arc<BudgetViewGuard>>)> {
    bound(0, 0, request)?;
    let mut material = Material::new(scope, request, ledger);
    let facts = loader::scope_facts(tx, scope, &mut material, false)?
        .ok_or(SchedulingSnapshotError::RetiredScope)?;
    Ok((facts, (material.bytes, material.records), material.guards))
}
