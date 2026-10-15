use super::read_preparation::WorkReadMaterial;
use super::*;
use crate::runtime::input_evidence_budget::{serialized_bytes, InputEvidenceBudgetLedger};
use crate::runtime::resident_input_evidence::LegacyDeliveryHeadSelector;
use crate::runtime::resident_work::{WorkDeliveryHead, WorkDeliveryHeads};
use qualification::{eligible_digest, manifests, validate_member};

#[cfg(test)]
pub(crate) fn load_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, false, false, false, None)
}
pub(crate) fn prepare_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, true, false, false, None)
}
/// The caller must hold the same original admitted-cleanup owner reservation;
/// this entry never authorizes ordinary activation of a Deleting identity.
pub(crate) fn prepare_original_work_recovery_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, true, true, false, None)
}

/// Same finite qualification and body budget checks, without sidecar mutation.
#[cfg(test)]
pub(crate) fn preflight_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, false, false, true, None)
}
pub(super) fn preflight_original_work_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, false, original_recovery, true, None)
}

/// Uses the caller's actual Host ledger and, for Memory, its existing remaining
/// transaction allowance. The original loader predicates and writers are shared.
pub(crate) fn prepare_work_domain_snapshot_with_budget_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<WorkBootstrapSnapshot> {
    let mut material = WorkReadMaterial::new(scope, request, ledger, memory)?;
    let mut snapshot = load(
        tx,
        scope,
        request,
        true,
        original_recovery,
        false,
        Some(&mut material),
    )?;
    if let WorkBootstrapSnapshot::Current(current) = &mut snapshot {
        current.material_usage = material.usage();
        current.material_views = material.into_guards();
    }
    Ok(snapshot)
}

pub(super) fn prepare_work_domain_snapshot_with_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
    prepare: bool,
    material: &mut WorkReadMaterial<'_>,
) -> Result<WorkBootstrapSnapshot> {
    load(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        !prepare,
        Some(material),
    )
}

fn load(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    mut material: Option<&mut WorkReadMaterial<'_>>,
) -> Result<WorkBootstrapSnapshot> {
    let facts = match material.as_deref_mut() {
        Some(material) => material.scope_tx(tx, original_recovery)?,
        None => scope_facts(tx, scope, original_recovery)?,
    };
    let Some(facts) = facts else {
        return Ok(WorkBootstrapSnapshot::Retired {
            scope: scope.clone(),
        });
    };
    ensure!(
        request.capture.nonopen_limit <= WORK_NONOPEN_WINDOW,
        "Work history request exceeds fixed maximum"
    );
    if let Some(material) = material.as_deref_mut() {
        material.selectors(&request.capture)?;
    }
    let seed = match material.as_deref_mut() {
        Some(m) => m.binding_tx(tx)?,
        None => binding(tx, scope)?,
    };
    let previous = match material.as_deref_mut() {
        Some(m) => m.coverage_tx(tx)?,
        None => current_coverage(tx, scope)?,
    };
    let revision = previous.as_ref().map_or(1, |coverage| coverage.revision);
    let fixed = match material.as_deref_mut() {
        Some(m) => m.manifests_tx(tx, seed.as_deref())?,
        None => manifests(tx, seed.as_deref())?,
    };
    let mut heads = BTreeMap::new();
    for manifest in fixed {
        heads.extend(manifest.latest_results.into_iter().map(|(work, head)| {
            (
                work,
                head.map(|head| WorkCompletionRef {
                    brief_id: head.brief_id,
                    transcript_entry_id: head.transcript_entry_id,
                    source_message_id: head.source_message_id,
                }),
            )
        }));
    }
    if let Some(previous) = &previous {
        if let Some(material) = material.as_deref_mut() {
            material.coverage_copy(previous, 1)?;
        }
        heads.extend(previous.latest_results.clone());
    }
    let live_work_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Work,
        "c.state IN ('open','completing')",
        None,
        material.as_deref_mut(),
    )?;
    let unresolved_wait_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Wait,
        "c.status IN ('active','triggered')",
        None,
        material.as_deref_mut(),
    )?;
    let active_frame_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Frame,
        "c.state='active'",
        None,
        material.as_deref_mut(),
    )?;
    let candidates = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Work,
        "c.state!='open'",
        Some(request.capture.nonopen_limit + 1),
        material.as_deref_mut(),
    )?;
    // namespace_ids uses the canonical history order when a limit is provided.
    // Namespace admission covers its derived ID/key copies.
    let nonopen_keys = candidates
        .iter()
        .take(request.capture.nonopen_limit)
        .map(|id| WorkKey::new(WorkFamily::Work, id))
        .collect::<Vec<_>>();
    let mut required = request.capture.mandatory_roots.clone();
    required.extend(
        request
            .capture
            .latest_result_work_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    if let Some(state) = &facts.state {
        if let Some(work) = &state.current_work_item_id {
            required.insert(WorkKey::new(WorkFamily::Work, work));
        }
        if let Some(work) = state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.work_item_id.as_ref())
        {
            required.insert(WorkKey::new(WorkFamily::Work, work));
        }
    }
    let (turn_wait_keys, task_wait_keys, trigger_wait_keys) =
        super::relations::load_wait_relations_tx(
            tx,
            scope,
            &facts,
            request,
            &mut required,
            material.as_deref_mut(),
        )?;
    if let Some(material) = material.as_deref_mut() {
        material.keys(&required, 3)?;
    }
    let mut pending = required.clone();
    pending.extend(
        live_work_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    pending.extend(
        unresolved_wait_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id)),
    );
    pending.extend(
        active_frame_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Frame, id)),
    );
    pending.extend(nonopen_keys.iter().cloned());
    pending.extend(
        request
            .capture
            .latest_result_work_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    bound_metadata(
        pending.len(),
        pending
            .iter()
            .map(|key| key.id.len() + std::mem::size_of::<WorkKey>())
            .sum(),
    )?;
    if pending.len() > request.record_limit {
        return Err(WorkSnapshotError::ResourceExhausted("selected record count").into());
    }
    let mut records = Vec::new();
    let mut observations = BTreeMap::new();
    let mut bytes = 0usize;
    let mut metadata_bytes = 0usize;
    let mut depth = BTreeMap::from_iter(pending.iter().cloned().map(|key| (key, 0usize)));
    while let Some(key) = pending.pop_first() {
        if observations.contains_key(&key) {
            continue;
        }
        metadata_bytes =
            metadata_bytes.saturating_add(key.id.len() + std::mem::size_of::<WorkKey>());
        bound_metadata(observations.len() + 1, metadata_bytes)?;
        if observations.len() >= request.record_limit {
            return Err(WorkSnapshotError::ResourceExhausted("selected record count").into());
        }
        let alias = match material.as_deref_mut() {
            Some(m) => m.alias_tx(tx, &key)?,
            None => alias(tx, scope, &key)?,
        };
        if alias
            .as_ref()
            .is_some_and(|alias| alias.availability == WorkAvailability::Pruned)
        {
            observations.insert(key, WorkAvailability::Pruned);
            continue;
        }
        let (table, id) = family_table(key.family);
        let length: Option<usize> = tx
            .query_row(
                &format!("SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {id}=?1"),
                [&key.id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(length) = length else {
            let eligible = match material.as_deref_mut() {
                Some(m) => m.eligible_tx(tx, seed.as_deref(), &key)?,
                None => eligible_digest(tx, seed.as_deref(), &key)?,
            };
            ensure!(
                eligible.is_none() && !qualification::known_anywhere(tx, &key)?,
                "known Work body is unavailable, not absent"
            );
            observations.insert(key.clone(), WorkAvailability::Absent);
            if prepare {
                upsert_alias(
                    tx,
                    scope,
                    &key,
                    &Alias {
                        revision,
                        availability: WorkAvailability::Absent,
                        digest: None,
                        fingerprint: None,
                    },
                )?;
            }
            continue;
        };
        bytes = bytes.saturating_add(length);
        if bytes > request.byte_limit {
            return Err(WorkSnapshotError::ResourceExhausted("selected body bytes").into());
        }
        let stored = match material.as_deref_mut() {
            Some(m) => m.record_tx(tx, &key)?,
            None => stored_on(tx, &scope.agent_id, &key)?,
        }
        .context("Work body disappeared in read transaction")?;
        let record_revision = match material.as_deref_mut() {
            Some(m) => m.validate_member_tx(tx, seed.as_deref(), &key, &stored)?,
            None => validate_member(tx, scope, seed.as_deref(), &key, &stored)?,
        };
        if !prepare && !preflight && alias.is_none() {
            return Err(WorkSnapshotError::NeedsPreparation(key).into());
        }
        if prepare && alias.is_none() {
            upsert_alias(
                tx,
                scope,
                &key,
                &Alias {
                    revision: record_revision,
                    availability: WorkAvailability::Present,
                    digest: Some(stored.digest()),
                    fingerprint: Some(stored.fingerprint.clone()),
                },
            )?;
        }
        if prepare {
            if let CanonicalWorkRecord::Wait(wait) = &stored.record {
                super::relations::register_wait_edges_tx(tx, scope, wait, record_revision, false)?;
            }
        }
        let next_depth = depth.get(&key).copied().unwrap_or_default() + 1;
        ensure!(
            next_depth <= MAX_CLOSURE_DEPTH,
            "Work owning closure depth exhausted"
        );
        // The admitted body/native footprint includes owning-reference copies.
        for child in stored.record.references() {
            if !observations.contains_key(&child) {
                depth.entry(child.clone()).or_insert(next_depth);
                pending.insert(child);
            }
        }
        observations.insert(key, WorkAvailability::Present);
        records.push(stored.qualified(scope, record_revision)?);
    }
    let selected_work = observations
        .keys()
        .filter(|key| key.family == WorkFamily::Work)
        .map(|key| key.id.clone())
        .collect::<BTreeSet<_>>();
    // The durable finite-by-key index survives resident window trimming.
    for work in &selected_work {
        let head = match material.as_deref_mut() {
            Some(m) => m.result_head_tx(tx, work)?,
            None => result_head_on(tx, scope, work)?,
        };
        if let Some(head) = head {
            heads.insert(work.clone(), head);
        }
    }
    if prepare || preflight {
        for work in &request.capture.latest_result_work_ids {
            if let Some(Some(head)) = heads.get(work) {
                let refreshed = super::completion::completion_head_refs_with_material_tx(
                    tx,
                    scope,
                    &head.brief_id,
                    material.as_deref_mut(),
                )?;
                heads.insert(work.clone(), Some(refreshed));
            }
        }
    }
    let latest_results = selected_work
        .iter()
        .map(
            |work| match observations[&WorkKey::new(WorkFamily::Work, work)] {
                WorkAvailability::Present => heads
                    .get(work)
                    .cloned()
                    .map(|head| (work.clone(), head))
                    .ok_or_else(|| {
                        WorkSnapshotError::NeedsPreparation(WorkKey::new(WorkFamily::Work, work))
                            .into()
                    }),
                WorkAvailability::Absent | WorkAvailability::Pruned => Ok((work.clone(), None)),
            },
        )
        .collect::<Result<BTreeMap<_, _>>>()?;
    if prepare {
        for (work, head) in &latest_results {
            persist_result_head(tx, scope, work, revision, head)?;
        }
    }
    let mut legacy_delivery_heads = BTreeMap::new();
    if let Some(m) = material.as_deref_mut() {
        let initialized:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_scopes WHERE agent_id=?1 AND identity_incarnation=?2)",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
        if prepare {
            super::super::input_evidence_snapshot::legacy_delivery::heads::initialize_scope_tx(
                tx,
                scope,
                seed.as_deref(),
            )?;
        }
        if prepare || initialized {
            for work in &selected_work {
                if observations[&WorkKey::new(WorkFamily::Work, work)] != WorkAvailability::Present
                {
                    continue;
                }
                let latest_any =
                    super::super::input_evidence_snapshot::legacy_delivery::heads::observe_head_tx(
                        tx,
                        scope,
                        work,
                        LegacyDeliveryHeadSelector::LatestAny,
                        |b, r| m.observe_projected(b, r),
                    )?;
                let latest_nonempty =
                    super::super::input_evidence_snapshot::legacy_delivery::heads::observe_head_tx(
                        tx,
                        scope,
                        work,
                        LegacyDeliveryHeadSelector::LatestNonempty,
                        |b, r| m.observe_projected(b, r),
                    )?;
                legacy_delivery_heads.insert(
                    work.clone(),
                    WorkDeliveryHeads {
                        latest_any: WorkDeliveryHead::from_observation(scope, latest_any)?,
                        latest_nonempty: WorkDeliveryHead::from_observation(
                            scope,
                            latest_nonempty,
                        )?,
                    },
                );
            }
        }
    } else if let Some(previous) = &previous {
        // Legacy metadata-only setup has no actual Host material. Preserve only
        // already prepared facts; missing is a separate report dependency.
        legacy_delivery_heads = previous
            .legacy_delivery_heads
            .iter()
            .filter(|(id, _)| selected_work.contains(*id))
            .map(|(id, heads)| (id.clone(), heads.clone()))
            .collect();
    }
    let mut coverage = WorkCoverage {
        revision,
        live_complete: true,
        live_work_ids: live_work_ids.into_iter().collect(),
        unresolved_wait_ids: unresolved_wait_ids.into_iter().collect(),
        active_frame_ids: active_frame_ids.into_iter().collect(),
        mandatory_roots: required,
        observations,
        nonopen: WorkHistoryCoverage {
            limit: request.capture.nonopen_limit,
            keys: nonopen_keys,
            is_exhaustive: candidates.len() <= request.capture.nonopen_limit,
        },
        latest_results,
        legacy_delivery_heads,
        turn_wait_keys,
        task_wait_keys,
        trigger_wait_keys,
    };
    if let Some(previous) = &previous {
        if let Some(material) = material.as_deref_mut() {
            material.coverage_copy(&coverage, 1)?;
        }
        let mut prior = coverage.clone();
        prior.revision = previous.revision;
        if &prior != previous {
            coverage.revision = previous
                .revision
                .checked_add(1)
                .context("Work revision overflow")?;
        }
    }
    if let Some(material) = material.as_deref_mut() {
        material.coverage_copy(&coverage, 4)?;
    }
    let resident = ResidentWorkDomain::bootstrap(
        scope.clone(),
        coverage.clone(),
        records,
        request.record_limit,
        request.byte_limit,
    )?;
    if prepare {
        persist_coverage(tx, scope, &coverage)?;
    } else if !preflight {
        ensure!(
            previous.as_ref() == Some(&coverage),
            "Work selected coverage requires preparation"
        );
    }
    let owning_input_roots = owning_input_roots(&resident, material.as_deref_mut())?;
    Ok(WorkBootstrapSnapshot::Current(Box::new(
        WorkCurrentSnapshot {
            facts,
            resident,
            owning_input_roots,
            material_usage: material.as_deref().map_or((0, 0), WorkReadMaterial::usage),
            material_views: Vec::new(),
        },
    )))
}

pub(super) fn owning_input_roots(
    resident: &ResidentWorkDomain,
    mut material: Option<&mut WorkReadMaterial<'_>>,
) -> Result<BTreeSet<InputEvidenceKey>> {
    if let Some(m) = material.as_deref_mut() {
        m.owning_roots_copy(resident)?;
    }
    let request = WorkCaptureRequest {
        include_live: true,
        nonopen_limit: resident.coverage().nonopen.limit,
        mandatory_roots: resident.coverage().mandatory_roots.clone(),
        latest_result_work_ids: resident.coverage().latest_results.keys().cloned().collect(),
        required_turn_ids: resident.coverage().turn_wait_keys.keys().cloned().collect(),
        required_task_ids: resident.coverage().task_wait_keys.keys().cloned().collect(),
        required_trigger_message_ids: resident
            .coverage()
            .trigger_wait_keys
            .keys()
            .cloned()
            .collect(),
    };
    let captured = match material {
        Some(m) => m.capture_resident(resident, &request)?,
        None => resident.capture(&request)?,
    };
    owning_input_roots_of_records(resident, &captured.records)
}

/// Receipt-only historical Work bodies are selectors, not current Input roots.
/// This bridge retains all live owning closure plus the current State Work.
pub(super) fn current_recovery_owning_input_roots(
    resident: &ResidentWorkDomain,
    state: &AgentState,
    material: &mut WorkReadMaterial<'_>,
) -> Result<BTreeSet<InputEvidenceKey>> {
    material.owning_roots_copy(resident)?;
    material.raw(
        usize::try_from(serialized_bytes(&(
            &state.current_work_item_id,
            &state.current_execution_binding,
        ))?)?,
        2,
    )?;
    let mut mandatory = BTreeSet::new();
    mandatory.extend(
        state
            .current_work_item_id
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    mandatory.extend(
        state
            .current_execution_binding
            .iter()
            .filter_map(|binding| binding.work_item_id.as_ref())
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    let captured = material.capture_resident(
        resident,
        &WorkCaptureRequest {
            nonopen_limit: 0,
            mandatory_roots: mandatory.clone(),
            ..WorkCaptureRequest::current()
        },
    )?;
    // capture also retains published mandatory selectors. They include old
    // receipt targets, so choose current owning closure explicitly here.
    mandatory.extend(
        resident
            .coverage()
            .live_work_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    mandatory.extend(
        resident
            .coverage()
            .unresolved_wait_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id)),
    );
    mandatory.extend(
        resident
            .coverage()
            .active_frame_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Frame, id)),
    );
    let records = captured
        .records
        .into_iter()
        .map(|record| (record.key.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeMap::new();
    while let Some(key) = mandatory.pop_first() {
        if selected.contains_key(&key) {
            continue;
        }
        let record = records
            .get(&key)
            .ok_or_else(|| WorkSnapshotError::NeedsPreparation(key.clone()))?;
        mandatory.extend(record.record.references());
        selected.insert(key, record.clone());
    }
    owning_input_roots_of_records(resident, &selected.into_values().collect::<Vec<_>>())
}

fn owning_input_roots_of_records(
    resident: &ResidentWorkDomain,
    records: &[Arc<QualifiedWorkRecord>],
) -> Result<BTreeSet<InputEvidenceKey>> {
    let mut roots = BTreeSet::new();
    for record in records {
        match &record.record {
            CanonicalWorkRecord::Work(work) => {
                if let Some(heads) = resident.coverage().legacy_delivery_heads.get(&work.id) {
                    for id in [heads.latest_any.body_id(), heads.latest_nonempty.body_id()]
                        .into_iter()
                        .flatten()
                    {
                        roots.insert(InputEvidenceKey::new(
                            InputEvidenceFamily::LegacyDelivery,
                            id,
                        ));
                    }
                }
                let completion_head = resident
                    .coverage()
                    .latest_results
                    .get(&work.id)
                    .and_then(Option::as_ref)
                    .map(|head| head.brief_id.as_str());
                // Both bound result fields describe this one completion edge.
                // Independent intent/source roots remain strict below.
                if let Some(id) = &work.result_brief_id {
                    if Some(id.as_str()) != completion_head {
                        roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, id));
                    }
                }
                if let Some(intent) = &work.completion_intent {
                    if let Some(id) = &intent.result_brief_id {
                        if Some(id.as_str()) != completion_head {
                            roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, id));
                        }
                    }
                    if let Some(id) = &intent.source_message_id {
                        roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
                    }
                    if let Some(id) = &intent.source_turn_id {
                        roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, id));
                    }
                }
            }
            CanonicalWorkRecord::Wait(wait) => {
                if let Some(id) = &wait.trigger_message_id {
                    roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
                }
            }
            CanonicalWorkRecord::Frame(_) => {}
        }
    }
    Ok(roots)
}

pub(super) fn current_coverage(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<Option<WorkCoverage>> {
    let row:Option<(u64,usize)>=tx.query_row("SELECT revision,length(CAST(coverage_json AS BLOB)) FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((revision, bytes)) = row else {
        return Ok(None);
    };
    bound_metadata(0, bytes)?;
    let json:String=tx.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    let coverage: WorkCoverage = serde_json::from_str(&json)?;
    ensure!(
        coverage.revision == revision,
        "Work coverage revision projection mismatch"
    );
    Ok(Some(coverage))
}
pub(super) fn persist_coverage(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    coverage: &WorkCoverage,
) -> Result<()> {
    let json = serde_json::to_string(coverage)?;
    bound_metadata(coverage.observations.len(), json.len())?;
    tx.execute("INSERT INTO work_domain_current(agent_id,identity_incarnation,revision,coverage_json)VALUES(?1,?2,?3,?4)ON CONFLICT(agent_id,identity_incarnation)DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",params![scope.agent_id,scope.identity_incarnation,coverage.revision,json])?;
    Ok(())
}
pub(crate) fn validate_work_domain_observation_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    revision: u64,
) -> Result<()> {
    ensure!(
        scope_facts(tx, scope, false)?.is_some(),
        "Work observation scope is retired"
    );
    let _ = binding(tx, scope)?;
    let current = current_coverage(tx, scope)?.context("Work observation was not prepared")?;
    if current.revision != revision {
        return Err(WorkSnapshotError::RevisionMismatch {
            expected: revision,
            actual: current.revision,
        }
        .into());
    }
    Ok(())
}

fn namespace_ids(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    family: WorkFamily,
    predicate: &str,
    limit: Option<usize>,
    material: Option<&mut WorkReadMaterial<'_>>,
) -> Result<Vec<String>> {
    let (table, id) = family_table(family);
    let sql=format!("SELECT c.{id} AS id FROM {table} c WHERE c.agent_id=?1 AND ({predicate}) AND (EXISTS(SELECT 1 FROM work_domain_records a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=c.{id} AND a.availability='present') OR (NOT EXISTS(SELECT 1 FROM work_domain_records a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=c.{id}) AND EXISTS(SELECT 1 FROM work_domain_eligibility e WHERE e.seed_id=?4 AND e.family=?3 AND e.record_id=c.{id}))) ORDER BY {} LIMIT ?5",if limit.is_some(){"c.updated_at DESC,c.created_at DESC,c.work_item_id ASC".to_owned()}else{"c.".to_owned()+id+" ASC"});
    if let Some(material) = material {
        // Admission of SQLite's finite sorting scratch precedes ORDER BY. The
        // scalar scans the original qualified namespace; a historical pool is
        // not misrepresented as simultaneously hydrated resident bodies.
        let pool = sql
            .split_once(" ORDER BY ")
            .context("Work namespace order absent")?
            .0;
        let sort_bytes = if limit.is_some() {
            format!("length(CAST(c.{id} AS BLOB))+length(CAST(c.updated_at AS BLOB))+length(CAST(c.created_at AS BLOB))")
        } else {
            format!("length(CAST(c.{id} AS BLOB))")
        };
        let pool = pool.replacen(
            &format!("SELECT c.{id} AS id"),
            &format!("SELECT {sort_bytes} AS n"),
            1,
        );
        let (pool_count, max_bytes): (usize, usize) = tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(MAX(n),0) FROM ({pool})"),
            params![
                scope.agent_id,
                scope.identity_incarnation,
                family_name(family),
                seed
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let slots = pool_count.min(limit.unwrap_or(MAX_METADATA_RECORDS + 1));
        material.raw(
            max_bytes
                .checked_mul(slots)
                .context("Work sort material overflow")?,
            slots,
        )?;
        material.ids_query(
            tx,
            &sql,
            params![
                scope.agent_id,
                scope.identity_incarnation,
                family_name(family),
                seed,
                i64::try_from(limit.unwrap_or(MAX_METADATA_RECORDS + 1))?
            ],
        )?;
    }
    let mut query = tx.prepare(&sql)?;
    let ids = query
        .query_map(
            params![
                scope.agent_id,
                scope.identity_incarnation,
                family_name(family),
                seed,
                i64::try_from(limit.unwrap_or(MAX_METADATA_RECORDS + 1))?
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    bound_metadata(ids.len(), ids.iter().map(String::len).sum())?;
    Ok(ids)
}

pub(super) fn result_head_on(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    work: &str,
) -> Result<Option<Option<WorkCompletionRef>>> {
    let bytes:Option<usize>=tx.query_row("SELECT length(CAST(head_json AS BLOB)) FROM work_domain_result_heads WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3",params![scope.agent_id,scope.identity_incarnation,work],|row|row.get(0)).optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    bound_metadata(1, bytes)?;
    let json:String=tx.query_row("SELECT head_json FROM work_domain_result_heads WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3",params![scope.agent_id,scope.identity_incarnation,work],|row|row.get(0))?;
    Ok(Some(serde_json::from_str(&json)?))
}
pub(super) fn persist_result_head(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    work: &str,
    revision: u64,
    head: &Option<WorkCompletionRef>,
) -> Result<()> {
    if result_head_on(tx, scope, work)?.as_ref() == Some(head) {
        return Ok(());
    }
    let json = serde_json::to_string(head)?;
    bound_metadata(1, json.len().saturating_add(work.len()))?;
    tx.execute("INSERT INTO work_domain_result_heads(agent_id,identity_incarnation,work_item_id,revision,head_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(agent_id,identity_incarnation,work_item_id) DO UPDATE SET revision=excluded.revision,head_json=excluded.head_json",params![scope.agent_id,scope.identity_incarnation,work,revision,json])?;
    Ok(())
}
