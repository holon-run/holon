//! Original-transaction finalization reuses qualified pre-SQL bodies. Only the
//! actual write set and newly selected closure members acquire canonical bodies.
use super::*;
use crate::runtime_db::task_result_settlement::TaskResultSettlementState;

#[cfg(all(test, holon_lib_test_shard = "b"))]
#[path = "write_delta_tests.rs"]
mod tests;
use qualification::{eligible_digest, manifests};

pub(super) fn finish_snapshot_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    baseline: TaskCurrentSnapshot,
    replacements: BTreeMap<TaskKey, Option<Arc<QualifiedTaskFact>>>,
    request: &TaskDomainLoadRequest,
    material: &mut read_preparation::TaskReadMaterial<'_>,
    budget: Option<&crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
) -> Result<TaskBootstrapSnapshot> {
    let scope = proof.scope();
    ensure!(
        baseline.resident.scope() == scope
            && baseline.resident.coverage().revision == proof.base_revision,
        "Task incremental baseline crossed scope/revision"
    );
    let old = baseline.resident.coverage();
    ensure!(
        old.active_complete && old.unsettled_complete,
        "Task incremental baseline lacks complete active/unsettled selection"
    );
    ensure!(
        request.capture.terminal_limit <= TASK_TERMINAL_WINDOW,
        "Task terminal request exceeds fixed maximum"
    );
    let facts = scope_facts(tx, scope, proof.original_recovery)?
        .context("Task incremental finalizer scope retired")?;
    let seed = binding(tx, scope)?;
    query_sources::initialize_fresh_source_inventory_tx(tx, scope)?;
    material.charge_checkpoint_metadata(tx, seed.as_deref())?;
    let fixed = manifests(tx, seed.as_deref())?;
    query_relations::initialize_tx(tx, scope, &fixed)?;

    // These complete sets change only through this transaction's declared W1
    // writes. No canonical active/unsettled namespace is decoded a second time.
    material.charge_ids(
        read_preparation::encoded_metadata_bytes(&(
            &old.active_task_ids,
            &old.unsettled_result_ids,
            &old.task_result_ids,
        ))?,
        old.active_task_ids.len()
            + old.unsettled_result_ids.len()
            + old
                .task_result_ids
                .values()
                .map(BTreeSet::len)
                .sum::<usize>()
            + old.task_result_ids.len(),
    )?;
    let mut active = old.active_task_ids.clone();
    let mut unsettled = old.unsettled_result_ids.clone();
    let mut reverse = old.task_result_ids.clone();
    for (key, record) in &replacements {
        match key.family {
            TaskFamily::Task => {
                active.remove(&key.id);
                if record.as_ref().is_some_and(|record| {
                    matches!(&record.fact, CanonicalTaskFact::Task(task)
                        if matches!(task.status, crate::types::TaskStatus::Queued
                            | crate::types::TaskStatus::Running
                            | crate::types::TaskStatus::Cancelling))
                }) {
                    active.insert(key.id.clone());
                }
            }
            TaskFamily::ResultSettlement => {
                unsettled.remove(&key.id);
                for ids in reverse.values_mut() {
                    ids.remove(&key.id);
                }
                if let Some(record) = record {
                    let CanonicalTaskFact::ResultSettlement(result) = &record.fact else {
                        anyhow::bail!("Result write has another canonical family")
                    };
                    if result.state != TaskResultSettlementState::Settled {
                        unsettled.insert(key.id.clone());
                    }
                    // An unselected Task has no complete reverse set yet.
                    if let Some(ids) = reverse.get_mut(&result.task_id) {
                        ids.insert(key.id.clone());
                    }
                }
            }
        }
    }
    // Reordering/removal can expose one previously unselected terminal row.
    // Preserve the original qualified namespace and its +1 exhaustive proof.
    let mut terminal = loader::namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        TaskFamily::Task,
        "c.status NOT IN ('queued','running','cancelling')",
        Some(request.capture.terminal_limit + 1),
        request,
        None,
        material,
    )?;
    let exhaustive = terminal.len() <= request.capture.terminal_limit
        && fixed
            .iter()
            .filter_map(|m| m.terminal.as_ref())
            .all(|w| w.is_exhaustive);
    terminal.truncate(request.capture.terminal_limit);
    let query = query_relations::observe_tx(tx, scope, &request.query, material)?;
    let mut keys = request.capture.required_keys.clone();
    keys.extend(
        query
            .all_selected_task_ids()
            .map(|id| TaskKey::new(TaskFamily::Task, id)),
    );
    keys.extend(active.iter().map(|id| TaskKey::new(TaskFamily::Task, id)));
    keys.extend(
        unsettled
            .iter()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
    );
    keys.extend(terminal.iter().map(|id| TaskKey::new(TaskFamily::Task, id)));
    material.charge_ids(keys.iter().map(|key| key.id.len() + 128).sum(), keys.len())?;
    let mut available: BTreeMap<_, _> = baseline
        .resident
        .selected_install_records()
        .into_iter()
        .map(|record| (record.locator.key.clone(), record))
        .collect();
    for (key, record) in &replacements {
        available.remove(key);
        if let Some(record) = record {
            available.insert(key.clone(), record.clone());
        }
    }
    let mut selected: BTreeMap<TaskKey, Arc<QualifiedTaskFact>> = BTreeMap::new();
    let mut observations = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    let mut pending: Vec<_> = keys.iter().cloned().collect();
    let mut body_bytes = 0usize;
    while let Some(key) = pending.pop() {
        if observations.contains_key(&key) {
            continue;
        }
        let record = match available.remove(&key) {
            Some(record) => Some(record),
            None => {
                let size = checkpoint_material_size_on(tx, &key)?.unwrap_or_default();
                material.charge_record(&key, size)?;
                match stored_on(tx, &scope.agent_id, &key)? {
                    Some(stored) => {
                        let revision = qualification::validate_member(
                            tx,
                            scope,
                            seed.as_deref(),
                            &key,
                            &stored,
                        )?;
                        let record = stored.qualified(scope, revision)?;
                        upsert_alias(
                            tx,
                            scope,
                            &key,
                            &Alias {
                                revision,
                                availability: TaskAvailability::Present,
                                digest: Some(record.locator.content_digest.clone()),
                                projection_digest: Some(record.locator.projection_digest.clone()),
                            },
                        )?;
                        Some(record)
                    }
                    None => None,
                }
            }
        };
        let Some(record) = record else {
            let availability = match alias(tx, scope, &key)? {
                Some(alias) => {
                    ensure!(
                        alias.availability != TaskAvailability::Present,
                        "qualified Task canonical body missing"
                    );
                    alias.availability
                }
                None => {
                    ensure!(
                        !qualification::known_anywhere(tx, &key)?
                            && eligible_digest(tx, seed.as_deref(), &key)?.is_none(),
                        "known Task body missing"
                    );
                    TaskAvailability::Absent
                }
            };
            observations.insert(key, availability);
            continue;
        };
        record.validate(scope)?;
        body_bytes = body_bytes
            .checked_add(record.resident_bytes)
            .context("Task incremental body bytes overflow")?;
        observe_material(keys.len(), body_bytes, Some(request))?;
        let mut related = Vec::new();
        match &record.fact {
            CanonicalTaskFact::Task(_) => {
                // Invalid Results also create partial reverse entries for
                // unselected Tasks. Only a selected baseline Task proves closure.
                let ids = match reverse
                    .get(&key.id)
                    .filter(|_| old.task_metadata.contains_key(&key.id))
                {
                    Some(ids) => ids.iter().cloned().collect(),
                    None => loader::namespace_ids(
                        tx,
                        scope,
                        seed.as_deref(),
                        TaskFamily::ResultSettlement,
                        &format!("c.task_id='{}'", key.id.replace('\'', "''")),
                        None,
                        request,
                        fixed
                            .iter()
                            .rev()
                            .find_map(|checkpoint| checkpoint.query_inventory.as_ref()),
                        material,
                    )?,
                };
                related.extend(
                    ids.into_iter()
                        .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
                );
            }
            CanonicalTaskFact::ResultSettlement(result) => {
                let task_key = TaskKey::new(TaskFamily::Task, &result.task_id);
                // Unchanged dependencies retain their exact qualified locators.
                // Source authority is still checked in the final metadata phase.
                let dependency = match old.result_dependencies.get(&key.id) {
                    Some(dependency)
                        if !replacements.contains_key(&key)
                            && !replacements.contains_key(&task_key)
                            && matches!(
                                dependency.relation,
                                TaskResultDependencyRelation::QualifiedCurrent { .. }
                            ) =>
                    {
                        dependency.clone()
                    }
                    _ => {
                        let (relation, _, _) = result_relation::observe_task_predicate_tx(
                            tx,
                            scope,
                            seed.as_deref(),
                            result,
                            available
                                .get(&task_key)
                                .or_else(|| selected.get(&task_key))
                                .map(|task| &task.locator),
                            request,
                            material,
                        )?;
                        TaskResultDependencyObservation {
                            result: record.locator.clone(),
                            relation,
                        }
                    }
                };
                if let TaskResultDependencyRelation::QualifiedCurrent { task } =
                    &dependency.relation
                {
                    related.push(task.key.clone());
                }
                dependencies.insert(key.id.clone(), dependency);
            }
        }
        for related in related {
            if keys.insert(related.clone()) {
                material.charge_ids(related.id.len() + 128, 1)?;
                pending.push(related);
            }
        }
        observations.insert(key.clone(), TaskAvailability::Present);
        selected.insert(key, record);
    }
    let mut coverage = empty_coverage(current_revision(tx, scope)?.unwrap_or(proof.base_revision));
    coverage.active_complete = true;
    coverage.unsettled_complete = true;
    coverage.active_task_ids = active;
    coverage.unsettled_result_ids = unsettled;
    coverage.query = query;
    coverage.required_keys = request.capture.required_keys.clone();
    coverage.observations = observations;
    coverage.result_dependencies = dependencies;
    coverage.terminal = TaskHistoryCoverage {
        limit: request.capture.terminal_limit,
        keys: terminal
            .into_iter()
            .map(|id| TaskKey::new(TaskFamily::Task, id))
            .collect(),
        is_exhaustive: exhaustive,
    };
    let records: Vec<_> = selected.into_values().collect();
    let (input_roots, work_roots) = observe_metadata_tx(
        tx,
        scope,
        request,
        &records,
        body_bytes,
        &mut coverage,
        material,
    )?;
    finish_companions_tx(
        tx,
        proof,
        request,
        &fixed,
        records,
        coverage,
        facts,
        input_roots,
        work_roots,
        material,
        budget,
        inserted,
        result,
    )
}

fn observe_metadata_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    records: &[Arc<QualifiedTaskFact>],
    body_bytes: usize,
    coverage: &mut TaskCoverage,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<(BTreeSet<InputEvidenceKey>, BTreeSet<String>)> {
    let mut input_roots = BTreeSet::new();
    let mut work_roots = BTreeSet::new();
    for record in records {
        match &record.fact {
            CanonicalTaskFact::Task(task) => {
                if task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("command_creation_id"))
                    .is_some()
                {
                    task.rejoin_fence().map_err(anyhow::Error::msg)?;
                }
                coverage
                    .task_metadata
                    .insert(task.id.clone(), TaskMetadata::from_record(task));
                coverage.task_result_ids.entry(task.id.clone()).or_default();
                if let Some(id) = &task.work_item_id {
                    work_roots.insert(id.clone());
                }
                if let Some(id) = &task.parent_message_id {
                    input_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
                }
                if let Ok(fence) = task.rejoin_fence() {
                    crate::runtime_db::command_task_creation::validate_source_tx(
                        tx,
                        scope,
                        &task.id,
                        fence.generation,
                        &fence.source,
                        task.work_item_id.as_deref(),
                        Some(task),
                        |bytes, records| material.charge(bytes, records, 8, false),
                    )?;
                    if let Some(parent) = fence.source.parent_turn_id() {
                        input_roots
                            .insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, parent));
                    }
                }
            }
            CanonicalTaskFact::ResultSettlement(result) => {
                // Activation and Message relations can change without rewriting
                // this Result, so they remain original-transaction observations.
                crate::runtime_db::command_task_creation::validate_source_tx(
                    tx,
                    scope,
                    &result.task_id,
                    result.rejoin_generation,
                    &result.source,
                    result.work_item_id.as_deref(),
                    None,
                    |bytes, records| material.charge(bytes, records, 8, false),
                )?;
                let metadata =
                    loader::result_metadata(tx, scope, result, request, body_bytes, material)?;
                coverage
                    .task_result_ids
                    .entry(result.task_id.clone())
                    .or_default()
                    .insert(result.result_identity.clone());
                if result.state == TaskResultSettlementState::CallerAdmitted {
                    let id = result
                        .activation_id
                        .as_ref()
                        .context("admitted Task result has no activation")?;
                    coverage
                        .activation_result_ids
                        .entry(id.clone())
                        .or_default()
                        .insert(result.result_identity.clone());
                }
                if let Some(id) = &metadata.reply_message_id {
                    coverage
                        .reply_result_ids
                        .entry(id.clone())
                        .or_default()
                        .insert(result.result_identity.clone());
                    input_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
                }
                coverage
                    .result_metadata
                    .insert(result.result_identity.clone(), metadata);
                if result.state != TaskResultSettlementState::Settled {
                    input_roots.insert(InputEvidenceKey::new(
                        InputEvidenceFamily::Message,
                        &result.message_id,
                    ));
                    if matches!(
                        coverage.result_dependencies[&result.result_identity].relation,
                        TaskResultDependencyRelation::QualifiedCurrent { .. }
                    ) {
                        if let Some(parent) = result.source.parent_turn_id() {
                            input_roots
                                .insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, parent));
                        }
                        if let Some(id) = &result.work_item_id {
                            work_roots.insert(id.clone());
                        }
                    }
                }
            }
        }
    }
    Ok((input_roots, work_roots))
}

fn finish_companions_tx(
    tx: &Transaction<'_>,
    proof: &TaskWriteProof,
    request: &TaskDomainLoadRequest,
    fixed: &[crate::runtime_db::episode_snapshot::task_checkpoint::TaskContinuityCheckpoint],
    records: Vec<Arc<QualifiedTaskFact>>,
    mut coverage: TaskCoverage,
    facts: ContextScopeFacts,
    input_roots: BTreeSet<InputEvidenceKey>,
    work_roots: BTreeSet<String>,
    material: &mut read_preparation::TaskReadMaterial<'_>,
    budget: Option<&crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
) -> Result<TaskBootstrapSnapshot> {
    let scope = proof.scope();
    for record in &records {
        upsert_alias(
            tx,
            scope,
            &record.locator.key,
            &Alias {
                revision: record.locator.revision,
                availability: TaskAvailability::Present,
                digest: Some(record.locator.content_digest.clone()),
                projection_digest: Some(record.locator.projection_digest.clone()),
            },
        )?;
    }
    let (stop, stop_records) =
        stop_observation::observe_with_result(tx, scope, &records, material, inserted, result)?;
    coverage.stop = stop;
    query_sources::initialize_source_inventory_tx(
        tx,
        scope,
        binding(tx, scope)?.as_deref(),
        fixed,
    )?;
    if query_sources::source_inventory_complete_tx(tx, scope)? {
        material.charge_ids(
            records
                .len()
                .checked_mul(std::mem::size_of::<&TaskFactLocator>())
                .context("Task output selector slots overflow")?,
            records.len(),
        )?;
        let locators = records
            .iter()
            .filter(|record| record.locator.key.family == TaskFamily::Task)
            .map(|record| &record.locator)
            .collect::<Vec<_>>();
        coverage.output =
            query_sources::observe_output_heads_tx(tx, scope, &locators, |bytes, count| {
                material.charge_ids(bytes, count)
            })?;
    }
    coverage.running = running_sources::observe_tx(
        tx,
        scope,
        binding(tx, scope)?.as_deref(),
        fixed,
        |bytes, count| material.charge_ids(bytes, count),
    )?;
    coverage.status = status_observation::observe_with_reused_heads_tx(
        tx,
        scope,
        &records,
        None,
        |bytes, count| material.charge_ids(bytes, count),
    )?;
    coverage.recovery_sources =
        recovery_sources::observe_tx(tx, scope, request, &records, material)?;
    if !request.capture.recovery_source_ids.is_empty() {
        coverage.recovery_at = request.recovery_at;
    }
    for (key, availability) in &coverage.observations {
        if *availability != TaskAvailability::Present && alias(tx, scope, key)?.is_none() {
            upsert_alias(
                tx,
                scope,
                key,
                &Alias {
                    revision: coverage.revision,
                    availability: *availability,
                    digest: None,
                    projection_digest: None,
                },
            )?;
        }
    }
    let bytes = read_preparation::encoded_metadata_bytes(&coverage)?;
    observe_material(
        coverage.observations.len() + input_roots.len() + work_roots.len(),
        bytes,
        Some(request),
    )?;
    material.charge_final_metadata(bytes, coverage.owned_metadata_record_count()?)?;
    sync_child_observers_tx(tx, scope, &records)?;
    let resident = ResidentTaskDomain::bootstrap_with_stops(
        scope.clone(),
        coverage.clone(),
        records,
        stop_records,
        request.record_limit,
        request.byte_limit,
    )?;
    let resident = match budget {
        Some(budget) => resident.with_budget(budget.clone())?,
        None => resident,
    };
    persist_coverage(tx, scope, &coverage)?;
    Ok(TaskBootstrapSnapshot::Current(Box::new(
        TaskCurrentSnapshot {
            facts,
            resident,
            input_roots,
            #[cfg(test)]
            turnless_parent_turns: BTreeSet::new(),
            work_roots,
        },
    )))
}
