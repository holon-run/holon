use super::*;
use loader::{current_coverage, load_original, persist_coverage};
/// Freezes only this original transaction's finite Task/result footprint.
/// Qualification is not a substitute for Task/rejoin/State business OCC.
pub(crate) struct TaskWriteProof {
    scope: InputEvidenceScope,
    base_revision: u64,
    before: BTreeMap<TaskKey, Option<StoredTask>>,
    original_recovery: bool,
}
impl TaskWriteProof {
    /// Only the already qualified pre-SQL body. New/absent keys cannot authorize Stop.
    pub(crate) fn task_before(&self, task_id: &str) -> Result<&crate::types::TaskRecord> {
        let key = TaskKey::new(TaskFamily::Task, task_id);
        match self
            .before
            .get(&key)
            .and_then(Option::as_ref)
            .map(|stored| &stored.fact)
        {
            Some(CanonicalTaskFact::Task(task)) => Ok(task),
            _ => anyhow::bail!("Task proof has no original qualified Task"),
        }
    }
    pub(in crate::runtime_db) fn validate_task_before_tx(
        &self,
        tx: &Transaction<'_>,
        id: &str,
    ) -> Result<()> {
        let key = TaskKey::new(TaskFamily::Task, id);
        let before = self
            .before
            .get(&key)
            .and_then(Option::as_ref)
            .context("Task proof has no original qualified Task")?;
        maintenance::require_existing_maintenance_scope_tx(tx, &self.scope)?;
        let seed = binding(tx, &self.scope)?;
        let current =
            stored_on(tx, &self.scope.agent_id, &key)?.context("original Task body missing")?;
        qualification::validate_member(tx, &self.scope, seed.as_deref(), &key, &current)?;
        ensure!(
            &current == before,
            "original Task proof changed before Message derivation"
        );
        Ok(())
    }
    /// Borrow the W1-owned pre-SQL material; no second Task body acquisition.
    pub(crate) fn validate_original_producer_before(
        &self,
        scope: &InputEvidenceScope,
        id: &str,
        content: &str,
        projection: &str,
    ) -> Result<()> {
        let row = self
            .before
            .get(&TaskKey::new(TaskFamily::Task, id))
            .and_then(Option::as_ref)
            .context("Command proof lacks qualified original Task")?;
        ensure!(
            &self.scope == scope && row.digest() == content && row.projection_digest == projection,
            "Command original before-version changed"
        );
        Ok(())
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
}
#[derive(Debug)]
pub(crate) struct CommittedTaskFacts {
    pub delta: CommittedTaskDelta,
    pub _input_roots: BTreeSet<InputEvidenceKey>,
    pub _work_roots: BTreeSet<String>,
}
pub(crate) fn prepare_task_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskWriteProof> {
    prepare(tx, scope, keys, false, None)
}
/// Same-incarnation cleanup only; every target already exists and is qualified.
pub(crate) fn prepare_original_task_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskWriteProof> {
    prepare(tx, scope, keys, true, None)
}
fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
    original_recovery: bool,
    mut material: Option<&mut read_preparation::TaskReadMaterial<'_>>,
) -> Result<TaskWriteProof> {
    let facts = if let Some(material) = material.as_deref_mut() {
        material.scope_facts_tx(tx, scope, original_recovery)?
    } else {
        scope_facts(tx, scope, original_recovery)?
    };
    ensure!(facts.is_some(), "Task write scope retired");
    if let Some(material) = material.as_deref_mut() {
        material.admit_original_metadata_tx(tx, scope, keys)?;
    } else {
        lengths(tx, keys, None)?;
    }
    let seed = binding(tx, scope)?;
    let mut before = BTreeMap::new();
    for key in keys {
        ensure!(!key.id.is_empty(), "empty Task write target");
        let a = alias(tx, scope, key)?;
        ensure!(
            !a.as_ref()
                .is_some_and(|x| x.availability == TaskAvailability::Pruned),
            "Task writer cannot revive pruned ID"
        );
        let stored = stored_on(tx, &scope.agent_id, key)?;
        if let Some(item) = &stored {
            qualification::validate_member(tx, scope, seed.as_deref(), key, item)?;
        } else {
            ensure!(
                !qualification::known_anywhere(tx, key)?,
                "known Task target has no canonical body"
            );
        }
        if original_recovery {
            ensure!(
                stored.is_some(),
                "original Task cleanup cannot admit a new key"
            );
        }
        before.insert(key.clone(), stored);
    }
    Ok(TaskWriteProof {
        scope: scope.clone(),
        base_revision: if let Some(material) = material {
            material.original_revision_tx(tx, scope)?
        } else {
            current_coverage(tx, scope)?.map_or(1, |c| c.revision)
        },
        before,
        original_recovery,
    })
}
/// Only the original W1 proof uses this existing phase; material is not authority.
pub(in crate::runtime_db) fn prepare_original_task_write_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
    original_recovery: bool,
    request: &TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<TaskWriteProof> {
    material.validate_original_allowance(scope, request, ledger)?;
    prepare(tx, scope, keys, original_recovery, Some(material))
}

pub(in crate::runtime_db) fn preflight_original_task_using_material_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<()> {
    material.validate_original_allowance(proof.scope(), request, ledger)?;
    let mark = material.mark_preflight()?;
    ensure!(
        material.original_revision_tx(tx, proof.scope())? == proof.base_revision,
        "Task proof base changed"
    );
    let snapshot = loader::load_selected_using_material(
        tx,
        proof.scope(),
        request,
        false,
        proof.original_recovery,
        true,
        Some(ledger),
        None,
        None,
        material,
    )?
    .require_current()?;
    drop(snapshot);
    material.retire_preflight(mark)
}

#[cfg(test)]
pub(crate) fn preflight_task_write_snapshot_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
) -> Result<TaskBootstrapSnapshot> {
    ensure!(
        current_coverage(tx, &proof.scope)?.map_or(1, |c| c.revision) == proof.base_revision,
        "Task proof base changed"
    );
    load_original(tx, &proof.scope, request, false, proof.original_recovery)
}
/// Budget rejection in the original read-only preflight precedes business SQL.
pub(crate) fn preflight_task_write_snapshot_with_budget_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<TaskBootstrapSnapshot> {
    ensure!(
        current_coverage(tx, &proof.scope)?.map_or(1, |c| c.revision) == proof.base_revision,
        "Task proof base changed"
    );
    loader::load_original_with_budget(
        tx,
        &proof.scope,
        request,
        false,
        proof.original_recovery,
        budget,
    )
}
/// Every selected metadata/body allowance is checked before the loader decodes it.
pub(crate) fn preflight_task_write_snapshot_with_memory_budget_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<TaskBootstrapSnapshot> {
    ensure!(
        current_coverage(tx, &proof.scope)?.map_or(1, |c| c.revision) == proof.base_revision,
        "Task proof base changed"
    );
    match memory_budget {
        Some(memory) => loader::load_with_memory_budget(
            tx,
            &proof.scope,
            request,
            false,
            proof.original_recovery,
            true,
            Some(budget),
            Some(memory),
        ),
        None => preflight_task_write_snapshot_with_budget_tx(tx, proof, request, budget),
    }
}

#[cfg(test)]
pub(crate) fn register_task_writes_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
) -> Result<CommittedTaskFacts> {
    register_task_writes_inner(
        tx,
        proof,
        actual_final_keys,
        final_state,
        request,
        None,
        None,
        None,
        None,
        None,
    )
}
/// Production original writer; guards for proof/selection remain caller-owned.
pub(crate) fn register_task_writes_with_budget_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<CommittedTaskFacts> {
    register_task_writes_inner(
        tx,
        proof,
        actual_final_keys,
        final_state,
        request,
        Some(budget),
        None,
        None,
        None,
        None,
    )
}
/// Original Memory command shares its combined allowance through final readback.
pub(crate) fn register_task_writes_with_memory_budget_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<CommittedTaskFacts> {
    match memory_budget {
        Some(memory) => register_task_writes_inner(
            tx,
            proof,
            actual_final_keys,
            final_state,
            request,
            Some(budget),
            Some(memory),
            None,
            None,
            None,
        ),
        None => register_task_writes_with_budget_tx(
            tx,
            proof,
            actual_final_keys,
            final_state,
            request,
            budget,
        ),
    }
}
pub(crate) fn register_task_writes_with_stop_observation_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    inserted: &crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>,
) -> Result<CommittedTaskFacts> {
    inserted.validate_scope_tx(tx, &proof.scope)?;
    register_task_writes_inner(
        tx,
        proof,
        actual_final_keys,
        final_state,
        request,
        Some(budget),
        memory_budget,
        Some(inserted),
        None,
        None,
    )
}

/// Exact original Result SQL was acknowledged before this final observation;
/// its whole receipt is inserted after the once-finalizer in the same transaction.
pub(crate) fn register_task_writes_with_stop_result_observation_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    result: &crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>,
) -> Result<CommittedTaskFacts> {
    result.validate_presence_tx(tx)?;
    ensure!(
        &result.actual().record().intent.task.scope == proof.scope(),
        "Stop Result observation crossed scope"
    );
    register_task_writes_inner(
        tx,
        proof,
        actual_final_keys,
        final_state,
        request,
        Some(budget),
        memory_budget,
        None,
        Some(result),
        None,
    )
}

/// Child terminal Source and all selected Status companions share one allowance.
/// The original W1 proof remains the sole Task write authority.
pub(in crate::runtime_db) fn register_original_task_writes_using_material_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual: &BTreeSet<TaskKey>,
    state: &AgentState,
    request: &TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<CommittedTaskFacts> {
    material.validate_original_allowance(proof.scope(), request, ledger)?;
    material.with_original_final_readback(|material| register_task_writes_inner(
        tx,
        proof,
        actual,
        state,
        request,
        Some(ledger),
        None,
        None,
        None,
        Some(material),
    ))
}

pub(in crate::runtime_db) fn register_original_task_writes_with_stop_using_material_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual: &BTreeSet<TaskKey>,
    state: &AgentState,
    request: &TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &mut read_preparation::TaskReadMaterial<'_>,
    inserted: &crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>,
) -> Result<CommittedTaskFacts> {
    inserted.validate_scope_tx(tx, proof.scope())?;
    material.validate_original_allowance(proof.scope(), request, ledger)?;
    material.with_original_final_readback(|material| register_task_writes_inner(
        tx,
        proof,
        actual,
        state,
        request,
        Some(ledger),
        None,
        Some(inserted),
        None,
        Some(material),
    ))
}

fn register_task_writes_inner(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    actual_final_keys: &BTreeSet<TaskKey>,
    final_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: Option<&crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
    mut material: Option<&mut read_preparation::TaskReadMaterial<'_>>,
) -> Result<CommittedTaskFacts> {
    let facts = if let Some(material) = material.as_deref_mut() {
        material.scope_facts_tx(tx, &proof.scope, proof.original_recovery)?
    } else {
        scope_facts(tx, &proof.scope, proof.original_recovery)?
    }
    .context("Task finalizer scope retired")?;
    ensure!(
        facts.state.as_ref() == Some(final_state) && final_state.id == proof.scope.agent_id,
        "Task finalizer requires actual final canonical State"
    );
    let mut guarded_previous = if let Some(material) = material.as_deref_mut() {
        Some(material.original_current_metadata_tx(tx)?)
    } else {
        None
    };
    let mut previous = if material.is_none() { current_coverage(tx, &proof.scope)? } else { None };
    let previous_ref = guarded_previous.as_ref().and_then(|read| read.as_ref()).or(previous.as_ref());
    let had_previous = previous_ref.is_some();
    ensure!(
        previous_ref.map_or(1, |c| c.revision) == proof.base_revision,
        "Task finalizer base changed"
    );
    ensure!(
        actual_final_keys
            .iter()
            .all(|k| proof.before.contains_key(k)),
        "Task actual writes exceed pre-SQL proof"
    );
    if let Some(material) = material.as_deref_mut() {
        material.admit_original_metadata_header_tx(tx, &proof.scope, actual_final_keys)?;
        material.admit_original_written_keys(tx, &proof.scope.agent_id, actual_final_keys)?;
    } else {
        lengths(tx, actual_final_keys, Some(request))?;
    }
    let mut changed = false;
    let mut removals = BTreeMap::new();
    let revision = proof
        .base_revision
        .checked_add(1)
        .context("Task revision overflow")?;
    let seed = binding(tx, &proof.scope)?;
    for key in actual_final_keys {
        let before = proof.before.get(key).expect("proof keys checked");
        let write = |after: Option<StoredTask>| -> Result<(bool, bool)> {
            match after {
                Some(after) => {
                    let different = before.as_ref() != Some(&after);
                    let record_revision = if different {
                        revision
                    } else {
                        qualification::validate_member(tx, &proof.scope, seed.as_deref(), key, &after)?
                    };
                    upsert_alias(
                        tx, &proof.scope, key,
                        &Alias { revision: record_revision, availability: TaskAvailability::Present,
                            digest: Some(after.digest()), projection_digest: Some(after.projection_digest.clone()) },
                    )?;
                    if let CanonicalTaskFact::Task(task) = &after.fact {
                        query_relations::register_actual_task_tx(tx, &proof.scope, task,
                            &after.digest(), &after.projection_digest, before.is_none())?;
                    }
                    Ok((different, false))
                }
                None => {
                    ensure!(before.is_some(), "Task removal has no original canonical body");
                    upsert_alias(tx, &proof.scope, key,
                        &Alias { revision, availability: TaskAvailability::Pruned,
                            digest: None, projection_digest: None })?;
                    Ok((true, true))
                }
            }
        };
        let (different, removed) = if let Some(material) = material.as_deref_mut() {
            material.with_original_written_row(tx, &proof.scope.agent_id, key, write)?
        } else {
            write(stored_on(tx, &proof.scope.agent_id, key)?)?
        };
        changed |= different;
        if removed {
            removals.insert(key.clone(), TaskAvailability::Pruned);
        }
    }
    // Build final complete coverage once, after all original business/State writes.
    // Installing an empty base before load also handles coverage=None + fixed updates.
    if !had_previous {
        persist_coverage(tx, &proof.scope, &empty_coverage(proof.base_revision))?;
    }
    if changed {
        if let Some(read) = guarded_previous.as_mut() {
            if let Some(base) = read.as_mut() {
                base.revision = revision;
                persist_coverage(tx, &proof.scope, base)?;
            } else {
                let mut base = empty_coverage(proof.base_revision);
                base.revision = revision;
                persist_coverage(tx, &proof.scope, &base)?;
            }
        } else {
            let mut base = current_coverage(tx, &proof.scope)?.expect("base initialized");
            base.revision = revision;
            persist_coverage(tx, &proof.scope, &base)?;
        }
        if let (Some(read), Some(material)) = (guarded_previous.take(), material.as_deref_mut()) {
            read.retire(material)?;
        }
        previous = None;
    }
    let snapshot = if let Some(material) = material.as_deref_mut() {
        loader::load_selected_using_material(
            tx,
            &proof.scope,
            request,
            true,
            proof.original_recovery,
            false,
            budget,
            inserted,
            result,
            material,
        )?
    } else {
        match budget {
            Some(budget) => loader::load_with_stop_result_observation(
                tx,
                &proof.scope,
                request,
                true,
                proof.original_recovery,
                false,
                Some(budget),
                memory_budget,
                inserted,
                result,
            )?,
            None => load_original(tx, &proof.scope, request, true, proof.original_recovery)?,
        }
    }
    .require_current()?;
    // Complete coverage/maps move out of the validated domain without a copy.
    let (mut coverage, resident_records, stop_records) = snapshot.resident.into_original_install_parts();
    // A row leaving the bounded selection is removed by pure trim. Keep only
    // removal observations in this exact prepared view; tombstones remain durable.
    removals.retain(|key, availability| coverage.observations.get(key) == Some(availability));
    let previous_ref = guarded_previous.as_ref().and_then(|read| read.as_ref()).or(previous.as_ref());
    if !changed && previous_ref.is_some_and(|p| p != &coverage) {
        changed = true;
        coverage.revision = revision;
        persist_coverage(tx, &proof.scope, &coverage)?;
    }
    let final_revision = if changed {
        revision
    } else {
        proof.base_revision
    };
    ensure!(
        coverage.revision == final_revision,
        "Task final coverage revision mismatch"
    );
    // No-op still carries current exact metadata; initial preparation is a real advance.
    if !had_previous && !changed {
        coverage.revision = revision;
        persist_coverage(tx, &proof.scope, &coverage)?;
    }
    persist_coverage(tx, &proof.scope, &coverage)?;
    let final_revision = coverage.revision;
    if let (Some(read), Some(material)) = (guarded_previous.take(), material.as_deref_mut()) {
        read.retire(material)?;
    }
    let (resident_records, stop_records) = if final_revision == proof.base_revision {
        (Vec::new(), Vec::new())
    } else {
        if let Some(material) = material.as_deref_mut() {
            let slots = resident_records.len().checked_add(stop_records.len())
                .and_then(|count| count.checked_add(2)).context("Task install Vec slots overflow")?;
            material.charge_writer_material(0, slots, 1)?;
        }
        (resident_records.into_values().collect(), stop_records.into_values().collect())
    };
    Ok(CommittedTaskFacts {
        delta: CommittedTaskDelta {
            scope: proof.scope.clone(),
            base_revision: proof.base_revision,
            revision: final_revision,
            records: resident_records,
            stop_records,
            removals,
            coverage,
        },
        _input_roots: snapshot.input_roots,
        _work_roots: snapshot.work_roots,
    })
}
pub(super) fn empty_coverage(revision: u64) -> TaskCoverage {
    TaskCoverage {
        revision,
        stop: TaskStopCoverage::default(),
        output: TaskOutputCoverage::default(),
        status: Default::default(),
        query: Default::default(),
        running: Default::default(),
        active_complete: false,
        unsettled_complete: false,
        active_task_ids: BTreeSet::new(),
        unsettled_result_ids: BTreeSet::new(),
        task_metadata: BTreeMap::new(),
        result_metadata: BTreeMap::new(),
        result_dependencies: BTreeMap::new(),
        task_result_ids: BTreeMap::new(),
        activation_result_ids: BTreeMap::new(),
        reply_result_ids: BTreeMap::new(),
        required_keys: BTreeSet::new(),
        observations: BTreeMap::new(),
        terminal: TaskHistoryCoverage {
            limit: 0,
            keys: Vec::new(),
            is_exhaustive: false,
        },
    }
}

impl TaskWriteProof {
    /// Original recovery policy is private to this proof; a stale owned proof
    /// cannot grant a fresh transaction authority over a successor identity.
    pub(crate) fn validate_task_stop_scope_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        super::maintenance::require_existing_maintenance_scope_tx(tx, &self.scope)?;
        if !self.original_recovery {
            let active: bool = tx.query_row(
                "SELECT status='active' FROM agent_identities WHERE agent_id=?1",
                [&self.scope.agent_id],
                |row| row.get(0),
            )?;
            ensure!(active, "new TaskStop requires the original Active scope");
        }
        Ok(())
    }
}

// Only the finite derived-Result capability may mint a new original-cleanup key.
pub(super) fn original_result_cleanup_proof(
    scope: InputEvidenceScope,
    base_revision: u64,
    before: BTreeMap<TaskKey, Option<StoredTask>>,
) -> TaskWriteProof {
    TaskWriteProof {
        scope,
        base_revision,
        before,
        original_recovery: true,
    }
}

#[cfg(test)]
#[path = "readback_lifetime_tests.rs"]
mod final_readback_lifetime_tests;
