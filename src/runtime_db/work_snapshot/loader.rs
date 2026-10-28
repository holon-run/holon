use super::*;
use qualification::{eligible_digest, manifests, validate_member};

#[cfg(test)]
pub(crate) fn load_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, false, false, false)
}
pub(crate) fn prepare_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, true, false, false)
}
/// The caller must hold the same original admitted-cleanup owner reservation;
/// this entry never authorizes ordinary activation of a Deleting identity.
pub(crate) fn prepare_original_work_recovery_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, true, true, false)
}

/// Same finite qualification and body budget checks, without sidecar mutation.
#[cfg(test)]
pub(crate) fn preflight_work_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    load(tx, scope, request, false, false, true)
}
pub(super) fn preflight_original_work_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
    decoded_state: Option<&super::super::context_inputs::ContextStateDecode>,
) -> Result<WorkBootstrapSnapshot> {
    let read =
        ContextDurableRead::new(tx, &scope.agent_id).scope_read_using_decode(decoded_state)?;
    load_with_scope_read(
        tx,
        scope,
        request,
        false,
        original_recovery,
        true,
        Some(read),
    )
}

/// Reuse only the caller's validated, unchanged transaction read.
pub(crate) fn preflight_named_work_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    read: &super::super::context_inputs::ContextScopeRead<'_>,
) -> Result<WorkBootstrapSnapshot> {
    read.validate(tx, scope)?;
    let facts = read.facts();
    crate::diagnostics::resource_io::admit_cloned_value(&(
        &facts.identity,
        facts.control_revision,
    ))?;
    let facts = facts.clone();
    let facts = scope_is_current(tx, scope, &facts, false)?.then_some(facts);
    load_with_scope_facts(tx, scope, request, false, true, facts)
}

fn load(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
) -> Result<WorkBootstrapSnapshot> {
    load_with_scope_read(
        tx,
        scope,
        request,
        prepare,
        original_recovery,
        preflight,
        None,
    )
}

pub(super) fn load_with_scope_read(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    prepare: bool,
    original_recovery: bool,
    preflight: bool,
    read: Option<super::super::context_inputs::ContextScopeRead<'_>>,
) -> Result<WorkBootstrapSnapshot> {
    let facts = match read {
        Some(read) => {
            let facts = read.into_facts(tx, scope)?;
            scope_is_current(tx, scope, &facts, original_recovery)?.then_some(facts)
        }
        None => scope_facts(tx, scope, original_recovery)?,
    };
    load_with_scope_facts(tx, scope, request, prepare, preflight, facts)
}

fn load_with_scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    prepare: bool,
    preflight: bool,
    facts: Option<ContextScopeFacts>,
) -> Result<WorkBootstrapSnapshot> {
    let Some(facts) = facts else {
        return Ok(WorkBootstrapSnapshot::Retired {
            scope: scope.clone(),
        });
    };
    ensure!(
        request.capture.nonopen_limit <= WORK_NONOPEN_WINDOW,
        "Work history request exceeds fixed maximum"
    );
    let seed = binding(tx, scope)?;
    let previous = current_coverage(tx, scope)?;
    let revision = previous.as_ref().map_or(1, |coverage| coverage.revision);
    let fixed = manifests(tx, seed.as_deref())?;
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
        heads.extend(previous.latest_results.clone());
    }
    let live_work_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Work,
        "c.state IN ('open','completing')",
        None,
    )?;
    let unresolved_wait_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Wait,
        "c.status IN ('active','triggered')",
        None,
    )?;
    let active_frame_ids = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Frame,
        "c.state='active'",
        None,
    )?;
    let candidates = namespace_ids(
        tx,
        scope,
        seed.as_deref(),
        WorkFamily::Work,
        "c.state!='open'",
        Some(request.capture.nonopen_limit + 1),
    )?;
    // namespace_ids uses the canonical history order when a limit is provided.
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
        super::relations::load_wait_relations_tx(tx, scope, &facts, request, &mut required)?;
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
        let alias = alias(tx, scope, &key)?;
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
            ensure!(
                eligible_digest(tx, seed.as_deref(), &key)?.is_none()
                    && !qualification::known_anywhere(tx, &key)?,
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
        let stored = stored_on(tx, &scope.agent_id, &key)?
            .context("Work body disappeared in read transaction")?;
        let record_revision = validate_member(tx, scope, seed.as_deref(), &key, &stored)?;
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
        if let Some(head) = result_head_on(tx, scope, work)? {
            heads.insert(work.clone(), head);
        }
    }
    if prepare || preflight {
        for work in &request.capture.latest_result_work_ids {
            if let Some(Some(head)) = heads.get(work) {
                let refreshed =
                    super::completion::completion_head_refs_tx(tx, scope, &head.brief_id)?;
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
        turn_wait_keys,
        task_wait_keys,
        trigger_wait_keys,
    };
    if let Some(previous) = &previous {
        let mut prior = coverage.clone();
        prior.revision = previous.revision;
        if &prior != previous {
            coverage.revision = previous
                .revision
                .checked_add(1)
                .context("Work revision overflow")?;
        }
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
    let owning_input_roots = owning_input_roots(&resident)?;
    Ok(WorkBootstrapSnapshot::Current(Box::new(
        WorkCurrentSnapshot {
            facts,
            resident,
            owning_input_roots,
        },
    )))
}

pub(super) fn owning_input_roots(
    resident: &ResidentWorkDomain,
) -> Result<BTreeSet<InputEvidenceKey>> {
    let captured = resident.capture(&WorkCaptureRequest {
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
    })?;
    owning_input_roots_of_records(resident, &captured.records)
}

/// Receipt-only historical Work bodies are selectors, not current Input roots.
/// This bridge retains all live owning closure plus the current State Work.
pub(crate) fn current_recovery_owning_input_roots(
    resident: &ResidentWorkDomain,
    state: &AgentState,
) -> Result<BTreeSet<InputEvidenceKey>> {
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
    let captured = resident.capture(&WorkCaptureRequest {
        nonopen_limit: 0,
        mandatory_roots: mandatory.clone(),
        ..WorkCaptureRequest::current()
    })?;
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
    validate_observation_revision(tx, scope, revision)
}

pub(crate) fn validate_work_domain_observation_with_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    revision: u64,
    read: &super::super::context_inputs::ContextScopeRead<'_>,
) -> Result<()> {
    read.validate(tx, scope)?;
    ensure!(
        read.facts()
            .identity
            .as_ref()
            .is_some_and(|identity| identity.status == AgentRegistryStatus::Active),
        "Work observation scope is retired"
    );
    validate_observation_revision(tx, scope, revision)
}

fn validate_observation_revision(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    revision: u64,
) -> Result<()> {
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
) -> Result<Vec<String>> {
    let (table, id) = family_table(family);
    let sql=format!("SELECT c.{id} FROM {table} c WHERE c.agent_id=?1 AND ({predicate}) AND (EXISTS(SELECT 1 FROM work_domain_records a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=c.{id} AND a.availability='present') OR (NOT EXISTS(SELECT 1 FROM work_domain_records a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=c.{id}) AND EXISTS(SELECT 1 FROM work_domain_eligibility e WHERE e.seed_id=?4 AND e.family=?3 AND e.record_id=c.{id}))) ORDER BY {} LIMIT ?5",if limit.is_some(){"c.updated_at DESC,c.created_at DESC,c.work_item_id ASC".to_owned()}else{"c.".to_owned()+id+" ASC"});
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
