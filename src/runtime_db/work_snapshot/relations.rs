use super::*;
use crate::types::{WaitConditionRecord, WakeSource};

type WaitRelations = BTreeMap<String, BTreeSet<String>>;

fn typed_edges(wait: &WaitConditionRecord) -> Result<BTreeSet<(&'static str, String)>> {
    let mut edges = BTreeSet::new();
    let mut bytes = 0usize;
    let mut insert = |kind: &'static str, id: &str| -> Result<()> {
        ensure!(!id.is_empty(), "empty qualified Wait relation source");
        if !edges.contains(&(kind, id.to_owned())) {
            bytes = bytes.saturating_add(id.len()).saturating_add(wait.id.len());
            bound_metadata(edges.len() + 1, bytes)?;
            edges.insert((kind, id.to_owned()));
        }
        Ok(())
    };
    if let Some(turn) = &wait.turn_id {
        insert("turn", turn)?;
    }
    if let Some(trigger) = &wait.trigger_message_id {
        insert("trigger", trigger)?;
    }
    for source in &wait.wake_sources {
        if let WakeSource::TaskResult { task_id } = source {
            insert("task", task_id)?;
        }
    }
    Ok(edges)
}

/// Only a selected, already-qualified body or the original actual writer may
/// publish these edges. Replacing a Wait removes its superseded relationships.
pub(super) fn register_wait_edges_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait: &WaitConditionRecord,
    revision: u64,
    pruned: bool,
) -> Result<()> {
    ensure!(wait.agent_id == scope.agent_id, "Wait edge scope mismatch");
    let edges = typed_edges(wait)?;
    if !pruned {
        let was_pruned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM work_domain_wait_edges WHERE agent_id=?1 AND identity_incarnation=?2 AND wait_id=?3 AND availability='pruned')",params![scope.agent_id,scope.identity_incarnation,wait.id],|row|row.get(0))?;
        ensure!(
            !was_pruned,
            "qualified Wait edge cannot resurrect a pruned ID"
        );
        tx.execute("DELETE FROM work_domain_wait_edges WHERE agent_id=?1 AND identity_incarnation=?2 AND wait_id=?3",params![scope.agent_id,scope.identity_incarnation,wait.id])?;
    } else {
        tx.execute("UPDATE work_domain_wait_edges SET availability='pruned',revision=?4 WHERE agent_id=?1 AND identity_incarnation=?2 AND wait_id=?3",params![scope.agent_id,scope.identity_incarnation,wait.id,revision])?;
    }
    for (kind, source) in edges {
        tx.execute("INSERT INTO work_domain_wait_edges(agent_id,identity_incarnation,edge_kind,source_id,wait_id,availability,revision) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(agent_id,identity_incarnation,edge_kind,source_id,wait_id) DO UPDATE SET availability=excluded.availability,revision=excluded.revision",params![scope.agent_id,scope.identity_incarnation,kind,source,wait.id,if pruned {"pruned"} else {"present"},revision])?;
    }
    Ok(())
}

/// Every matching canonical row enters the ordinary qualified body path.
/// A raw/unqualified match or a known pruned edge never becomes observed empty.
pub(super) fn load_wait_relations_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    facts: &ContextScopeFacts,
    request: &WorkDomainLoadRequest,
    required: &mut BTreeSet<WorkKey>,
) -> Result<(WaitRelations, WaitRelations, WaitRelations)> {
    let mut turns = request.capture.required_turn_ids.clone();
    if let Some(state) = &facts.state {
        turns.extend(state.current_turn_id.clone());
        turns.extend(
            state
                .current_execution_binding
                .as_ref()
                .map(|binding| binding.turn_id.clone()),
        );
    }
    let seed = binding(tx, scope)?;
    let fixed = qualification::manifests(tx, seed.as_deref())?;
    let mut relation_count = 0usize;
    let mut relation_bytes = 0usize;
    let mut collect = |kind: &str,
                       keys: &BTreeSet<String>,
                       predicate: &str|
     -> Result<WaitRelations> {
        let mut relations = BTreeMap::new();
        for key in keys {
            ensure!(!key.is_empty(), "empty Work Wait relation source");
            relation_count = relation_count.saturating_add(1);
            relation_bytes = relation_bytes.saturating_add(key.len());
            bound_metadata(relation_count, relation_bytes)?;
            // Missing fixed members are not a fresh absence. An actual qualified
            // update replaces their relation using the durable edge index.
            for manifest in &fixed {
                let source = match kind {
                    "turn" => &manifest.turn_wait_keys,
                    "task" => &manifest.task_wait_keys,
                    _ => &manifest.trigger_wait_keys,
                };
                for id in source.get(key).into_iter().flatten() {
                    let wait = WorkKey::new(WorkFamily::Wait, id);
                    if alias(tx, scope, &wait)?.is_none() {
                        let present:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE wait_condition_id=?1)",[id],|row|row.get(0))?;
                        if !present {
                            return Err(WorkSnapshotError::NeedsPreparation(wait).into());
                        }
                        required.insert(wait);
                    }
                }
            }
            let mut query=tx.prepare(&format!("SELECT wait_condition_id FROM wait_conditions WHERE agent_id=?1 AND ({predicate}) ORDER BY wait_condition_id LIMIT ?3"))?;
            let mut ids = BTreeSet::new();
            for row in query.query_map(
                params![
                    scope.agent_id,
                    key,
                    i64::try_from(MAX_METADATA_RECORDS + 1)?
                ],
                |row| row.get::<_, String>(0),
            )? {
                let id = row?;
                relation_count = relation_count.saturating_add(1);
                relation_bytes = relation_bytes.saturating_add(id.len());
                bound_metadata(relation_count, relation_bytes)?;
                required.insert(WorkKey::new(WorkFamily::Wait, &id));
                ids.insert(id);
            }
            let mut edges=tx.prepare("SELECT wait_id,availability,revision FROM work_domain_wait_edges WHERE agent_id=?1 AND identity_incarnation=?2 AND edge_kind=?3 AND source_id=?4 ORDER BY wait_id LIMIT ?5")?;
            for row in edges.query_map(
                params![
                    scope.agent_id,
                    scope.identity_incarnation,
                    kind,
                    key,
                    i64::try_from(MAX_METADATA_RECORDS + 1)?
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )? {
                let (id, availability, revision) = row?;
                relation_count = relation_count.saturating_add(1);
                relation_bytes = relation_bytes.saturating_add(id.len());
                bound_metadata(relation_count, relation_bytes)?;
                let wait = WorkKey::new(WorkFamily::Wait, &id);
                let current = alias(tx, scope, &wait)?
                    .context("qualified Wait relation has no original alias")?;
                if availability == "pruned" || current.availability == WorkAvailability::Pruned {
                    return Err(WorkSnapshotError::NeedsPreparation(wait).into());
                }
                ensure!(
                    availability == "present"
                        && current.availability == WorkAvailability::Present
                        && revision == current.revision,
                    "Wait relation qualification revision mismatch"
                );
                ensure!(
                    ids.contains(&id),
                    "qualified Wait relationship changed without its original writer"
                );
                required.insert(wait);
            }
            relations.insert(key.clone(), ids);
        }
        Ok(relations)
    };
    // Payload alternatives expose normalized corruption to stored_on validation.
    let turns = collect(
        "turn",
        &turns,
        "last_turn_id=?2 OR json_extract(payload_json,'$.turn_id')=?2",
    )?;
    let tasks=collect("task",&request.capture.required_task_ids,"EXISTS(SELECT 1 FROM json_each(wake_sources_json) w WHERE json_extract(w.value,'$.kind')='task_result' AND json_extract(w.value,'$.task_id')=?2) OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.wake_sources') w WHERE json_extract(w.value,'$.kind')='task_result' AND json_extract(w.value,'$.task_id')=?2)")?;
    let triggers = collect(
        "trigger",
        &request.capture.required_trigger_message_ids,
        "trigger_message_id=?2 OR json_extract(payload_json,'$.trigger_message_id')=?2",
    )?;
    Ok((turns, tasks, triggers))
}
