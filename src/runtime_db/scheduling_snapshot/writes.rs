//! Finite original transaction proof. Business commands retain their own fences.
use super::*;
use crate::types::AgentState;
#[path = "trigger_observation.rs"]
mod trigger_observation;
pub(crate) use trigger_observation::ExternalTriggerWriterObservation;
#[path = "maintenance.rs"]
mod maintenance;
pub(crate) use maintenance::{
    prepare_bootstrap_wake_normalization_tx, prepare_existing_scheduling_maintenance_tx,
    register_existing_scheduling_cleanup_tx, register_existing_scheduling_maintenance_tx,
    ExistingSchedulingMaintenanceProof, SchedulingMaintenanceAck, SchedulingScopeInvalidation,
};

pub(crate) struct SchedulingWriteProof<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    base_revision: u64,
    original_recovery: bool,
    had_coverage: bool,
    before: BTreeMap<SchedulingKey, Option<stored::StoredScheduling>>,
    material_bytes: usize,
    material_records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
    ledger: InputEvidenceBudgetLedger,
}
impl SchedulingWriteProof<'_, '_> {
    pub(in crate::runtime_db) fn input_writer_material(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(std::ptr::eq(self.tx, tx) && self.scope == *scope
            && self.ledger.same_ledger(ledger) && !self.guards.is_empty(),
            "Native Input Scheduling prior differs from original TX");
        for guard in &self.guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok((self.material_bytes, self.material_records))
    }
    pub(crate) fn fact_before(&self, key: &SchedulingKey) -> Option<&CanonicalSchedulingFact> {
        self.before
            .get(key)
            .and_then(|value| value.as_ref())
            .map(|value| &value.fact)
    }
    pub(crate) fn remaining_request(
        &self,
        request: &SchedulingDomainLoadRequest,
    ) -> Result<SchedulingDomainLoadRequest> {
        let bytes = request
            .byte_limit
            .min(MAX_BYTES)
            .checked_sub(self.material_bytes)
            .filter(|n| *n > 0)
            .ok_or(SchedulingSnapshotError::ResourceExhausted(
                "Scheduling external material",
            ))?;
        let records = request
            .record_limit
            .min(MAX_RECORDS)
            .checked_sub(self.material_records)
            .filter(|n| *n > 0)
            .ok_or(SchedulingSnapshotError::ResourceExhausted(
                "Scheduling external material",
            ))?;
        Ok(SchedulingDomainLoadRequest {
            // This view is only an allowance for frozen external material.
            // The original finalizer still uses its full original selector.
            capture: SchedulingCaptureRequest::current(),
            byte_limit: bytes,
            record_limit: records,
        })
    }
    pub(crate) fn absorb_material(
        &mut self,
        bytes: usize,
        records: usize,
        request: &SchedulingDomainLoadRequest,
    ) -> Result<()> {
        self.material_bytes = self
            .material_bytes
            .checked_add(bytes)
            .context("Scheduling external bytes overflow")?;
        self.material_records = self
            .material_records
            .checked_add(records)
            .context("Scheduling external count overflow")?;
        bound(self.material_records, self.material_bytes, request)
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
}
#[derive(Debug)]
pub(crate) struct CommittedSchedulingFacts {
    pub delta: CommittedSchedulingDelta,
    // Original pending-source metadata; Queue/Input ACK owns live source installation.
    pub _input_roots: BTreeSet<InputEvidenceKey>,
    pub requires_prepared_domain: bool,
    guards: Vec<Arc<BudgetViewGuard>>,
    pub(in crate::runtime_db) material_usage: (usize,usize),
}
impl CommittedSchedulingFacts {
    pub(crate) fn take_budget_views(&mut self) -> Vec<Arc<BudgetViewGuard>> {
        std::mem::take(&mut self.guards)
    }
    pub(crate) fn append_budget_views(&mut self, guards: Vec<Arc<BudgetViewGuard>>) {
        self.guards.extend(guards);
    }
}
pub(crate) fn prepare_scheduling_write_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<SchedulingWriteProof<'tx, 'conn>> {
    prepare(tx, scope, keys, request, ledger, false, None)
}
pub(crate) fn prepare_scheduling_write_with_memory_budget_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<SchedulingWriteProof<'tx, 'conn>> {
    prepare(tx, scope, keys, request, ledger, false, memory)
}
/// Only existing admitted members, with exact same-inc Deleting identity/State.
/// This is qualification maintenance, not authority for the business mutation.
pub(crate) fn prepare_original_scheduling_cleanup_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<SchedulingWriteProof<'tx, 'conn>> {
    ensure!(!keys.is_empty(), "empty original Scheduling cleanup");
    prepare(tx, scope, keys, request, ledger, true, None)
}
fn prepare<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<SchedulingWriteProof<'tx, 'conn>> {
    let mut material = Material::new(scope, request, ledger);
    material.memory_budget = memory;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys))?)?,
        keys.len()
            .checked_add(1)
            .context("Scheduling proof selector count overflow")?,
    )?;
    loader::scope_facts(tx, scope, &mut material, original_recovery)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    let checkpoints = qualification::manifests(tx, scope, &mut material)?;
    let current = loader::current(tx, scope, &mut material)?;
    let revision = current.as_ref().map_or(1, |c| c.revision);
    let mut before = BTreeMap::new();
    observe_keys(
        tx,
        scope,
        keys,
        original_recovery,
        &checkpoints,
        &mut material,
        &mut before,
    )?;
    Ok(SchedulingWriteProof {
        tx,
        scope: scope.clone(),
        base_revision: revision,
        original_recovery,
        had_coverage: current.is_some(),
        before,
        material_bytes: material.bytes,
        material_records: material.records,
        guards: material.guards,
        ledger: ledger.clone(),
    })
}
fn observe_keys(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<SchedulingKey>,
    original_recovery: bool,
    checkpoints: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
    before: &mut BTreeMap<SchedulingKey, Option<stored::StoredScheduling>>,
) -> Result<()> {
    for key in keys {
        if before.contains_key(key) {
            continue;
        }
        if key.family == SchedulingFamily::TimerWake {
            let (timer, _) = key.wake_pair()?;
            ensure!(
                keys.contains(&SchedulingKey::new(SchedulingFamily::Timer, timer)),
                "Scheduling Wake proof omitted its owning Timer"
            );
        }
        let alias = read_alias(tx, scope, key, material)?;
        ensure!(
            alias
                .as_ref()
                .is_none_or(|a| a.availability != SchedulingAvailability::Pruned),
            "Scheduling Pruned member cannot be revived"
        );
        let mut qualified = alias
            .as_ref()
            .is_some_and(|a| a.availability == SchedulingAvailability::Present);
        if !qualified {
            for checkpoint in checkpoints {
                material.charge(128, 0)?;
                if qualification::fixed_digest(tx, checkpoint, key)?.is_some() {
                    qualified = true;
                    break;
                }
            }
        }
        if qualified {
            let value = stored::read_stored(tx, key, &scope.agent_id, Some(material))?
                .ok_or_else(|| SchedulingSnapshotError::CanonicalChanged(key.clone()))?;
            loader::member_revision(tx, scope, key, &value, &checkpoints, material)?;
            before.insert(key.clone(), Some(value));
        } else {
            ensure!(
                !original_recovery
                    && !stored::exists_global(tx, key)?
                    && !qualification::known_anywhere(tx, key)?,
                SchedulingSnapshotError::NeedsPreparation(key.clone())
            );
            before.insert(key.clone(), None);
        }
    }
    Ok(())
}
/// Additional closed targets, observed before any business SQL in the same TX.
pub(crate) fn reserve_scheduling_write_keys_tx(
    tx: &Transaction<'_>,
    proof: &mut SchedulingWriteProof<'_, '_>,
    keys: &BTreeSet<SchedulingKey>,
    request: &SchedulingDomainLoadRequest,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<()> {
    ensure!(
        std::ptr::eq(tx, proof.tx),
        "Scheduling reserve belongs to another transaction"
    );
    let mut material = Material::new(&proof.scope, request, &proof.ledger);
    material.memory_budget = memory;
    material.bytes = proof.material_bytes;
    material.records = proof.material_records;
    material.charge(usize::try_from(serialized_bytes(keys)?)?, keys.len())?;
    let checkpoints = qualification::manifests(tx, &proof.scope, &mut material)?;
    observe_keys(
        tx,
        &proof.scope,
        keys,
        proof.original_recovery,
        &checkpoints,
        &mut material,
        &mut proof.before,
    )?;
    proof.material_bytes = material.bytes;
    proof.material_records = material.records;
    proof.guards.extend(material.guards);
    Ok(())
}
pub(crate) fn register_scheduling_writes_tx(
    tx: &Transaction<'_>,
    proof: &SchedulingWriteProof<'_, '_>,
    actual_keys: &BTreeSet<SchedulingKey>,
    final_state: &AgentState,
    request: &SchedulingDomainLoadRequest,
) -> Result<CommittedSchedulingFacts> {
    register_scheduling_writes_with_memory_budget_tx(
        tx,
        proof,
        actual_keys,
        final_state,
        request,
        None,
    )
}
pub(crate) fn register_scheduling_writes_with_memory_budget_tx(
    tx: &Transaction<'_>,
    proof: &SchedulingWriteProof<'_, '_>,
    actual_keys: &BTreeSet<SchedulingKey>,
    final_state: &AgentState,
    request: &SchedulingDomainLoadRequest,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<CommittedSchedulingFacts> {
    register_scheduling_writes_inner(tx, proof, actual_keys, final_state, request, memory, false)
}
fn register_scheduling_writes_inner(
    tx: &Transaction<'_>,
    proof: &SchedulingWriteProof<'_, '_>,
    actual_keys: &BTreeSet<SchedulingKey>,
    final_state: &AgentState,
    request: &SchedulingDomainLoadRequest,
    mut memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    raw_changed: bool,
) -> Result<CommittedSchedulingFacts> {
    ensure!(
        std::ptr::eq(tx, proof.tx),
        "Scheduling proof belongs to another transaction"
    );
    ensure!(
        final_state.id == proof.scope.agent_id
            && actual_keys.iter().all(|key| proof.before.contains_key(key)),
        "Scheduling actual footprint exceeds original proof"
    );
    let mut material = Material::new(&proof.scope, request, &proof.ledger);
    material.memory_budget = memory.as_deref_mut();
    material.bytes = proof.material_bytes;
    material.records = proof.material_records;
    let facts = loader::scope_facts(tx, &proof.scope, &mut material, proof.original_recovery)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    ensure!(
        facts.state.as_ref() == Some(final_state),
        "Scheduling final State differs from actual business ACK"
    );
    let current = loader::current(tx, &proof.scope, &mut material)?;
    ensure!(
        current.as_ref().map_or(1, |c| c.revision) == proof.base_revision
            && current.is_some() == proof.had_coverage,
        SchedulingSnapshotError::RevisionMismatch {
            expected: proof.base_revision,
            actual: current.as_ref().map(|c| c.revision)
        }
    );
    let mut after = BTreeMap::new();
    let mut changed = raw_changed;
    for (key, before) in &proof.before {
        let actual = stored::read_stored(tx, key, &proof.scope.agent_id, Some(&mut material))?;
        let equal = match (before, actual.as_ref()) {
            (Some(a), Some(b)) => a.content == b.content && a.projection == b.projection,
            (None, None) => true,
            _ => false,
        };
        ensure!(
            equal || actual_keys.contains(key),
            "Scheduling business changed an undeclared actual target"
        );
        if actual_keys.contains(key) {
            ensure!(actual.is_some(),"Scheduling upsert ACK has no actual canonical row; removal needs its own maintenance actor");
            changed |= !equal;
            after.insert(key.clone(), actual.unwrap());
        }
    }
    let revision = proof
        .base_revision
        .checked_add(u64::from(changed))
        .context("Scheduling revision overflow")?;
    for (key, actual) in &after {
        write_alias(
            tx,
            &proof.scope,
            key,
            revision,
            SchedulingAvailability::Present,
            Some(&actual.content),
            Some(&actual.projection),
            Some((&actual.status, &actual.sort_at, &actual.created_at)),
        )?;
    }
    let material_bytes = material.bytes;
    let material_records = material.records;
    let material_guards = std::mem::take(&mut material.guards);
    drop(material);
    let mut snapshot = loader::load_with_revision(
        tx,
        &proof.scope,
        request,
        &proof.ledger,
        true,
        proof.original_recovery,
        Some(revision),
        Some((material_bytes, material_records)),
        memory,
    )?
    .require_current()?;
    let material_usage=snapshot.material_usage;
    let records = snapshot.resident.selected_install_records();
    let mut guards = proof.guards.clone();
    guards.extend(material_guards);
    guards.append(&mut snapshot._read_material);
    // The newly decoded allocations stay Candidate-observed until the caller's
    // whole receipt and Current publication replace this transaction carrier.
    guards.push(proof.ledger.register_scheduling_view(
        &proof.scope,
        InputMemoryCategory::Candidate,
        records.iter(),
    )?);
    let delta = CommittedSchedulingDelta {
        scope: proof.scope.clone(),
        base_revision: proof.base_revision,
        revision,
        coverage: snapshot.resident.coverage().clone(),
        records,
    };
    Ok(CommittedSchedulingFacts {
        delta,
        _input_roots: snapshot.input_roots,
        requires_prepared_domain: true,
        guards,
        material_usage,
    })
}
