use super::qualification::{eligible_digest, manifests, validate_member};
use super::*;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementRecord, TaskResultSettlementState,
};

#[cfg(test)]
pub(crate) fn prepare_task_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
) -> Result<TaskBootstrapSnapshot> {
    load(tx, scope, request, true, false, false, None)
}
#[cfg(test)]
pub(crate) fn load_task_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
) -> Result<TaskBootstrapSnapshot> {
    load(tx, scope, request, false, false, false, None)
}
pub(super) fn load_original(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
) -> Result<TaskBootstrapSnapshot> {
    load(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        !prepare,
        None,
    )
}
/// The original writer selects/validates the same namespace without a second
/// connection, charging material before body decode. Preflight never writes aliases.
pub(super) fn load_original_with_budget(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<TaskBootstrapSnapshot> {
    load(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        !prepare,
        Some(budget),
    )
}
/// Production read-preparation charges owned decode/metadata copies before SQL body reads.
pub(crate) fn prepare_task_domain_snapshot_with_budget_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<TaskBootstrapSnapshot> {
    load(tx, scope, request, true, false, false, Some(budget))
}

/// The original positive receipt / admitted owner preserves its Deleting scope.
pub(crate) fn prepare_original_task_recovery_snapshot_with_budget_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<TaskBootstrapSnapshot> {
    load(tx, scope, request, true, true, false, Some(budget))
}

fn load(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
) -> Result<TaskBootstrapSnapshot> {
    load_with_memory_budget(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        budget,
        None,
    )
}

pub(super) fn load_with_memory_budget<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&'a crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    memory_budget: Option<
        &'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    >,
) -> Result<TaskBootstrapSnapshot> {
    load_with_stop_observation(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        budget,
        memory_budget,
        None,
    )
}
pub(super) fn load_with_stop_observation<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&'a crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    memory_budget: Option<
        &'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    >,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
) -> Result<TaskBootstrapSnapshot> {
    load_with_stop_result_observation(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        budget,
        memory_budget,
        inserted,
        None,
    )
}
pub(super) fn load_with_stop_result_observation<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&'a crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    memory_budget: Option<
        &'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    >,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
) -> Result<TaskBootstrapSnapshot> {
    load_selected(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        budget,
        memory_budget,
        inserted,
        result,
    )
}

/// Compatibility name for focused selector tests; current policy always observes heads.
#[cfg(test)]
pub(crate) fn prepare_task_output_snapshot_with_budget_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<TaskBootstrapSnapshot> {
    prepare_task_domain_snapshot_with_budget_tx(tx, scope, request, budget)
}

fn load_selected<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&'a crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    memory_budget: Option<
        &'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    >,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
) -> Result<TaskBootstrapSnapshot> {
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, budget)
        .with_memory_budget(memory_budget);
    load_selected_using_material(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        budget,
        inserted,
        result,
        &mut material,
    )
}

/// Only an enclosing original producer uses this shared phase allowance.
/// The material grants no alias, mutation, State or related-scope authority.
pub(in crate::runtime_db) fn load_selected_using_material(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    budget: Option<&crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>,
    inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
    mut material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<TaskBootstrapSnapshot> {
    let Some(facts) = material.scope_facts_tx(tx, scope, original_recovery)? else {
        return Ok(TaskBootstrapSnapshot::Retired {
            scope: scope.clone(),
        });
    };
    ensure!(
        request.capture.terminal_limit <= TASK_TERMINAL_WINDOW,
        "Task terminal request exceeds fixed maximum"
    );
    bound(0, 0, Some(request))?;
    let seed = binding(tx, scope)?;
    query_sources::initialize_fresh_source_inventory_tx(tx, scope)?;
    material.charge_checkpoint_metadata(tx, seed.as_deref())?;
    let fixed = manifests(tx, seed.as_deref())?;
    let previous = material.original_current_metadata_tx(tx)?;
    let revision = previous.as_ref().map_or(1, |c| c.revision);
    let active = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        TaskFamily::Task,
        "c.status IN ('queued','running','cancelling')",
        None,
        request,
        None,
        material,
    )?;
    // CallerAdmitted + still Open is part of complete metadata, not omitted here.
    let unsettled = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        TaskFamily::ResultSettlement,
        "c.state!='settled'",
        None,
        request,
        None,
        material,
    )?;
    let mut terminal = namespace_ids(
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
    let mut exhaustive = terminal.len() <= request.capture.terminal_limit;
    terminal.truncate(request.capture.terminal_limit);
    // A truncated fixed historical namespace cannot be upgraded to global exhaustive.
    exhaustive &= fixed
        .iter()
        .filter_map(|m| m.terminal.as_ref())
        .all(|w| w.is_exhaustive);
    if prepare && !preflight {
        query_relations::initialize_tx(tx, scope, &fixed)?;
    }
    let query_coverage = query_relations::observe_tx(tx, scope, &request.query, &mut material)?;
    material.admit_loader_required(&request.capture.required_keys)?;
    let mut keys = request.capture.required_keys.clone();
    keys.extend(
        query_coverage
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
    bound(
        keys.len(),
        keys.iter().map(|k| k.id.len() + 128).sum(),
        Some(request),
    )?;
    let mut stored = BTreeMap::new();
    let mut observations = BTreeMap::new();
    let mut result_dependencies = BTreeMap::new();
    let mut body_bytes = keys.iter().map(|k| k.id.len() + 128).sum::<usize>();
    material.admit_loader_key_copies(&keys)?;
    let mut pending: Vec<_> = keys.iter().cloned().collect();
    while let Some(key) = pending.pop() {
        if observations.contains_key(&key) {
            continue;
        }
        let item = material.original_stored_row_tx(tx, &scope.agent_id, &key, |size| {
            body_bytes = body_bytes
                .checked_add(size.unwrap_or_default())
                .context("Task material bytes overflow")?;
            bound(keys.len(), body_bytes, Some(request))
        })?;
        match item {
            Some(item) => {
                validate_member(tx, scope, seed.as_deref(), &key, &item)?;
                let related_task = if matches!(item.fact, CanonicalTaskFact::ResultSettlement(_)) {
                    let observation = result_relation::observe_result_dependency_tx(
                        tx,
                        scope,
                        seed.as_deref(),
                        &item,
                        request,
                        material,
                    )?;
                    let related = match &observation.relation {
                        TaskResultDependencyRelation::QualifiedCurrent { task } => {
                            Some(task.key.clone())
                        }
                        TaskResultDependencyRelation::Invalid { .. } => None,
                    };
                    body_bytes = body_bytes
                        .checked_add(read_preparation::encoded_metadata_bytes(&observation)?)
                        .context("Task relation coverage bytes overflow")?;
                    bound(keys.len(), body_bytes, Some(request))?;
                    result_dependencies.insert(key.id.clone(), observation);
                    related
                } else {
                    None
                };
                if let Some(related) = related_task {
                    if keys.insert(related.clone()) {
                        body_bytes = body_bytes
                            .checked_add(related.id.len() + 128)
                            .context("Task closure bytes overflow")?;
                        bound(keys.len(), body_bytes, Some(request))?;
                        pending.push(related);
                    }
                }
                // A qualified Task's finite result reverse index includes every qualified state.
                if key.family == TaskFamily::Task {
                    for id in namespace_ids(
                        tx,
                        scope,
                        seed.as_deref(),
                        TaskFamily::ResultSettlement,
                        &format!("c.task_id={}", sql_text(&key.id)),
                        None,
                        request,
                        fixed
                            .iter()
                            .rev()
                            .find_map(|checkpoint| checkpoint.query_inventory.as_ref()),
                        material,
                    )? {
                        let related = TaskKey::new(TaskFamily::ResultSettlement, id);
                        if keys.insert(related.clone()) {
                            body_bytes = body_bytes
                                .checked_add(related.id.len() + 128)
                                .context("Task closure bytes overflow")?;
                            bound(keys.len(), body_bytes, Some(request))?;
                            pending.push(related);
                        }
                    }
                }
                observations.insert(key.clone(), TaskAvailability::Present);
                stored.insert(key, item);
            }
            None => {
                if let Some(a) = alias(tx, scope, &key)? {
                    ensure!(
                        a.availability != TaskAvailability::Present,
                        "qualified Task canonical body missing"
                    );
                    observations.insert(key, a.availability);
                } else {
                    ensure!(
                        !qualification::known_anywhere(tx, &key)?
                            && eligible_digest(tx, seed.as_deref(), &key)?.is_none(),
                        "known Task body missing"
                    );
                    observations.insert(key, TaskAvailability::Absent);
                }
            }
        }
        bound(keys.len(), body_bytes, Some(request))?;
    }
    let mut coverage = TaskCoverage {
        revision,
        stop: TaskStopCoverage::default(),
        output: TaskOutputCoverage::default(),
        status: Default::default(),
        query: query_coverage,
        running: Default::default(),
        active_complete: true,
        unsettled_complete: true,
        active_task_ids: active.into_iter().collect(),
        unsettled_result_ids: unsettled.into_iter().collect(),
        task_metadata: BTreeMap::new(),
        result_metadata: BTreeMap::new(),
        result_dependencies,
        task_result_ids: BTreeMap::new(),
        activation_result_ids: BTreeMap::new(),
        reply_result_ids: BTreeMap::new(),
        required_keys: request.capture.required_keys.clone(),
        observations,
        terminal: TaskHistoryCoverage {
            limit: request.capture.terminal_limit,
            keys: terminal
                .into_iter()
                .map(|id| TaskKey::new(TaskFamily::Task, id))
                .collect(),
            is_exhaustive: exhaustive,
        },
    };
    let mut input_roots = BTreeSet::new();
    let mut work_roots = BTreeSet::new();
    for item in stored.values() {
        material.admit_loader_metadata(item)?;
        match &item.fact {
            CanonicalTaskFact::Task(task) => {
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
                    input_roots.insert(InputEvidenceKey::new(
                        InputEvidenceFamily::Turn,
                        fence.parent_turn_id,
                    ));
                }
            }
            CanonicalTaskFact::ResultSettlement(result) => {
                let metadata =
                    result_metadata(tx, scope, result, request, body_bytes, &mut material)?;
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
                        input_roots.insert(InputEvidenceKey::new(
                            InputEvidenceFamily::Turn,
                            &result.parent_turn_id,
                        ));
                        if let Some(id) = &result.work_item_id {
                            work_roots.insert(id.clone());
                        }
                    }
                }
            }
        }
    }
    let mut records = Vec::new();
    for (key, item) in &stored {
        let r = validate_member(tx, scope, seed.as_deref(), key, item)?;
        records.push(material.original_qualified_row(item, scope, r)?);
        if prepare {
            upsert_alias(
                tx,
                scope,
                key,
                &Alias {
                    revision: r,
                    availability: TaskAvailability::Present,
                    digest: Some(item.digest()),
                    projection_digest: Some(item.projection_digest.clone()),
                },
            )?;
        }
    }
    let (stop_coverage, stop_records) =
        stop_observation::observe_with_result(tx, scope, &records, material, inserted, result)?;
    coverage.stop = stop_coverage;
    // The ordinary current policy owns bounded heads/window metadata for
    // every selected Task. Body acquisition remains a separate Input request.
    if prepare && !preflight {
        query_sources::initialize_source_inventory_tx(tx, scope, seed.as_deref(), &fixed)?;
    }
    if query_sources::source_inventory_complete_tx(tx, scope)? {
        material.charge_ids(
            records
                .len()
                .checked_mul(std::mem::size_of::<&TaskFactLocator>())
                .context("Task output selector slots overflow")?,
            records.len(),
        )?;
        let task_locators = records
            .iter()
            .filter(|record| record.locator.key.family == TaskFamily::Task)
            .map(|record| &record.locator)
            .collect::<Vec<_>>();
        coverage.output =
            query_sources::observe_output_heads_tx(tx, scope, &task_locators, |bytes, count| {
                material.charge_ids(bytes, count)
            })?;
    }
    coverage.running =
        running_sources::observe_tx(tx, scope, seed.as_deref(), &fixed, |bytes, count| {
            material.charge_ids(bytes, count)
        })?;
    coverage.status = observe_task_status_headers_tx(tx, scope, &records, |bytes, count| {
        material.charge_ids(bytes, count)
    })?;
    if let Some(previous) = previous.as_ref() {
        if !prepare && !preflight && previous != &coverage {
            // A changed selector/metadata view at the same revision needs
            // named preparation; it is not the concurrent-revision retry case.
            anyhow::bail!("Task coverage needs explicit preparation at unchanged revision");
        }
    }
    // The comparison consumed this independently decoded old coverage.
    previous.retire(material)?;
    // Explicit absence is acquired only after global canonical + every-scope membership checks.
    if prepare {
        for (key, availability) in &coverage.observations {
            if *availability != TaskAvailability::Present && alias(tx, scope, key)?.is_none() {
                upsert_alias(
                    tx,
                    scope,
                    key,
                    &Alias {
                        revision,
                        availability: *availability,
                        digest: None,
                        projection_digest: None,
                    },
                )?;
            }
        }
    }
    let bytes = read_preparation::encoded_metadata_bytes(&coverage)?;
    bound(
        keys.len() + input_roots.len() + work_roots.len(),
        bytes,
        Some(request),
    )?;
    material.charge_final_metadata(bytes, coverage.owned_metadata_record_count()?)?;
    if prepare {
        sync_child_observers_tx(tx, scope, &records)?;
    }
    let resident = material.with_original_resident_ctor(
        scope.clone(), coverage, records, stop_records, budget,
    )?;
    if prepare {
        persist_coverage(tx, scope, resident.coverage())?;
    }
    Ok(TaskBootstrapSnapshot::Current(Box::new(
        TaskCurrentSnapshot {
            facts,
            resident,
            input_roots,
            work_roots,
        },
    )))
}
fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn namespace_ids(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    family: TaskFamily,
    predicate: &str,
    limit: Option<usize>,
    request: &TaskDomainLoadRequest,
    query_inventory: Option<
        &super::super::episode_snapshot::task_checkpoint::queries::TaskQueryInventory,
    >,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<Vec<String>> {
    let (table, id, owner) = family_table(family);
    if let Some(inventory) = query_inventory {
        ensure!(
            family == TaskFamily::ResultSettlement && inventory.agent_id == scope.agent_id,
            "fixed Result reverse index crossed scope/family"
        );
        super::super::episode_snapshot::task_checkpoint::queries::validate(tx, inventory)?;
    }
    let order = if limit.is_some() {
        format!("c.updated_at DESC,c.created_at DESC,c.{id} ASC")
    } else {
        format!("c.{id} ASC")
    };
    let sql=format!("SELECT c.{id} FROM {table} c WHERE c.{owner}=?1 AND ({predicate}) AND NOT EXISTS(SELECT 1 FROM task_domain_records r WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=c.{id} AND r.availability!='present') AND (EXISTS(SELECT 1 FROM task_domain_records r WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=c.{id} AND r.availability='present') OR EXISTS(SELECT 1 FROM task_domain_eligibility e WHERE e.seed_id=?4 AND e.family=?3 AND e.record_id=c.{id} AND e.availability='present') OR EXISTS(SELECT 1 FROM task_query_fixed_results q WHERE q.inventory_id=?6 AND q.result_identity=c.{id})) ORDER BY {order} LIMIT ?5");
    let query_limit = i64::try_from(limit.unwrap_or(request.record_limit.min(MAX_RECORDS) + 1))?;
    let query_family = family_name(family);
    let query_inventory_id = query_inventory.map(|inventory| inventory.id.as_str());
    let query_values = params![
        scope.agent_id,
        scope.identity_incarnation,
        query_family,
        seed,
        query_limit,
        query_inventory_id
    ];
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({id} AS BLOB))+128),0) FROM ({sql})"),
        query_values,
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    bound(count, bytes, Some(request))?;
    material.charge_ids(bytes, count)?;
    let mut q = tx.prepare(&sql)?;
    let mut out = Vec::with_capacity(count);
    let mut bytes = 0usize;
    for row in q.query_map(
        params![
            scope.agent_id,
            scope.identity_incarnation,
            family_name(family),
            seed,
            i64::try_from(limit.unwrap_or(request.record_limit.min(MAX_RECORDS) + 1))?,
            query_inventory.map(|inventory| inventory.id.as_str()),
        ],
        |r| r.get::<_, String>(0),
    )? {
        let id = row?;
        bytes = bytes
            .checked_add(id.len() + 128)
            .context("Task ID bytes overflow")?;
        bound(out.len() + 1, bytes, Some(request))?;
        out.push(id)
    }
    ensure!(
        out.len() == count,
        "Task namespace changed in original view"
    );
    Ok(out)
}
fn result_metadata(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    x: &TaskResultSettlementRecord,
    request: &TaskDomainLoadRequest,
    resident_body_bytes: usize,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<TaskResultMetadata> {
    let activation_open = if x.state == TaskResultSettlementState::CallerAdmitted {
        let bytes:Option<usize>=tx.query_row(
            "SELECT length(CAST(agent_id AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(lifecycle_state AS BLOB))+COALESCE(length(CAST(json_extract(payload_json,'$.agent_id') AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.state') AS BLOB)),0) FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
            params![scope.agent_id,x.activation_id],|r|r.get(0)).optional()?;
        bound(1, bytes.unwrap_or_default(), Some(request))?;
        material.charge_relation(bytes.unwrap_or_default())?;
        let row:Option<(String,String,String,String,String)>=tx.query_row(
            "SELECT agent_id,attempt_id,lifecycle_state,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.state') FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
            params![scope.agent_id,x.activation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        match row {
            None => false,
            Some((agent, id, status, payload_agent, payload_status)) => {
                ensure!(
                    agent == scope.agent_id
                        && Some(id.as_str()) == x.activation_id.as_deref()
                        && payload_agent == agent
                        && payload_status == status,
                    "Task activation projection mismatch"
                );
                status == "open"
            }
        }
    } else {
        false
    };
    let reply_message_id = if x.state != TaskResultSettlementState::Settled {
        let size: Option<usize> = tx
            .query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE message_id=?1",
                [&x.message_id],
                |r| r.get(0),
            )
            .optional()?;
        bound(
            1,
            resident_body_bytes
                .checked_add(size.unwrap_or_default())
                .context("Task reply material overflow")?,
            Some(request),
        )?;
        material.charge_message(&x.message_id, size.unwrap_or_default())?;
        let raw: Option<(String, String)> = tx
            .query_row(
                "SELECT agent_id,payload_json FROM messages WHERE message_id=?1",
                [&x.message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (agent, payload) = raw.context("unsettled Task result canonical Message missing")?;
        let message = repositories::decode_message_payload(&payload)?;
        ensure!(
            agent == scope.agent_id
                && message.agent_id == agent
                && message.id == x.message_id
                && message.task_id.as_deref() == Some(x.task_id.as_str()),
            "Task result Message identity mismatch"
        );
        crate::wake_contract::agent_message_reply_reference(&message).map(str::to_owned)
    } else {
        None
    };
    Ok(TaskResultMetadata {
        result_identity: x.result_identity.clone(),
        task_id: x.task_id.clone(),
        message_id: x.message_id.clone(),
        work_item_id: x.work_item_id.clone(),
        rejoin_generation: x.rejoin_generation,
        parent_turn_id: x.parent_turn_id.clone(),
        state: x.state,
        activation_id: x.activation_id.clone(),
        activation_open,
        reply_message_id,
        created_at: x.created_at,
        next_recheck_at: x.next_recheck_at,
    })
}
pub(super) fn current_coverage(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<Option<TaskCoverage>> {
    let size:Option<(u64,usize)>=tx.query_row("SELECT revision,length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((revision, size)) = size else {
        return Ok(None);
    };
    bound(0, size, None)?;
    let raw:String=tx.query_row("SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    let coverage: TaskCoverage = serde_json::from_str(&raw)?;
    ensure!(
        coverage.revision == revision,
        "Task coverage revision projection mismatch"
    );
    Ok(Some(coverage))
}
pub(super) fn persist_coverage(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    coverage: &TaskCoverage,
) -> Result<()> {
    let raw = serde_json::to_string(coverage)?;
    bound(coverage.observations.len(), raw.len(), None)?;
    tx.execute("INSERT INTO task_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",params![scope.agent_id,scope.identity_incarnation,coverage.revision,raw])?;
    Ok(())
}
pub(crate) fn validate_task_domain_observation_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    revision: u64,
) -> Result<()> {
    ensure!(
        scope_facts(tx, scope, false)?.is_some(),
        "Task observation retired"
    );
    let actual = current_coverage(tx, scope)?.map(|c| c.revision);
    if actual != Some(revision) {
        return Err(TaskSnapshotError::RevisionMismatch {
            expected: revision,
            actual,
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod status_loader_tests {
    use super::*;
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    use crate::types::{
        TaskKind, TaskRecord, TaskStatus, WorkItemDelegationRecord, WorkItemDelegationState,
    };

    fn write(tx: &Transaction<'_>, task: &TaskRecord) -> Result<CommittedTaskFacts> {
        let scope = super::super::tests::scope();
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, task)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new(&scope.agent_id),
            &super::super::tests::request(),
        )
    }

    #[test]
    fn original_task_ack_contains_exact_status_and_child_index_without_canonical_readback(
    ) -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let request = super::super::tests::request();
        let before = db
            .transaction(|tx| {
                prepare_task_domain_snapshot_tx(tx, &scope, &request)?.require_current()
            })?
            .resident;
        let mut task = super::super::tests::task("loader-status", TaskStatus::Cancelled);
        task.kind = TaskKind::WorktreeSubagentTask;
        task.detail = Some(
            serde_json::json!({"child_agent_id":"loader-child","worktree":{"path":"/original","branch":"original"}}),
        );
        let delegation = WorkItemDelegationRecord {
            delegation_id: "loader-delegation".into(),
            parent_agent_id: "global-parent".into(),
            parent_work_item_id: "global-parent-work".into(),
            child_agent_id: "loader-child".into(),
            child_work_item_id: "global-child-work".into(),
            state: WorkItemDelegationState::Open,
            result_summary: None,
            created_at: task.created_at,
            updated_at: task.updated_at,
        };
        db.work_item_delegations().upsert(&delegation)?;
        let facts = db.transaction(|tx| write(tx, &task))?;
        let locator = &facts.delta.coverage.status.heads[&task.id].task;
        let index: (String, u64, String, String) = db.connection()?.query_row(
            "SELECT child_agent_id,task_revision,content_digest,projection_digest FROM task_status_child_observers WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3",
            params![scope.agent_id,scope.identity_incarnation,task.id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )?;
        assert_eq!(
            index,
            (
                "loader-child".into(),
                locator.revision,
                locator.content_digest.clone(),
                locator.projection_digest.clone()
            )
        );
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let resident = before
            .with_committed(&facts.delta)?
            .with_budget(ledger.clone())?;
        db.transaction(|tx| {
            tx.execute_batch("ALTER TABLE tasks RENAME TO status_task_body_forbidden;ALTER TABLE control_cancellation_obligations RENAME TO status_control_body_forbidden;ALTER TABLE work_item_delegations RENAME TO status_delegation_body_forbidden;")?;
            Ok(())
        })?;
        let captured = resident.capture(&request.capture)?;
        let record = captured
            .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
            .unwrap();
        let status = captured.task_status_projection(&task.id)?;
        assert_eq!(status.task, record.locator);
        let child = status.child_supervision(record)?.unwrap();
        assert_eq!(child.delegation_id.as_deref(), Some("loader-delegation"));
        assert_eq!(
            child.parent_work_item_id.as_deref(),
            Some("global-parent-work")
        );
        assert!(ledger.report()?.observed_bytes > 0);
        drop((captured, resident, facts));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn actual_task_update_replaces_child_discovery_with_its_new_exact_version() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let mut task = super::super::tests::task("loader-child-update", TaskStatus::Running);
        task.kind = TaskKind::ChildAgentTask;
        task.detail = Some(serde_json::json!({"child_agent_id":"before-child"}));
        let before = db.transaction(|tx| write(tx, &task))?;
        task.updated_at += chrono::Duration::seconds(1);
        task.detail = Some(serde_json::json!({"child_agent_id":"after-child"}));
        let after = db.transaction(|tx| write(tx, &task))?;
        let previous = &before.delta.coverage.status.heads[&task.id];
        let next = &after.delta.coverage.status.heads[&task.id];
        assert_ne!(previous.task, next.task);
        let rows: Vec<(String,u64)> = db.connection()?.prepare(
            "SELECT child_agent_id,task_revision FROM task_status_child_observers WHERE agent_id=?1 AND identity_incarnation=?2 ORDER BY task_id",
        )?.query_map(params![scope.agent_id,scope.identity_incarnation], |row| Ok((row.get(0)?,row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        assert_eq!(rows, vec![("after-child".into(), next.task.revision)]);
        Ok(())
    }

    #[test]
    fn loader_rejects_oversized_accepted_status_before_publication_and_preserves_previous_index(
    ) -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let mut task = super::super::tests::task("loader-status-budget", TaskStatus::Cancelled);
        task.kind = TaskKind::WorktreeSubagentTask;
        task.detail = Some(
            serde_json::json!({"child_agent_id":"budget-child","worktree":{"path":"/original","branch":"original"}}),
        );
        db.transaction(|tx| write(tx, &task))?;
        let mut request = super::super::tests::request();
        request.record_limit = 1024;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        db.transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &request, &ledger)?
                .require_current()
        })?;
        let before = db.transaction(|tx| current_coverage(tx, &scope))?;
        // A bounded observer fixture; original Control acceptance has separate writer tests.
        let generation = serde_json::json!({"created_at":task.created_at,"rejoin":task.rejoin_fence().ok(),"artifact":task.detail.as_ref().unwrap()["worktree"]});
        db.transaction(|tx| {
            tx.execute("INSERT INTO task_status_control_sources VALUES(?1,?2,'budget-source','worktree',?3,?4,?5,?6)",params![scope.agent_id,scope.identity_incarnation,task.id,generation.to_string(),serde_json::json!({"changed_files":vec!["file";1536]}).to_string(),repositories::timestamp(task.updated_at)])?;
            Ok(())
        })?;
        let error = db
            .transaction(|tx| {
                prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &request, &ledger)
            })
            .unwrap_err();
        assert!(
            error.downcast_ref::<TaskSnapshotError>().is_some(),
            "{error:#}"
        );
        assert_eq!(db.transaction(|tx| current_coverage(tx, &scope))?, before);
        let child: String = db.connection()?.query_row("SELECT child_agent_id FROM task_status_child_observers WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3", params![scope.agent_id,scope.identity_incarnation,task.id], |row| row.get(0))?;
        assert_eq!(child, "budget-child");
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn saved_large_status_is_bounded_before_any_corrupt_current_coverage_decode() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let mut task = super::super::tests::task("saved-status-budget", TaskStatus::Cancelled);
        task.kind = TaskKind::WorktreeSubagentTask;
        task.detail = Some(
            serde_json::json!({"child_agent_id":"saved-child","worktree":{"path":"/original","branch":"original"}}),
        );
        db.transaction(|tx| write(tx, &task))?;
        let generation = serde_json::json!({"created_at":task.created_at,"rejoin":task.rejoin_fence().ok(),"artifact":task.detail.as_ref().unwrap()["worktree"]});
        db.transaction(|tx| {
            tx.execute("INSERT INTO task_status_control_sources VALUES(?1,?2,'saved-source','worktree',?3,?4,?5,?6)",params![scope.agent_id,scope.identity_incarnation,task.id,generation.to_string(),serde_json::json!({"changed_files":vec!["file";1536]}).to_string(),repositories::timestamp(task.updated_at)])?;
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut large = super::super::tests::request();
        large.record_limit = 65_536;
        db.transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &large, &ledger)?
                .require_current()
        })?;
        db.transaction(|tx| {
            tx.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.revision','malformed-original-revision') WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation])?;
            Ok(())
        })?;
        let mut small = large;
        small.record_limit = 1024;
        let error = db
            .transaction(|tx| {
                prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &small, &ledger)
            })
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "capacity must reject before malformed revision decode: {error:#}"
        );
        let saved: String = db.connection()?.query_row("SELECT json_extract(coverage_json,'$.revision') FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation], |row|row.get(0))?;
        assert_eq!(saved, "malformed-original-revision");
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
