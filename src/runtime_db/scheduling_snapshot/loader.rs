use super::*;

pub(crate) fn prepare_scheduling_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<SchedulingBootstrapSnapshot> {
    load(tx, scope, request, ledger, true, false)
}
/// Existing original-scope positive recovery only; this grants no business admission.
pub(crate) fn prepare_original_scheduling_recovery_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<SchedulingBootstrapSnapshot> {
    load(tx, scope, request, ledger, true, true)
}
#[cfg(test)]
pub(crate) fn load_scheduling_domain_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<SchedulingBootstrapSnapshot> {
    load(tx, scope, request, ledger, false, false)
}
pub(super) fn scope_identity(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
    original_recovery: bool,
) -> Result<Option<crate::types::AgentIdentityRecord>> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Scheduling scope"
    );
    let bytes:Option<usize>=tx.query_row("SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0)).optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    material.charge(bytes, 1)?;
    let (id, status, raw): (String, String, String) = tx.query_row(
        "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let identity = repositories::decode_agent_identity_payload(&raw)?;
    ensure!(
        identity.agent_id == id
            && id == scope.agent_id
            && identity.incarnation > 0
            && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
        "Scheduling identity projection differs"
    );
    if identity.incarnation != scope.identity_incarnation
        || !(identity.status == AgentRegistryStatus::Active
            || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    {
        return Ok(None);
    }
    Ok(Some(identity))
}

pub(super) fn scope_facts(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
    original_recovery: bool,
) -> Result<Option<ContextScopeFacts>> {
    let Some(identity) = scope_identity(tx, scope, material, original_recovery)? else {
        return Ok(None);
    };
    let bytes: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(payload_json AS BLOB))+256 FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    material.charge(bytes, 1)?;
    // The raw guard precedes SQLite JSON traversal; the node guard precedes
    // WorkingMemory/State collection allocation in the canonical decoder.
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree((SELECT payload_json FROM agent_states WHERE agent_id=?1))",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    material.charge(
        nodes
            .checked_mul(std::mem::size_of::<serde_json::Value>())
            .context("Scheduling State node bytes overflow")?,
        nodes,
    )?;
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    ensure!(
        facts.identity.as_ref() == Some(&identity)
            && facts.state.as_ref().is_some_and(|s| s.id == scope.agent_id),
        "Scheduling canonical scope/State differs"
    );
    Ok(Some(facts))
}
pub(super) fn current(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
) -> Result<Option<SchedulingCoverage>> {
    let size:Option<usize>=tx.query_row("SELECT length(CAST(coverage_json AS BLOB))+32 FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0)).optional()?;
    let Some(size) = size else { return Ok(None) };
    material.charge(size, 1)?;
    // Admit every saved collection before Serde allocates its sets/maps. Count
    // array entries before deduplication, including each family's history keys.
    let records: usize = tx.query_row(
        "SELECT
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.active_timers')) +
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.pending_wakes')) +
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.active_triggers')) +
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.required_keys')) +
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.observations')) +
            (SELECT COUNT(*) FROM json_each(d.coverage_json,'$.history')) +
            COALESCE((SELECT SUM(COALESCE(json_array_length(h.value,'$.keys'),0)+COALESCE(json_array_length(h.value,'$.loaded_keys'),0))
                FROM json_each(d.coverage_json,'$.history') h),0)
         FROM scheduling_domain_current d
         WHERE d.agent_id=?1 AND d.identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation],
        |row| row.get(0),
    )?;
    material.charge(
        records
            .checked_mul(128)
            .context("saved Scheduling collection metadata overflow")?,
        records,
    )?;
    let (revision,raw):(u64,String)=tx.query_row("SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let coverage: SchedulingCoverage = serde_json::from_str(&raw)?;
    ensure!(
        coverage.revision == revision,
        "Scheduling coverage revision differs"
    );
    Ok(Some(coverage))
}
pub(super) fn selection_filter(
    family: SchedulingFamily,
    live: bool,
    manifests: &[SchedulingContinuityCheckpoint],
) -> String {
    let (_, id, _, predicate, _) = stored::source(family);
    let inventory = manifests
        .iter()
        .map(|m| format!("'{}'", m.inventory_id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let inventory = if inventory.is_empty() {
        "NULL"
    } else {
        &inventory
    };
    let current_known=format!("({id} IN (SELECT record_id FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='{}' AND availability!='absent') OR {id} IN (SELECT record_id FROM scheduling_fixed_members WHERE family='{}' AND inventory_id IN ({inventory})))",family_name(family),family_name(family));
    let known_anywhere=format!("({id} IN (SELECT record_id FROM scheduling_domain_records WHERE family='{}' AND availability!='absent') OR {id} IN (SELECT record_id FROM scheduling_fixed_members WHERE family='{}'))",family_name(family),family_name(family));
    if live {
        format!("({predicate}) AND ({current_known} OR NOT {known_anywhere})")
    } else {
        format!("NOT ({predicate}) AND {current_known}")
    }
}
fn ids(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    family: SchedulingFamily,
    live: bool,
    limit: Option<usize>,
    manifests: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
) -> Result<Vec<SchedulingKey>> {
    let (from, id, owner, _, sort) = stored::source(family);
    // Live canonical rows are checked, never silently filtered into complete empty.
    // Optional history is only already-qualified or fixed, not a new SQL tail.
    let filter = selection_filter(family, live, manifests);
    let selected = if live {
        format!("SELECT {id} AS selected_id FROM {from} WHERE {owner}=?1 AND {filter} ORDER BY {sort} DESC,c.created_at DESC,{id} ASC{}",limit.map_or(String::new(),|n|format!(" LIMIT {n}")))
    } else {
        let heads = super::history_selection::accepted_heads(family, manifests);
        format!("SELECT record_id AS selected_id FROM ({heads}) h WHERE NOT ({}) ORDER BY sort_at DESC,created_at DESC,record_id ASC{}",
            super::history_selection::live_header_predicate(family),limit.map_or(String::new(),|n|format!(" LIMIT {n}")))
    };
    let sql = format!(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(selected_id AS BLOB))+64),0) FROM ({selected})"
    );
    let (count, bytes): (usize, usize) = tx.query_row(
        &sql,
        params![scope.agent_id, scope.identity_incarnation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    material.charge(bytes, count)?;
    let mut statement = tx.prepare(&selected)?;
    let read = |r: &rusqlite::Row<'_>| r.get::<_, String>(0);
    let values: Vec<String> = statement
        .query_map(params![scope.agent_id, scope.identity_incarnation], read)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(values
        .into_iter()
        .map(|id| SchedulingKey::new(family, id))
        .collect())
}
fn fixed_live_keys(
    tx: &Transaction<'_>,
    checkpoints: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
) -> Result<BTreeSet<SchedulingKey>> {
    let mut keys = BTreeSet::new();
    for checkpoint in checkpoints {
        let predicate="((family='timer' AND status IN ('active','scheduled')) OR (family='timer_wake' AND status='pending') OR (family='external_trigger' AND status='active'))";
        let (count,bytes):(usize,usize)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(family AS BLOB))+length(CAST(record_id AS BLOB))+64),0) FROM scheduling_fixed_members WHERE inventory_id=?1 AND {predicate}"),[&checkpoint.inventory_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        material.charge(bytes, count)?;
        let mut statement=tx.prepare(&format!("SELECT family,record_id FROM scheduling_fixed_members WHERE inventory_id=?1 AND {predicate} ORDER BY family,record_id"))?;
        let mut rows = statement.query([&checkpoint.inventory_id])?;
        while let Some(row) = rows.next()? {
            let family: String = row.get(0)?;
            let family = match family.as_str() {
                "timer" => SchedulingFamily::Timer,
                "timer_wake" => SchedulingFamily::TimerWake,
                "external_trigger" => SchedulingFamily::ExternalTrigger,
                _ => anyhow::bail!("invalid fixed Scheduling family"),
            };
            keys.insert(SchedulingKey::new(family, row.get::<_, String>(1)?));
        }
    }
    Ok(keys)
}

pub(super) fn member_revision(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &SchedulingKey,
    stored: &stored::StoredScheduling,
    manifests: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
) -> Result<u64> {
    verify_member_version(
        tx,
        scope,
        key,
        (
            &stored.content,
            &stored.projection,
            &stored.status,
            &stored.sort_at,
            &stored.created_at,
        ),
        manifests,
        material,
    )
}

pub(super) fn verify_member_version(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &SchedulingKey,
    version: (&str, &str, &str, &str, &str),
    manifests: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
) -> Result<u64> {
    let (content, projection, status, sort_at, created_at) = version;
    if let Some(alias) = read_alias(tx, scope, key, material)? {
        ensure!(
            alias.availability == SchedulingAvailability::Present,
            SchedulingSnapshotError::NeedsPreparation(key.clone())
        );
        ensure!(
            alias.content.as_deref() == Some(content)
                && alias.projection.as_deref() == Some(projection)
                && alias.status.as_deref() == Some(status)
                && alias.sort_at.as_deref() == Some(sort_at)
                && alias.created_at.as_deref() == Some(created_at),
            SchedulingSnapshotError::CanonicalChanged(key.clone())
        );
        return Ok(alias.revision);
    }
    for manifest in manifests.iter().rev() {
        material.charge(128, 0)?;
        if let Some(expected) = qualification::fixed_digest(tx, manifest, key)? {
            ensure!(
                expected == (content.to_owned(), projection.to_owned()),
                SchedulingSnapshotError::CanonicalChanged(key.clone())
            );
            return Ok(1);
        }
    }
    Err(SchedulingSnapshotError::NeedsPreparation(key.clone()).into())
}
fn load(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prepare: bool,
    original_recovery: bool,
) -> Result<SchedulingBootstrapSnapshot> {
    load_with_revision(
        tx,
        scope,
        request,
        ledger,
        prepare,
        original_recovery,
        None,
        None,
        None,
    )
}
pub(super) fn load_with_revision(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prepare: bool,
    original_recovery: bool,
    revision_override: Option<u64>,
    prior_material: Option<(usize, usize)>,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<SchedulingBootstrapSnapshot> {
    load_with_revision_inner(
        tx,
        scope,
        request,
        ledger,
        prepare,
        original_recovery,
        revision_override,
        prior_material,
        memory_budget,
        false,
    )
}

pub(super) fn load_for_input(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<SchedulingBootstrapSnapshot> {
    load_with_revision_inner(
        tx,
        scope,
        request,
        ledger,
        true,
        original_recovery,
        None,
        None,
        memory_budget,
        true,
    )
}

fn load_with_revision_inner(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prepare: bool,
    original_recovery: bool,
    revision_override: Option<u64>,
    prior_material: Option<(usize, usize)>,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    input_memory: bool,
) -> Result<SchedulingBootstrapSnapshot> {
    bound(0, 0, request)?;
    ensure!(
        request.capture.history_limit <= SCHEDULING_HISTORY_WINDOW,
        "Scheduling history policy exceeded"
    );
    let mut material = Material::new(scope, request, ledger);
    material.memory_budget = memory_budget;
    material.input_memory = input_memory;
    if let Some((bytes, records)) = prior_material {
        material.bytes = bytes;
        material.records = records;
    }
    material.charge(usize::try_from(serialized_bytes(scope)?)?, 1)?;
    let Some(facts) = scope_facts(tx, scope, &mut material, original_recovery)? else {
        return Ok(SchedulingBootstrapSnapshot::Retired {
            _scope: scope.clone(),
            _read_material: material.guards,
        });
    };
    let checkpoints = qualification::manifests(tx, scope, &mut material)?;
    let previous = current(tx, scope, &mut material)?;
    let revision = revision_override.unwrap_or_else(|| previous.as_ref().map_or(1, |c| c.revision));
    ensure!(
        revision >= previous.as_ref().map_or(1, |c| c.revision),
        "Scheduling final revision went backwards"
    );
    if !prepare {
        ensure!(previous.is_some(), SchedulingReadError::IncompleteLive);
    }
    let mut coverage = SchedulingCoverage::empty(revision);
    let selector_bytes = usize::try_from(serialized_bytes(&request.capture.required_keys)?)?;
    material.charge(selector_bytes, request.capture.required_keys.len())?;
    coverage.required_keys = request.capture.required_keys.clone();
    let mut selected = request.capture.required_keys.clone();
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        let live: BTreeSet<_> = ids(tx, scope, family, true, None, &checkpoints, &mut material)?
            .into_iter()
            .collect();
        selected.extend(live.iter().cloned());
        match family {
            SchedulingFamily::Timer => coverage.active_timers = live,
            SchedulingFamily::TimerWake => coverage.pending_wakes = live,
            SchedulingFamily::ExternalTrigger => coverage.active_triggers = live,
        }
        let mut history = ids(
            tx,
            scope,
            family,
            false,
            Some(request.capture.history_limit + 1),
            &checkpoints,
            &mut material,
        )?;
        let exhaustive = history.len() <= request.capture.history_limit;
        let loaded = if family == SchedulingFamily::TimerWake || request.capture.history_limit == 0
        {
            Some(history.clone())
        } else {
            super::history_selection::loaded_history_ids(
                tx,
                scope,
                family,
                request.capture.history_limit + 1,
                &checkpoints,
                &mut material,
            )?
        };
        let mut loaded_history = if let Some(loaded) = loaded {
            ensure!(
                loaded.len() == history.len(),
                "Scheduling history orders differ in cardinality"
            );
            loaded
        } else {
            // Pruned is retained metadata, never a replacement timestamp or a
            // reason to roll the accepted 128-head cohort forward.
            let cohort = &history[..history.len().min(request.capture.history_limit)];
            previous
                .as_ref()
                .filter(|previous| previous.revision == revision)
                .and_then(|previous| previous.history.get(&family))
                .filter(|prior| prior.keys == cohort && prior.loaded_keys.len() == cohort.len())
                .map(|prior| prior.loaded_keys.clone())
                .unwrap_or_default()
        };
        history.truncate(request.capture.history_limit);
        loaded_history.truncate(request.capture.history_limit);
        selected.extend(history.iter().cloned());
        selected.extend(loaded_history.iter().cloned());
        coverage.history.insert(
            family,
            SchedulingHistory {
                keys: history,
                loaded_keys: loaded_history,
                is_exhaustive: exhaustive,
            },
        );
    }
    // Recheck the finite prior heads as well: canonical/header drift must not
    // hide a previously observed key by moving it below the new cutoff.
    let mut core = selected.clone();
    let old_live = if let Some(previous) = &previous {
        previous
            .active_timers
            .iter()
            .chain(&previous.pending_wakes)
            .chain(&previous.active_triggers)
            .chain(
                previous
                    .history
                    .values()
                    .flat_map(|history| history.keys.iter().chain(&history.loaded_keys)),
            )
            .cloned()
            .collect()
    } else {
        fixed_live_keys(tx, &checkpoints, &mut material)?
    };
    selected.extend(old_live);
    let mut records = BTreeMap::new();
    let mut observations = BTreeMap::new();
    let mut pending: Vec<_> = selected.iter().cloned().collect();
    let mut inputs = BTreeSet::new();
    while let Some(key) = pending.pop() {
        if observations.contains_key(&key) {
            continue;
        }
        let alias = read_alias(tx, scope, &key, &mut material)?;
        if let Some(alias) = &alias {
            if alias.availability == SchedulingAvailability::Pruned {
                observations.insert(key, SchedulingAvailability::Pruned);
                continue;
            }
        }
        // A foreign normalized owner is a refusal, never adoption authority.
        // Check its scalar relation before qualification or any foreign body read.
        ensure!(
            !stored::has_foreign_owner(tx, &key, &scope.agent_id)?,
            SchedulingSnapshotError::ForeignRecord(key.clone())
        );
        let mut qualified = alias
            .as_ref()
            .is_some_and(|a| a.availability == SchedulingAvailability::Present);
        if !qualified {
            for checkpoint in checkpoints.iter().rev() {
                material.charge(128, 0)?;
                if qualification::fixed_digest(tx, checkpoint, &key)?.is_some() {
                    qualified = true;
                    break;
                }
            }
        }
        if !qualified && stored::exists_global(tx, &key)? {
            // A current same-name namespace cannot even decode an old or raw
            // unqualified body as a candidate for adoption.
            return Err(SchedulingSnapshotError::NeedsPreparation(key).into());
        }
        if let Some(stored) = stored::read_stored(tx, &key, &scope.agent_id, Some(&mut material))? {
            let member_revision =
                member_revision(tx, scope, &key, &stored, &checkpoints, &mut material)?;
            if !prepare {
                ensure!(
                    alias
                        .as_ref()
                        .is_some_and(|a| a.availability == SchedulingAvailability::Present),
                    SchedulingSnapshotError::NeedsPreparation(key.clone())
                );
            }
            let value = stored.qualified(scope, member_revision)?;
            if core.contains(&key) {
                if let CanonicalSchedulingFact::TimerWake(w) = &value.fact {
                    let parent = SchedulingKey::new(SchedulingFamily::Timer, &w.timer_id);
                    core.insert(parent.clone());
                    if selected.insert(parent.clone()) {
                        pending.push(parent);
                    }
                    if w.status == SchedulingWakeStatus::Pending {
                        inputs.insert(InputEvidenceKey::new(
                            InputEvidenceFamily::Message,
                            &w.message_id,
                        ));
                    }
                }
            }
            if prepare {
                write_alias(
                    tx,
                    scope,
                    &key,
                    member_revision,
                    SchedulingAvailability::Present,
                    Some(&stored.content),
                    Some(&stored.projection),
                    Some((&stored.status, &stored.sort_at, &stored.created_at)),
                )?;
            }
            observations.insert(key.clone(), SchedulingAvailability::Present);
            records.insert(key, value);
        } else {
            ensure!(
                !stored::exists_global(tx, &key)?,
                SchedulingSnapshotError::NeedsPreparation(key.clone())
            );
            if alias
                .as_ref()
                .is_some_and(|a| a.availability == SchedulingAvailability::Present)
                || qualification::known_anywhere(tx, &key)?
            {
                return Err(SchedulingSnapshotError::CanonicalChanged(key).into());
            }
            let unavailable = !checkpoints.is_empty()
                && checkpoints
                    .iter()
                    .all(|c| !c.observed_tables.contains(&key.family));
            let availability = if unavailable {
                SchedulingAvailability::HistoricalUnavailable
            } else {
                SchedulingAvailability::Absent
            };
            if prepare && !unavailable {
                write_alias(
                    tx,
                    scope,
                    &key,
                    revision,
                    SchedulingAvailability::Absent,
                    None,
                    None,
                    None,
                )?;
            }
            observations.insert(key, availability);
        }
    }
    records.retain(|key, _| core.contains(key));
    observations.retain(|key, _| core.contains(key));
    coverage.observations = observations
        .into_iter()
        .map(|(key, availability)| SchedulingObservation { key, availability })
        .collect();
    let metadata = usize::try_from(serialized_bytes(&coverage)?)?;
    material.charge(metadata, 1)?;
    let resident = ResidentSchedulingDomain::bootstrap(
        scope.clone(),
        coverage.clone(),
        records.into_values().collect(),
        request.record_limit,
        request.byte_limit,
    )?
    .with_budget(ledger.clone())?;
    if prepare {
        tx.execute("INSERT INTO scheduling_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",params![scope.agent_id,scope.identity_incarnation,revision,serde_json::to_string(&coverage)?])?;
    } else {
        ensure!(
            previous.as_ref() == Some(&coverage),
            SchedulingReadError::IncompleteLive
        );
    }
    // Owned decode guards stay until the resident's actual shared allocation views exist.
    let material_usage = (material.bytes, material.records);
    let guards = material.guards;
    Ok(SchedulingBootstrapSnapshot::Current(Box::new(
        SchedulingCurrentSnapshot {
            facts,
            resident,
            input_roots: inputs,
            material_usage,
            _read_material: guards,
        },
    )))
}
