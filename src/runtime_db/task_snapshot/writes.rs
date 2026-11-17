use super::*;
use loader::{current_coverage, current_revision, load_original, persist_coverage};
#[path = "write_delta.rs"]
mod write_delta;

/// No business SQL ran. Reprepare outside the writer, keeping the command frozen.
#[derive(Debug, thiserror::Error)]
#[error("prepared Task write baseline is stale")]
pub(crate) struct TaskWriteBaselineStale {
    pub(crate) scope: InputEvidenceScope,
    pub(crate) request: TaskDomainLoadRequest,
    /// Storage-visible classification: lets the transaction logger tell the
    /// reprepare control flow apart from real write failures without
    /// depending on this runtime type.
    #[source]
    reader_reprepare: holon_storage::ReaderReprepareRequested,
    _guard: Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
}

impl TaskWriteBaselineStale {
    pub(in crate::runtime_db) fn new(
        scope: &InputEvidenceScope,
        request: &TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        use crate::runtime::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        let guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(&(
                    scope,
                    &request.query,
                    &request.capture.required_keys,
                    &request.capture.recovery_source_ids,
                ))? + 256,
            },
        )?;
        Ok(Self {
            scope: scope.clone(),
            request: request.clone(),
            reader_reprepare: holon_storage::ReaderReprepareRequested::new("task"),
            _guard: guard,
        })
    }
}

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
/// The existing published selection remains complete when only one unsettled
/// Result's deferral fields change. The exact SQL body still supplies the ACK.
pub(in crate::runtime_db) struct PreparedTaskDefer {
    coverage: TaskCoverage,
    key: TaskKey,
}

impl PreparedTaskDefer {
    pub(in crate::runtime_db) fn prepare(
        tx: &Transaction<'_>,
        proof: &TaskWriteProof,
        keys: &BTreeSet<TaskKey>,
        base: &ResidentTaskDomain,
        request: &TaskDomainLoadRequest,
        material: &mut read_preparation::TaskReadMaterial<'_>,
    ) -> Result<Option<Self>> {
        if proof.original_recovery
            || base.scope() != &proof.scope
            || base.coverage().revision != proof.base_revision
            || request.query != Default::default()
            || request.recovery_at.is_some()
            || !request.capture.recovery_source_ids.is_empty()
        {
            return Ok(None);
        }
        // The selected Result also carries its original Task as an observation.
        let mut results = keys
            .iter()
            .filter(|key| key.family == TaskFamily::ResultSettlement);
        let Some(key) = results.next() else {
            return Ok(None);
        };
        if results.next().is_some() {
            return Ok(None);
        }
        let Some(before) = proof.before.get(key).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let CanonicalTaskFact::ResultSettlement(result) = &before.fact else {
            return Ok(None);
        };
        let published = base.coverage();
        if result.state
            == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
            || result.recovery_paused_at.is_some()
            || !published.active_complete
            || !published.unsettled_complete
            || !published.unsettled_result_ids.contains(&key.id)
            || published.terminal.limit != request.capture.terminal_limit
            || published.required_keys != request.capture.required_keys
            || published.query.birth_after.is_some()
            || !published.query.reply_task_ids.is_empty()
            || !published.query.parent_turn_tasks.is_empty()
            || published.recovery_at.is_some()
        {
            return Ok(None);
        }
        let Some(coverage) = material.read_current_metadata(tx)? else {
            return Ok(None);
        };
        if &coverage != published {
            return Ok(None);
        }
        let seed = binding(tx, &proof.scope)?;
        let dependency = result_relation::observe_result_dependency_tx(
            tx,
            &proof.scope,
            seed.as_deref(),
            before,
            request,
            material,
        )?;
        let metadata = loader::result_metadata(tx, &proof.scope, result, request, 0, material)?;
        if !matches!(
            dependency.relation,
            crate::runtime::resident_task::TaskResultDependencyRelation::QualifiedCurrent { .. }
        ) {
            return Ok(None);
        }
        if coverage.result_dependencies.get(&key.id) != Some(&dependency)
            || coverage.result_metadata.get(&key.id) != Some(&metadata)
        {
            return Ok(None);
        }
        // A previously absent deadline and two decimal revision increases are
        // the only possible growth. The read already admitted the base JSON.
        material.charge_ids(128, 0)?;
        Ok(Some(Self {
            coverage,
            key: key.clone(),
        }))
    }

    pub(in crate::runtime_db) fn finish(
        mut self,
        tx: &Transaction<'_>,
        proof: &TaskWriteProof,
        actual: &BTreeSet<TaskKey>,
    ) -> Result<Option<CommittedTaskDelta>> {
        ensure!(
            actual.iter().all(|key| key == &self.key),
            "Defer ACK escaped its original Result"
        );
        if actual.is_empty() {
            return Ok(None);
        }
        let before = proof.before[&self.key]
            .as_ref()
            .context("Defer omitted its original Result")?;
        let after = stored_on(tx, &proof.scope.agent_id, &self.key)?
            .context("Defer actual Result is missing")?;
        if before == &after {
            return Ok(None);
        }
        let (
            CanonicalTaskFact::ResultSettlement(before),
            CanonicalTaskFact::ResultSettlement(result),
        ) = (&before.fact, &after.fact)
        else {
            anyhow::bail!("Defer ACK has another Task family");
        };
        let mut expected = before.clone();
        expected.deferred_reason.clone_from(&result.deferred_reason);
        expected.deferred_at = result.deferred_at;
        expected.next_recheck_at = result.next_recheck_at;
        expected.updated_at = result.updated_at;
        ensure!(
            &expected == result,
            "Defer changed fields outside its original operation"
        );
        let revision = proof
            .base_revision
            .checked_add(1)
            .context("Task revision overflow")?;
        let fact = after.qualified(&proof.scope, revision)?;
        upsert_alias(
            tx,
            &proof.scope,
            &self.key,
            &Alias {
                revision,
                availability: TaskAvailability::Present,
                digest: Some(fact.locator.content_digest.clone()),
                projection_digest: Some(fact.locator.projection_digest.clone()),
            },
        )?;
        self.coverage.revision = revision;
        self.coverage
            .result_metadata
            .get_mut(&self.key.id)
            .context("Defer coverage omitted Result metadata")?
            .next_recheck_at = result.next_recheck_at;
        self.coverage
            .result_dependencies
            .get_mut(&self.key.id)
            .context("Defer coverage omitted Result dependency")?
            .result = fact.locator.clone();
        persist_coverage(tx, &proof.scope, &self.coverage)?;
        Ok(Some(CommittedTaskDelta {
            scope: proof.scope.clone(),
            base_revision: proof.base_revision,
            prepared_base: None,
            revision,
            records: vec![fact],
            stop_records: Vec::new(),
            removals: BTreeMap::new(),
            coverage: self.coverage,
        }))
    }
}

pub(crate) fn prepare_task_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskWriteProof> {
    prepare(tx, scope, keys, false)
}
/// Same-incarnation cleanup only; every target already exists and is qualified.
pub(crate) fn prepare_original_task_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskWriteProof> {
    prepare(tx, scope, keys, true)
}
fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
    original_recovery: bool,
) -> Result<TaskWriteProof> {
    ensure!(
        scope_facts(tx, scope, original_recovery)?.is_some(),
        "Task write scope retired"
    );
    let seed = binding(tx, scope)?;
    lengths(tx, keys, None)?;
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
        base_revision: current_revision(tx, scope)?.unwrap_or(1),
        before,
        original_recovery,
    })
}
#[cfg(test)]
pub(crate) fn preflight_task_write_snapshot_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
) -> Result<TaskBootstrapSnapshot> {
    ensure!(
        current_revision(tx, &proof.scope)?.unwrap_or(1) == proof.base_revision,
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
        current_revision(tx, &proof.scope)?.unwrap_or(1) == proof.base_revision,
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
    match memory_budget {
        Some(memory) => {
            ensure!(
                current_revision(tx, &proof.scope)?.unwrap_or(1) == proof.base_revision,
                "Task proof base changed"
            );
            loader::load_with_memory_budget(
                tx,
                &proof.scope,
                request,
                false,
                proof.original_recovery,
                true,
                Some(budget),
                Some(memory),
            )
        }
        None => preflight_task_write_snapshot_with_budget_tx(tx, proof, request, budget),
    }
}

/// Dynamic short re-verification for a prepared reader baseline. Adoption
/// re-checks only bounded semantic facts — scope identity, the revision
/// fence, persisted coverage equality, and capture satisfaction — and never
/// decodes a Task body. Ineligible or cold inputs return None so the caller
/// keeps its explicit full preflight.
pub(crate) fn adopt_task_write_baseline_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
    resident: &ResidentTaskDomain,
    coverage_json: &str,
) -> Result<Option<TaskCurrentSnapshot>> {
    if resident.scope() != &proof.scope {
        return Ok(None);
    }
    if request.recovery_at.is_some() || !request.capture.recovery_source_ids.is_empty() {
        return Ok(None);
    }
    let published = resident.coverage();
    if !published.active_complete || !published.unsettled_complete {
        return Ok(None);
    }
    if let Err(error) = resident
        .capture_query(&request.query)
        .and_then(|_| resident.capture(&request.capture))
    {
        match error.downcast_ref::<TaskReadError>() {
            // A domain read error is plain ineligibility, not writer failure.
            Some(_) => return Ok(None),
            None => return Err(error),
        }
    }
    if published.revision != proof.base_revision {
        return Ok(None);
    }
    let same_coverage = tx
        .query_row(
            "SELECT coverage_json=?3 FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![proof.scope.agent_id, proof.scope.identity_incarnation, coverage_json],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    if !same_coverage {
        return Ok(None);
    }
    let Some(facts) = scope_facts(tx, &proof.scope, proof.original_recovery)? else {
        return Ok(None);
    };
    // Roots and the qualified namespace are re-observed from the actual write
    // set by the incremental finalizer; an adopted baseline carries neither.
    Ok(Some(TaskCurrentSnapshot {
        facts,
        // A same-revision empty delta preserves body Arcs and the domain's
        // limits/budget contract while allocating an independently owned view.
        resident: resident.with_committed(&CommittedTaskDelta {
            scope: proof.scope.clone(),
            base_revision: published.revision,
            prepared_base: None,
            revision: published.revision,
            records: Vec::new(),
            stop_records: Vec::new(),
            removals: BTreeMap::new(),
            coverage: published.clone(),
        })?,
        input_roots: BTreeSet::new(),
        #[cfg(test)]
        turnless_parent_turns: BTreeSet::new(),
        work_roots: BTreeSet::new(),
    }))
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
        None,
    )
}
/// Test-only original writer: production stop observation now registers the
/// actual write delta instead of re-loading the full domain.
#[cfg(test)]
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
    register_task_writes_inner(
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
        None,
    )
}

impl TaskWriteProof {
    /// The pre-SQL selection is observation material, never write authority.
    pub(in crate::runtime_db) fn register_incremental_tx(
        &self,
        tx: &Transaction<'_>,
        baseline: TaskCurrentSnapshot,
        actual: &BTreeSet<TaskKey>,
        state: &AgentState,
        request: &TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        material: &mut read_preparation::TaskReadMaterial<'_>,
        inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    ) -> Result<CommittedTaskFacts> {
        material.validate_original_allowance(self.scope(), request, ledger)?;
        if let Some(inserted) = inserted {
            inserted.validate_scope_tx(tx, self.scope())?;
        }
        register_task_writes_inner(
            tx,
            self,
            actual,
            state,
            request,
            Some(ledger),
            None,
            inserted,
            None,
            Some(material),
            Some(baseline),
        )
    }
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
    baseline: Option<TaskCurrentSnapshot>,
) -> Result<CommittedTaskFacts> {
    let mut finalize_stage = crate::diagnostics::attribution::TASK_WRITE_DELTA_FINALIZE.start();
    finalize_stage.rows(actual_final_keys.len());
    let facts = scope_facts(tx, &proof.scope, proof.original_recovery)?
        .context("Task finalizer scope retired")?;
    ensure!(
        facts.state.as_deref() == Some(final_state) && final_state.id == proof.scope.agent_id,
        "Task finalizer requires actual final canonical State"
    );
    let previous = current_coverage(tx, &proof.scope)?;
    ensure!(
        previous.as_ref().map_or(1, |c| c.revision) == proof.base_revision,
        "Task finalizer base changed"
    );
    ensure!(
        actual_final_keys
            .iter()
            .all(|k| proof.before.contains_key(k)),
        "Task actual writes exceed pre-SQL proof"
    );
    lengths(tx, actual_final_keys, Some(request))?;
    let mut changed = false;
    let mut removals = BTreeMap::new();
    let mut replacements = BTreeMap::new();
    let revision = proof
        .base_revision
        .checked_add(1)
        .context("Task revision overflow")?;
    let seed = binding(tx, &proof.scope)?;
    for key in actual_final_keys {
        let before = proof.before.get(key).expect("proof keys checked");
        if baseline.is_some() {
            // Replacements bypass the selected-body loader, so admit their
            // original completion material before reading canonical bodies.
            material
                .as_deref_mut()
                .context("incremental Task finalizer requires original material")?
                .charge_record(
                    key,
                    checkpoint_material_size_on(tx, key)?.unwrap_or_default(),
                )?;
        }
        let after = stored_on(tx, &proof.scope.agent_id, key)?;
        match after {
            Some(after) => {
                let different = before.as_ref() != Some(&after);
                changed |= different;
                let record_revision = if different {
                    revision
                } else {
                    qualification::validate_member(tx, &proof.scope, seed.as_deref(), key, &after)?
                };
                upsert_alias(
                    tx,
                    &proof.scope,
                    key,
                    &Alias {
                        revision: record_revision,
                        availability: TaskAvailability::Present,
                        digest: Some(after.digest()),
                        projection_digest: Some(after.projection_digest.clone()),
                    },
                )?;
                if let CanonicalTaskFact::Task(task) = &after.fact {
                    query_relations::register_actual_task_tx(
                        tx,
                        &proof.scope,
                        task,
                        &after.digest(),
                        &after.projection_digest,
                        before.is_none(),
                    )?;
                }
                if baseline.is_some() {
                    replacements.insert(
                        key.clone(),
                        Some(after.qualified(&proof.scope, record_revision)?),
                    );
                }
            }
            None => {
                ensure!(
                    before.is_some(),
                    "Task removal has no original canonical body"
                );
                changed = true;
                upsert_alias(
                    tx,
                    &proof.scope,
                    key,
                    &Alias {
                        revision,
                        availability: TaskAvailability::Pruned,
                        digest: None,
                        projection_digest: None,
                    },
                )?;
                removals.insert(key.clone(), TaskAvailability::Pruned);
                if baseline.is_some() {
                    replacements.insert(key.clone(), None);
                }
            }
        }
    }
    // Build final complete coverage once, after all original business/State writes.
    // Installing an empty base before load also handles coverage=None + fixed updates.
    if previous.is_none() {
        persist_coverage(tx, &proof.scope, &empty_coverage(proof.base_revision))?;
    }
    if changed {
        let mut base = previous
            .clone()
            .unwrap_or_else(|| empty_coverage(proof.base_revision));
        base.revision = revision;
        persist_coverage(tx, &proof.scope, &base)?;
    }
    let snapshot = if let Some(baseline) = baseline {
        write_delta::finish_snapshot_tx(
            tx,
            proof,
            baseline,
            replacements,
            request,
            material.context("incremental Task finalizer requires original material")?,
            budget,
            inserted,
            result,
        )?
    } else if let Some(material) = material {
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
    let mut coverage = snapshot.resident.coverage().clone();
    // A row leaving the bounded selection is removed by pure trim. Keep only
    // removal observations in this exact prepared view; tombstones remain durable.
    removals.retain(|key, availability| coverage.observations.get(key) == Some(availability));
    if !changed && previous.as_ref().is_some_and(|p| p != &coverage) {
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
    if previous.is_none() && !changed {
        coverage.revision = revision;
        persist_coverage(tx, &proof.scope, &coverage)?;
    }
    let final_revision = coverage.revision;
    let stop_records = if final_revision == proof.base_revision {
        Vec::new()
    } else {
        snapshot.resident.selected_install_stop_records()
    };
    let resident_records = if final_revision == proof.base_revision {
        Vec::new()
    } else {
        snapshot.resident.selected_install_records()
    };
    Ok(CommittedTaskFacts {
        delta: CommittedTaskDelta {
            scope: proof.scope.clone(),
            base_revision: proof.base_revision,
            prepared_base: None,
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
        recovery_sources: BTreeMap::new(),
        recovery_at: None,
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
