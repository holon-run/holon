//! Existing canonical cleanup retains only original Scheduling membership.
use super::*;
#[path = "bootstrap_wakes.rs"]
mod bootstrap_wakes;
pub(crate) use bootstrap_wakes::prepare_bootstrap_wake_normalization_tx;

struct ExistingTarget {
    content: String,
    projection: String,
    alias: Option<Alias>,
    qualified: bool,
}
/// Qualification proof only. The original actor retains its business fences.
/// NoState is supported without reading State or constructing a Ready domain.
pub(crate) struct ExistingSchedulingMaintenanceProof<'tx, 'conn> {
    inner: SchedulingWriteProof<'tx, 'conn>,
    targets: BTreeMap<SchedulingKey, ExistingTarget>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingScopeInvalidation {
    pub base_revision: u64,
    pub revision: u64,
}
#[derive(Debug)]
pub(crate) struct SchedulingMaintenanceAck {
    // Original validated ACK address; this sparse result cannot install Ready.
    pub _scope: InputEvidenceScope,
    pub qualified_actual_keys: BTreeSet<SchedulingKey>,
    pub invalidation: Option<SchedulingScopeInvalidation>,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl SchedulingMaintenanceAck {
    pub(crate) fn take_budget_views(&mut self) -> Vec<Arc<BudgetViewGuard>> {
        std::mem::take(&mut self.guards)
    }
}
impl ExistingSchedulingMaintenanceProof<'_, '_> {
    pub(crate) fn qualified_keys(&self) -> impl Iterator<Item = &SchedulingKey> {
        self.targets
            .iter()
            .filter_map(|(key, target)| target.qualified.then_some(key))
    }
}
pub(crate) fn prepare_existing_scheduling_maintenance_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ExistingSchedulingMaintenanceProof<'tx, 'conn>> {
    let mut material = Material::new(scope, request, ledger);
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys))?)?,
        keys.len() + 1,
    )?;
    loader::scope_identity(tx, scope, &mut material, true)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    let manifests = qualification::original_maintenance_manifests(tx, scope, &mut material)?;
    let current = loader::current(tx, scope, &mut material)?;
    let mut targets = BTreeMap::new();
    let mut before = BTreeMap::new();
    for key in keys {
        ensure!(!key.id.is_empty(), "empty existing Scheduling target");
        if key.family == SchedulingFamily::TimerWake {
            let (timer, _) = key.wake_pair()?;
            ensure!(
                keys.contains(&SchedulingKey::new(SchedulingFamily::Timer, timer)),
                "Scheduling maintenance Wake omitted its owning Timer"
            );
        }
        let alias = read_alias(tx, scope, key, &mut material)?;
        ensure!(
            !alias
                .as_ref()
                .is_some_and(|a| a.availability == SchedulingAvailability::Pruned),
            "Scheduling maintenance cannot revive Pruned membership"
        );
        let mut qualified = alias
            .as_ref()
            .is_some_and(|a| a.availability == SchedulingAvailability::Present);
        if alias.is_none() {
            for manifest in manifests.iter().rev() {
                material.charge(128, 0)?;
                if qualification::fixed_digest(tx, manifest, key)?.is_some() {
                    qualified = true;
                    break;
                }
            }
        }
        // A raw existing target may be cleaned by its original business actor,
        // but an absent ID or a foreign canonical body cannot be adopted here.
        let value = stored::read_stored(tx, key, &scope.agent_id, Some(&mut material))?
            .context("existing Scheduling maintenance target missing")?;
        if qualified {
            loader::member_revision(tx, scope, key, &value, &manifests, &mut material)?;
        }
        targets.insert(
            key.clone(),
            ExistingTarget {
                content: value.content.clone(),
                projection: value.projection.clone(),
                alias,
                qualified,
            },
        );
        if qualified {
            before.insert(key.clone(), Some(value));
        }
    }
    Ok(ExistingSchedulingMaintenanceProof {
        inner: SchedulingWriteProof {
            tx,
            scope: scope.clone(),
            state_before: None,
            base_revision: current.as_ref().map_or(1, |c| c.revision),
            original_recovery: true,
            had_coverage: current.is_some(),
            before,
            material_bytes: material.bytes,
            material_records: material.records,
            guards: material.guards,
            ledger: ledger.clone(),
            preparation_phase: None,
        },
        targets,
    })
}
fn validate_actual(
    tx: &Transaction<'_>,
    proof: &mut ExistingSchedulingMaintenanceProof<'_, '_>,
    actual: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
) -> Result<(BTreeSet<SchedulingKey>, bool)> {
    ensure!(
        std::ptr::eq(tx, proof.inner.tx),
        "Scheduling maintenance crossed transaction"
    );
    ensure!(
        actual.iter().all(|key| proof.targets.contains_key(key)),
        "Scheduling maintenance actual footprint exceeds original targets"
    );
    let mut material = Material::new(&proof.inner.scope, request, &proof.inner.ledger);
    material.bytes = proof.inner.material_bytes;
    material.records = proof.inner.material_records;
    loader::scope_identity(tx, &proof.inner.scope, &mut material, true)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    let current = loader::current(tx, &proof.inner.scope, &mut material)?;
    ensure!(
        current.as_ref().map_or(1, |c| c.revision) == proof.inner.base_revision
            && current.is_some() == proof.inner.had_coverage,
        "Scheduling maintenance base coverage changed"
    );
    let mut selected = BTreeSet::new();
    let mut raw_changed = false;
    for (key, target) in &proof.targets {
        ensure!(
            read_alias(tx, &proof.inner.scope, key, &mut material)? == target.alias,
            "Scheduling maintenance membership changed during business SQL"
        );
        if target.qualified {
            if actual.contains(key) {
                selected.insert(key.clone());
            }
            // The common finalizer (or sparse readback below) checks these bodies.
            continue;
        }
        let after = stored::read_stored(tx, key, &proof.inner.scope.agent_id, Some(&mut material))?
            .context("Scheduling maintenance cannot remove a canonical target")?;
        let changed = after.content != target.content || after.projection != target.projection;
        ensure!(
            !changed || actual.contains(key),
            "Scheduling maintenance changed an undeclared target"
        );
        raw_changed |= actual.contains(key) && changed;
    }
    proof.inner.material_bytes = material.bytes;
    proof.inner.material_records = material.records;
    proof.inner.guards.extend(material.guards);
    Ok((selected, raw_changed))
}
/// Current actor only: its actual final State and all complete live frontiers
/// are validated by the same ordinary finalizer. Raw cleanup never adds aliases.
pub(crate) fn register_existing_scheduling_cleanup_tx(
    tx: &Transaction<'_>,
    mut proof: ExistingSchedulingMaintenanceProof<'_, '_>,
    actual: &BTreeSet<SchedulingKey>,
    final_state: &AgentState,
    request: &SchedulingDomainLoadRequest,
) -> Result<CommittedSchedulingFacts> {
    let (selected, raw_changed) = validate_actual(tx, &mut proof, actual, request)?;
    register_scheduling_writes_inner(
        tx,
        &proof.inner,
        &selected,
        final_state,
        request,
        None,
        raw_changed,
    )
}
/// Sparse original-scope maintenance ACK; it cannot install a Ready domain.
pub(crate) fn register_existing_scheduling_maintenance_tx(
    tx: &Transaction<'_>,
    mut proof: ExistingSchedulingMaintenanceProof<'_, '_>,
    actual: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
) -> Result<SchedulingMaintenanceAck> {
    let (selected, raw_changed) = validate_actual(tx, &mut proof, actual, request)?;
    let mut material = Material::new(&proof.inner.scope, request, &proof.inner.ledger);
    material.bytes = proof.inner.material_bytes;
    material.records = proof.inner.material_records;
    let mut after = BTreeMap::new();
    let mut changed = raw_changed;
    for (key, before) in &proof.inner.before {
        let value = stored::read_stored(tx, key, &proof.inner.scope.agent_id, Some(&mut material))?
            .context("Scheduling maintenance actual row missing")?;
        let before = before
            .as_ref()
            .context("Scheduling original member missing")?;
        let different = before.content != value.content || before.projection != value.projection;
        ensure!(
            !different || selected.contains(key),
            "Scheduling maintenance changed an undeclared qualified target"
        );
        if selected.contains(key) {
            changed |= different;
            after.insert(key, value);
        }
    }
    let revision = proof
        .inner
        .base_revision
        .checked_add(u64::from(changed))
        .context("Scheduling maintenance revision overflow")?;
    for (key, value) in after {
        write_alias(
            tx,
            &proof.inner.scope,
            key,
            revision,
            SchedulingAvailability::Present,
            Some(&value.content),
            Some(&value.projection),
            Some((&value.status, &value.sort_at, &value.created_at)),
        )?;
    }
    let invalidation = if changed {
        let mut coverage = SchedulingCoverage::empty(revision);
        coverage.active_timers_complete = false;
        coverage.pending_wakes_complete = false;
        coverage.active_triggers_complete = false;
        material.charge(usize::try_from(serialized_bytes(&coverage)?)?, 1)?;
        tx.execute("INSERT INTO scheduling_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",
            params![proof.inner.scope.agent_id,proof.inner.scope.identity_incarnation,revision,serde_json::to_string(&coverage)?])?;
        Some(SchedulingScopeInvalidation {
            base_revision: proof.inner.base_revision,
            revision,
        })
    } else {
        None
    };
    let mut guards = proof.inner.guards.clone();
    guards.extend(material.guards);
    Ok(SchedulingMaintenanceAck {
        _scope: proof.inner.scope.clone(),
        qualified_actual_keys: selected,
        invalidation,
        guards,
    })
}
