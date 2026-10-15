use super::*;
/// New explicit preparation may install qualified/fixed facts, never a raw tail.
pub(crate) fn prepare_current_read_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<CurrentReadSnapshot> {
    let mut material = Material::new(scope, request, ledger);
    load(tx, scope, state, request, &mut material, None, false)
}
pub(crate) fn prepare_original_current_read_recovery_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<CurrentReadSnapshot> {
    let mut material = Material::new(scope, request, ledger);
    load(tx, scope, state, request, &mut material, None, true)
}
/// The enclosing original Input actor owns the remaining phase and retains
/// both domains' real guards until its install. No second budget is created.
pub(in crate::runtime_db) fn prepare_current_read_using_input_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    input: &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
    original: bool,
) -> Result<CurrentReadSnapshot> {
    input.validate_current_allowance(scope, ledger, request)?;
    let mut material = Material::new(scope, request, ledger);
    let snapshot = load(tx, scope, state, request, &mut material, None, original)?;
    input.observe_current_prior(&ObservedCurrentReadMaterial {
        scope,
        ledger,
        usage: (material.bytes, material.records),
        guards: &snapshot._material,
    })?;
    Ok(snapshot)
}
pub(super) fn scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    material: &mut Material<'_>,
    original: bool,
) -> Result<(AgentIdentityRecord, u64)> {
    ensure!(
        scope.identity_incarnation > 0 && state.id == scope.agent_id,
        CurrentReadError::RetiredScope
    );
    let (identity, actual, control) = actual_scope_facts(tx, scope, material, original)?;
    ensure!(&actual == state, CurrentReadError::CanonicalChanged);
    Ok((identity, control))
}
pub(super) fn actual_scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_>,
    original: bool,
) -> Result<(AgentIdentityRecord, AgentState, u64)> {
    ensure!(
        scope.identity_incarnation > 0,
        CurrentReadError::RetiredScope
    );
    let size:Option<usize>=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+256 FROM agent_identities WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0)).optional()?;
    material.charge(size.ok_or(CurrentReadError::RetiredScope)?, 1)?;
    material.native(
        tx,
        "agent_identities",
        "payload_json",
        "agent_id",
        &scope.agent_id,
    )?;
    let (status, raw): (String, String) = tx.query_row(
        "SELECT status,payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let identity = repositories::decode_agent_identity_payload(&raw)?;
    ensure!(
        identity.agent_id == scope.agent_id
            && serde_json::to_value(identity.status)?.as_str() == Some(status.as_str()),
        CurrentReadError::CanonicalChanged
    );
    // A stale incarnation is rejected before any successor State is touched.
    ensure!(
        identity.incarnation == scope.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original && identity.status == AgentRegistryStatus::Deleting)),
        CurrentReadError::RetiredScope
    );
    let size:Option<usize>=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(control_revision AS BLOB))+256 FROM agent_states WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0)).optional()?;
    material.charge(size.ok_or(CurrentReadError::RetiredScope)?, 1)?;
    material.native(
        tx,
        "agent_states",
        "payload_json",
        "agent_id",
        &scope.agent_id,
    )?;
    // Identity was already bounded and validated above. Read only this State;
    // a generic scope adapter would decode a second simultaneous identity tree.
    let (state_raw, control_revision): (String, i64) = tx.query_row(
        "SELECT payload_json,control_revision FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let actual_state = repositories::decode_agent_state_payload(&state_raw)?;
    ensure!(
        actual_state.id == scope.agent_id,
        CurrentReadError::CanonicalChanged
    );
    Ok((identity, actual_state, u64::try_from(control_revision)?))
}
pub(super) struct SavedCoverage {
    pub coverage: CurrentReadCoverage,
    pub binding_invalidated: bool,
    pub records_invalidated: bool,
}
impl std::ops::Deref for SavedCoverage {
    type Target = CurrentReadCoverage;
    fn deref(&self) -> &Self::Target {
        &self.coverage
    }
}
pub(super) fn saved(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_>,
) -> Result<Option<SavedCoverage>> {
    let size:Option<usize>=tx.query_row("SELECT length(CAST(coverage_json AS BLOB))+length(CAST(revision AS BLOB))+length(CAST(freshness AS BLOB))+128 FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0)).optional()?;
    let Some(size) = size else { return Ok(None) };
    material.charge(size, 1)?;
    let nodes:usize=tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM current_read_current c,json_tree(c.coverage_json) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    material.charge_native(nodes)?;
    let (revision, raw, valid, binding_invalidated, records_invalidated): (u64, String, bool, bool, bool) = tx.query_row(
        "SELECT revision,coverage_json,freshness IN('current','binding_invalidated','records_invalidated'),freshness='binding_invalidated',freshness='records_invalidated' FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    ensure!(valid, CurrentReadError::CanonicalChanged);
    let coverage: CurrentReadCoverage = serde_json::from_str(&raw)?;
    ensure!(
        coverage.revision == revision && revision > 0,
        CurrentReadError::CanonicalChanged
    );
    ensure!(
        [&coverage.state_digest, &coverage.identity_digest]
            .into_iter()
            .all(|d| d.len() == 64
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        CurrentReadError::CanonicalChanged
    );
    coverage.event_query.validate()?;
    // A sealed original maintenance ACK may keep metadata while its canonical
    // namespace is incomplete. Only explicit preparation can rebuild bodies.
    ensure!(
        coverage.queue_complete || records_invalidated,
        CurrentReadError::NeedsPreparation
    );
    ensure!(
        coverage.queue_counts.len() == 8
            && [
                "queued",
                "dequeued",
                "processed",
                "interjected",
                "interrupted",
                "aborted",
                "dropped",
                "quarantined"
            ]
            .iter()
            .all(|s| coverage.queue_counts.contains_key(*s)),
        CurrentReadError::CanonicalChanged
    );
    let pending = ["queued", "dequeued", "interjected", "interrupted"]
        .iter()
        .try_fold(0u64, |n, s| {
            n.checked_add(coverage.queue_counts[*s])
                .context("CurrentRead saved pending count overflow")
        })?;
    ensure!(
        pending == coverage.pending_queue.len() as u64
            && coverage.event_body_limit <= coverage.event_header_limit
            && coverage.event_header_limit <= CURRENT_EVENT_WINDOW
            && coverage.events.len() <= coverage.event_header_limit
            && coverage.terminal_queue_limit <= CURRENT_QUEUE_HISTORY
            && coverage.terminal_queue.len() <= coverage.terminal_queue_limit,
        CurrentReadError::CanonicalChanged
    );
    let mut seen = BTreeSet::new();
    for observation in &coverage.observations {
        ensure!(
            seen.insert(&observation.key),
            CurrentReadError::CanonicalChanged
        );
    }
    for key in coverage
        .pending_queue
        .iter()
        .chain(&coverage.terminal_queue)
        .chain(&coverage.events)
    {
        ensure!(
            coverage
                .observations
                .iter()
                .any(|o| &o.key == key && o.availability == CurrentReadAvailability::Present),
            CurrentReadError::CanonicalChanged
        );
    }
    Ok(Some(SavedCoverage {
        coverage,
        binding_invalidated,
        records_invalidated,
    }))
}
/// This closed union freezes selection to current members or the original seed.
/// Global canonical rows never supply an optional history selector.
pub(super) fn heads() -> &'static str {
    "SELECT m.family,m.record_id,m.revision,m.availability,m.content_digest,m.projection_digest,m.status,m.sort_seq,m.sort_at,m.created_at
     FROM current_read_members m WHERE m.agent_id=?1 AND m.identity_incarnation=?2
     UNION ALL
     SELECT f.family,f.record_id,1,'present',f.content_digest,f.projection_digest,f.status,f.sort_seq,f.sort_at,f.created_at
     FROM current_read_fixed_members f
     JOIN current_read_fixed_captures fc ON fc.seed_id=f.seed_id AND fc.capture_phase=f.capture_phase AND fc.sealed=1
     JOIN agent_episode_continuity c ON c.seed_id=f.seed_id AND c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND c.mode='legacy_adopted'
     JOIN episode_continuity_seeds seed ON seed.seed_id=f.seed_id AND seed.agent_id=?1 AND (seed.identity_incarnation IS NULL OR seed.identity_incarnation=?2)
     WHERE f.agent_id=?1
       AND NOT EXISTS(SELECT 1 FROM current_read_members m WHERE m.agent_id=?1 AND m.identity_incarnation=?2 AND m.family=f.family AND m.record_id=f.record_id)
       AND NOT EXISTS(SELECT 1 FROM current_read_fixed_members later WHERE later.seed_id=f.seed_id AND later.family=f.family AND later.record_id=f.record_id
         AND CASE later.capture_phase WHEN 'first_import' THEN 3 WHEN 'legacy_import' THEN 2 ELSE 1 END
           > CASE f.capture_phase WHEN 'first_import' THEN 3 WHEN 'legacy_import' THEN 2 ELSE 1 END)"
}
pub(super) fn eligible(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &CurrentReadKey,
    material: &mut Material<'_>,
) -> Result<Option<(u64, CurrentReadAvailability, String, String)>> {
    let h = heads();
    let size:Option<usize>=tx.query_row(&format!("SELECT length(CAST(record_id AS BLOB))+length(CAST(revision AS BLOB))+length(CAST(availability AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0)+256 FROM ({h}) WHERE family=?3 AND record_id=?4"),params![scope.agent_id,scope.identity_incarnation,family(key.family),key.id],|r|r.get(0)).optional()?;
    let Some(size) = size else { return Ok(None) };
    material.charge(size, 8)?;
    let (revision,availability,content,projection):(u64,String,Option<String>,Option<String>)=tx.query_row(&format!("SELECT revision,availability,content_digest,projection_digest FROM ({h}) WHERE family=?3 AND record_id=?4"),params![scope.agent_id,scope.identity_incarnation,family(key.family),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    match availability.as_str() {
        "present" => {
            let content = content.context("CurrentRead content digest missing")?;
            let projection = projection.context("CurrentRead projection digest missing")?;
            ensure!(
                content.len() == 64 && projection.len() == 64,
                CurrentReadError::CanonicalChanged
            );
            Ok(Some((
                revision,
                CurrentReadAvailability::Present,
                content,
                projection,
            )))
        }
        "pruned" => {
            ensure!(
                content.is_none() && projection.is_none(),
                CurrentReadError::CanonicalChanged
            );
            Ok(Some((
                revision,
                CurrentReadAvailability::Pruned,
                String::new(),
                String::new(),
            )))
        }
        _ => Err(CurrentReadError::CanonicalChanged.into()),
    }
}
fn chosen_ids(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    f: CurrentReadFamily,
    predicate: &str,
    limit: Option<usize>,
    material: &mut Material<'_>,
) -> Result<Vec<CurrentReadKey>> {
    let h = heads();
    let limit = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
    let order = if f == CurrentReadFamily::Queue {
        "sort_at DESC,created_at ASC,record_id ASC"
    } else {
        "sort_seq DESC,sort_at DESC,record_id ASC"
    };
    let selected=format!("SELECT record_id FROM ({h}) WHERE family=?3 AND availability='present' AND {predicate} ORDER BY {order}{limit}");
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(record_id AS BLOB))+64),0) FROM ({selected})"
        ),
        params![scope.agent_id, scope.identity_incarnation, family(f)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    material.charge(bytes, count)?;
    let mut statement = tx.prepare(&selected)?;
    let ids = statement
        .query_map(
            params![scope.agent_id, scope.identity_incarnation, family(f)],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids
        .into_iter()
        .map(|id| CurrentReadKey { family: f, id })
        .collect())
}
pub(super) fn load(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    material: &mut Material<'_>,
    final_revision: Option<u64>,
    original: bool,
) -> Result<CurrentReadSnapshot> {
    request.validate()?;
    super::record_invalidation::install_tx(tx)?;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, request))?)?,
        request.required_queue_ids.len() + 8,
    )?;
    let (identity, control_revision) = scope_facts(tx, scope, state, material, original)?;
    // Binding is an original fixed qualification fact, not evidence inferred
    // from empty canonical tables or a caller-supplied incarnation.
    let bound: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND ((c.mode='fresh_empty' AND c.seed_id IS NULL) OR (c.mode='legacy_adopted' AND EXISTS(SELECT 1 FROM episode_continuity_seeds s WHERE s.seed_id=c.seed_id AND s.agent_id=c.agent_id AND (s.identity_incarnation IS NULL OR s.identity_incarnation=c.identity_incarnation)))))", params![scope.agent_id,scope.identity_incarnation], |r| r.get(0))?;
    ensure!(bound, CurrentReadError::NeedsPreparation);
    let h = heads();
    for id in &request.required_queue_ids {
        let pruned: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM ({h}) WHERE family='queue' AND record_id=?3 AND availability='pruned')"), params![scope.agent_id,scope.identity_incarnation,id], |r| r.get(0))?;
        ensure!(!pruned, CurrentReadError::Pruned(CurrentReadKey::queue(id)));
    }
    let pruned_pending:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM ({h}) WHERE family='queue' AND availability='pruned' AND status IN('queued','dequeued','interjected','interrupted'))"),params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    ensure!(!pruned_pending, CurrentReadError::NeedsPreparation);
    let invalid_status: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM ({h}) WHERE family='queue' AND availability='present' AND status NOT IN('queued','dequeued','processed','interjected','interrupted','aborted','dropped','quarantined'))"), params![scope.agent_id,scope.identity_incarnation], |r| r.get(0))?;
    ensure!(!invalid_status, CurrentReadError::CanonicalChanged);
    let tables_ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='queue_entries') AND EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='audit_events')",[],|r|r.get(0))?;
    ensure!(tables_ready, CurrentReadError::HistoricalUnavailable);
    let unavailable_fixed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN current_read_fixed_captures f ON f.seed_id=c.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND f.sealed=1 AND (f.queue_available=0 OR f.event_available=0) AND NOT EXISTS(SELECT 1 FROM current_read_fixed_captures imported WHERE imported.seed_id=f.seed_id AND imported.capture_phase='first_import' AND imported.sealed=1 AND imported.queue_available=1 AND imported.event_available=1))",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;

    let pending_import:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND s.import_required=1 AND NOT EXISTS(SELECT 1 FROM current_read_fixed_captures f WHERE f.seed_id=s.seed_id AND f.capture_phase='first_import' AND f.sealed=1))",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    ensure!(!pending_import, CurrentReadError::NeedsPreparation);
    let unknown:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM queue_entries q WHERE q.agent_id=?1 AND NOT EXISTS(SELECT 1 FROM ({h}) known WHERE known.family='queue' AND known.record_id=q.message_id))"),params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    let previous = saved(tx, scope, material)?;
    let event_query = event_queries::load_tx(tx, scope, request.event_birth_after, material)?;
    let state_digest = typed_digest(state)?;
    let identity_digest = typed_digest(&identity)?;
    if final_revision.is_none() {
        if let Some(previous) = &previous {
            ensure!(
                previous.binding_invalidated
                    || previous.records_invalidated
                    || (previous.state_digest == state_digest
                        && previous.identity_digest == identity_digest),
                CurrentReadError::CanonicalChanged
            );
        }
    }
    let prepared_revision = match &previous {
        Some(saved)
            if (saved.binding_invalidated || saved.records_invalidated)
                && final_revision.is_none() =>
        {
            saved
                .revision
                .checked_add(1)
                .context("CurrentRead invalidated revision overflow")?
        }
        Some(saved) => saved.revision,
        None => 1,
    };
    let revision = final_revision.unwrap_or(prepared_revision);
    // Explicit preparation may expand a known role. Actual ACK maintenance uses
    // the original live-role snapshot; ordinary capture cannot expand or use SQL.
    let event_body_limit = if final_revision.is_some() {
        request.event_limit
    } else {
        request
            .event_limit
            .max(previous.as_ref().map_or(0, |c| c.event_body_limit))
    };
    let terminal_queue_limit = request
        .terminal_queue_limit
        .max(previous.as_ref().map_or(0, |c| c.terminal_queue_limit));
    let event_header_limit = if final_revision.is_some() {
        request.event_count_limit.max(event_body_limit)
    } else {
        request
            .event_count_limit
            .max(event_body_limit)
            .max(previous.as_ref().map_or(0, |c| c.event_header_limit))
    };
    let pending = chosen_ids(
        tx,
        scope,
        CurrentReadFamily::Queue,
        "status IN ('queued','dequeued','interjected','interrupted')",
        None,
        material,
    )?;
    let terminal = chosen_ids(
        tx,
        scope,
        CurrentReadFamily::Queue,
        "status NOT IN ('queued','dequeued','interjected','interrupted')",
        Some(terminal_queue_limit),
        material,
    )?;
    let events = chosen_ids(
        tx,
        scope,
        CurrentReadFamily::Event,
        "1",
        Some(event_header_limit),
        material,
    )?;
    let mut observations = BTreeMap::new();
    let mut keys = BTreeSet::new();
    material.charge(
        usize::try_from(serialized_bytes(&(&pending, &terminal, &events))?)?,
        pending.len() + terminal.len() + events.len(),
    )?;
    keys.extend(
        pending
            .iter()
            .chain(&terminal)
            .chain(events.iter().take(event_body_limit))
            .cloned(),
    );
    // Header-only observations grant no body material or authority.
    for key in events.iter().skip(event_body_limit) {
        observations.insert(key.clone(), CurrentReadAvailability::Present);
    }
    for id in &request.required_queue_ids {
        keys.insert(CurrentReadKey::queue(id));
    }
    material.charge(
        usize::try_from(serialized_bytes(&event_query)?)?,
        event_query
            .born_event_seqs
            .len()
            .checked_mul(3)
            .context("Event query key count overflow")?,
    )?;
    keys.extend(
        event_query
            .born_event_seqs
            .keys()
            .map(CurrentReadKey::event),
    );
    let mut records = Vec::new();
    for key in keys {
        let mark = material.selected_mark();
        let qualified = {
            if let Some((member_revision, availability, content, projection)) =
                eligible(tx, scope, &key, material)?
            {
                ensure!(
                    availability != CurrentReadAvailability::Pruned
                        || key.family != CurrentReadFamily::Queue
                        || !request.required_queue_ids.contains(&key.id),
                    CurrentReadError::Pruned(key.clone())
                );
                observations.insert(key.clone(), availability);
                if availability == CurrentReadAvailability::Present {
                    let record = stored::read(tx, &key, &scope.agent_id, material)?
                        .ok_or(CurrentReadError::CanonicalChanged)?;
                    ensure!(
                        record.content == content && record.projection == projection,
                        CurrentReadError::CanonicalChanged
                    );
                    if key.family == CurrentReadFamily::Event {
                        if let Some(seq) = event_query.born_event_seqs.get(&key.id) {
                            ensure!(record.seq == *seq, CurrentReadError::CanonicalChanged);
                        }
                    }
                    Some(record.qualified(scope, member_revision)?)
                } else {
                    None
                }
            } else {
                ensure!(
                    key.family != CurrentReadFamily::Event
                        || !event_query.born_event_seqs.contains_key(&key.id),
                    CurrentReadError::NeedsPreparation
                );
                let (table, id, _, _) = stored::source(key.family);
                let canonical: bool = tx.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1)"),
                    [&key.id],
                    |r| r.get(0),
                )?;
                let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_members WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM current_read_fixed_members WHERE family=?1 AND record_id=?2)",params![family(key.family),key.id],|r|r.get(0))?;
                ensure!(!canonical && !known, CurrentReadError::NeedsPreparation);
                observations.insert(key, CurrentReadAvailability::Absent);
                None
            }
        };
        material.finish_selected_record(mark, qualified.as_ref())?;
        if let Some(record) = qualified {
            records.push(record);
        }
    }
    let event_count: u64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM ({h}) WHERE family='event' AND availability='present'"),
        params![scope.agent_id, scope.identity_incarnation],
        |r| r.get(0),
    )?;
    let mut counts = BTreeMap::new();
    for status in [
        "queued",
        "dequeued",
        "processed",
        "interjected",
        "interrupted",
        "aborted",
        "dropped",
        "quarantined",
    ] {
        let count:u64=tx.query_row(&format!("SELECT COUNT(*) FROM ({h}) WHERE family='queue' AND availability='present' AND status=?3"),params![scope.agent_id,scope.identity_incarnation,status],|r|r.get(0))?;
        counts.insert(status.to_owned(), count);
    }
    let coverage = CurrentReadCoverage {
        revision,
        state_digest,
        identity_digest,
        queue_complete: true,
        queue_counts: counts,
        pending_queue: pending.into_iter().collect(),
        terminal_queue: terminal,
        terminal_queue_limit,
        events,
        events_exhaustive: event_count <= event_header_limit as u64,
        event_body_limit,
        event_header_limit,
        event_query,
        observations: observations
            .into_iter()
            .map(|(key, availability)| CurrentReadObservation { key, availability })
            .collect(),
    };
    if final_revision.is_none() {
        if let Some(previous) = previous.as_ref().filter(|previous| previous.queue_complete) {
            ensure!(
                previous.pending_queue == coverage.pending_queue
                    && previous.terminal_queue.as_slice()
                        == &coverage.terminal_queue[..previous
                            .terminal_queue
                            .len()
                            .min(coverage.terminal_queue.len())]
                    && previous.events.as_slice()
                        == &coverage.events[..previous.events.len().min(coverage.events.len())]
                    && (!previous.events_exhaustive
                        || previous.events.len() == coverage.events.len())
                    && previous.queue_counts == coverage.queue_counts,
                CurrentReadError::CanonicalChanged
            );
        }
    }
    // A missing fixed family is an honest incomplete domain, but cannot mask
    // corruption in this scope's independently accepted actual members.
    ensure!(!unavailable_fixed, CurrentReadError::HistoricalUnavailable);
    // Unknown raw rows never authorize bodies. Validate all requested accepted
    // members first, so sparse metadata cannot conceal Pruned/corrupt material.
    ensure!(
        !unknown,
        CurrentReadError::UnknownNamespace(CurrentReadFamily::Queue)
    );
    material.charge(
        usize::try_from(serialized_bytes(&coverage)?)?,
        coverage.observations.len() + 32,
    )?;
    tx.execute("INSERT INTO current_read_current(agent_id,identity_incarnation,revision,coverage_json,freshness) VALUES(?1,?2,?3,?4,'current') ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json,freshness='current' ",params![scope.agent_id,scope.identity_incarnation,revision,serde_json::to_string(&coverage)?])?;
    let resident = ResidentCurrentReadFacts::bootstrap(
        scope.clone(),
        &identity,
        coverage,
        records,
        material.ledger.clone(),
        request,
    )?;
    Ok(CurrentReadSnapshot {
        resident: Arc::new(resident),
        control_revision,
        _material: std::mem::take(&mut material.guards),
    })
}
