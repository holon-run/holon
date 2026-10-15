//! Original accepted/current source relations, captured at the fixed cutover.
use super::*;
use crate::types::{MessageKind, MessageOrigin};

type Pending = VecDeque<(String, String, usize)>;

pub(super) fn capture_relations(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
    authority: Option<&imports::ImportAuthority>,
    inputs: &mut RootCollector,
    checkpoint: &mut WorkContinuityCheckpoint,
    pending: &mut Pending,
    selected: &mut BTreeSet<(String, String)>,
    selected_bytes: &mut usize,
) -> Result<()> {
    let state = if root_table_available(tables, "agent_states") {
        load_state(connection, agent_id)?.0
    } else {
        None
    };
    let mut turns = BTreeSet::new();
    let mut sources = BTreeSet::new();
    if let Some(state) = state {
        turns.extend(state.current_turn_id);
        if let Some(binding) = state.current_execution_binding {
            turns.insert(binding.turn_id);
            sources.insert(binding.source_message_id);
        }
    }
    if root_table_available(tables, "queue_entries") {
        let mut query = connection.prepare("SELECT message_id FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted','dequeued','interjected') ORDER BY message_id LIMIT ?2")?;
        for row in query.query_map(
            params![agent_id, i64::try_from(CONTINUITY_MAX_RECORDS + 1)?],
            |row| row.get::<_, String>(0),
        )? {
            sources.insert(row?);
            bound_sources(&sources)?;
        }
    }
    let mut tasks = BTreeSet::new();
    let mut triggers = BTreeSet::new();
    if let Some(authority) = authority {
        // First import cannot erase the original complete relation merely
        // because a source already settled before its first seal.
        let original = authority.original_relations();
        turns.extend(original.turn_wait_keys.keys().cloned());
        tasks.extend(original.task_wait_keys.keys().cloned());
        triggers.extend(original.trigger_wait_keys.keys().cloned());
    }
    for id in sources {
        if !root_table_available(tables, "messages") {
            continue;
        }
        let metadata: Option<(String,usize)> = connection.query_row(
            "SELECT agent_id,length(CAST(payload_json AS BLOB)) FROM messages WHERE evidence_id=?1",[&id],
            |row|Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let Some((stored_agent, bytes)) = metadata else {
            continue;
        };
        ensure!(
            stored_agent == agent_id,
            "fixed Wait source belongs to another agent"
        );
        let already = inputs
            .roots
            .record_digests
            .contains_key(&format!("message:{id}"));
        if !already && inputs.payload_bytes.saturating_add(bytes) > CONTINUITY_MAX_PAYLOAD_BYTES {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "Wait relation source bytes",
            }
            .into());
        }
        let payload: String = connection.query_row(
            "SELECT payload_json FROM messages WHERE evidence_id=?1",
            [&id],
            |row| row.get(0),
        )?;
        if let Some(proof) = authority {
            if !proof.permits_relation_source(connection, &id, &bytes_digest(payload.as_bytes()))? {
                continue;
            }
        }
        let message: MessageEnvelope = serde_json::from_str(&payload)?;
        ensure!(
            message.id == id && message.agent_id == agent_id,
            "fixed Wait source projection mismatch"
        );
        inputs.digest("message", &id, &payload)?;
        triggers.insert(id);
        if message.kind == MessageKind::TaskResult {
            if let MessageOrigin::Task { task_id } = &message.origin {
                if message.task_id.as_deref() == Some(task_id.as_str()) && !task_id.is_empty() {
                    tasks.insert(task_id.clone());
                }
            }
        }
    }
    for keys in [&turns, &tasks, &triggers] {
        bound_sources(keys)?;
    }
    if let Some(authority) = authority {
        // A disappeared original relation member is not a new empty relation.
        let original = authority.original_relations();
        for id in original
            .turn_wait_keys
            .values()
            .chain(original.task_wait_keys.values())
            .chain(original.trigger_wait_keys.values())
            .flatten()
            .collect::<BTreeSet<_>>()
        {
            let (agent,bytes):(String,usize) = connection.query_row(
                "SELECT agent_id,length(CAST(payload_json AS BLOB)) FROM wait_conditions WHERE wait_condition_id=?1",[id],
                |row|Ok((row.get(0)?,row.get(1)?)),
            ).context("original fixed Wait relation member disappeared during import")?;
            ensure!(agent == agent_id, "original Wait relation changed scope");
            if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
                return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                    limit: "Wait relation body",
                }
                .into());
            }
            let payload: String = connection.query_row(
                "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
                [id],
                |row| row.get(0),
            )?;
            authority.validate_relation_wait(connection, id, &bytes_digest(payload.as_bytes()))?;
            enqueue(pending, selected, selected_bytes, "wait", id, 0)?;
        }
    }
    let mut metadata_count = turns
        .len()
        .saturating_add(tasks.len())
        .saturating_add(triggers.len());
    let mut metadata_bytes = turns
        .iter()
        .chain(&tasks)
        .chain(&triggers)
        .map(String::len)
        .sum::<usize>();
    bound_relation(metadata_count, metadata_bytes)?;
    for (kind, keys) in [("turn", turns), ("task", tasks), ("trigger", triggers)] {
        for source in keys {
            let mut ids = BTreeSet::new();
            if root_table_available(tables, "wait_conditions") {
                let predicate=match kind {
                    "turn"=>"last_turn_id=?2 OR json_extract(payload_json,'$.turn_id')=?2",
                    "task"=>"EXISTS(SELECT 1 FROM json_each(payload_json,'$.wake_sources') w WHERE json_extract(w.value,'$.kind')='task_result' AND json_extract(w.value,'$.task_id')=?2)",
                    _=>"trigger_message_id=?2 OR json_extract(payload_json,'$.trigger_message_id')=?2",
                };
                let mut query=connection.prepare(&format!("SELECT wait_condition_id,agent_id,length(CAST(payload_json AS BLOB)) FROM wait_conditions WHERE agent_id=?1 AND ({predicate}) ORDER BY wait_condition_id LIMIT ?3"))?;
                for row in query.query_map(
                    params![agent_id, source, i64::try_from(CONTINUITY_MAX_RECORDS + 1)?],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, usize>(2)?,
                        ))
                    },
                )? {
                    let (id, agent, bytes) = row?;
                    ensure!(agent == agent_id, "fixed Wait relation scope mismatch");
                    metadata_count = metadata_count.saturating_add(1);
                    metadata_bytes = metadata_bytes.saturating_add(id.len());
                    bound_relation(metadata_count, metadata_bytes)?;
                    if let Some(authority) = authority {
                        if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
                            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                                limit: "Wait relation body",
                            }
                            .into());
                        }
                        let payload: String = connection.query_row(
                            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
                            [&id],
                            |row| row.get(0),
                        )?;
                        authority.validate_relation_wait(
                            connection,
                            &id,
                            &bytes_digest(payload.as_bytes()),
                        )?;
                    }
                    enqueue(pending, selected, selected_bytes, "wait", &id, 0)?;
                    ids.insert(id);
                }
            }
            match kind {
                "turn" => {
                    checkpoint.turn_wait_keys.insert(source, ids);
                }
                "task" => {
                    checkpoint.task_wait_keys.insert(source, ids);
                }
                _ => {
                    checkpoint.trigger_wait_keys.insert(source, ids);
                }
            }
        }
    }
    Ok(())
}
fn bound_sources(keys: &BTreeSet<String>) -> Result<()> {
    bound_relation(keys.len(), keys.iter().map(String::len).sum())
}
fn bound_relation(count: usize, bytes: usize) -> Result<()> {
    if count > CONTINUITY_MAX_RECORDS || bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "Wait relation metadata",
        }
        .into());
    }
    Ok(())
}
