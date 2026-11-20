//! Original accepted/current source relations, captured at the fixed cutover.
use super::*;
use crate::types::{MessageKind, MessageOrigin};

type Pending = VecDeque<(String, String, usize)>;

pub(super) fn capture_relations(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
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
        let mut query = connection.prepare("SELECT message_id FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted','dequeued','interjected') ORDER BY message_id")?;
        for row in query.query_map(params![agent_id], |row| row.get::<_, String>(0))? {
            sources.insert(row?);
        }
    }
    let mut tasks = BTreeSet::new();
    let mut triggers = BTreeSet::new();
    for id in sources {
        if !root_table_available(tables, "messages") {
            continue;
        }
        let stored_agent: Option<String> = connection
            .query_row(
                "SELECT agent_id FROM messages WHERE evidence_id=?1",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored_agent) = stored_agent else {
            continue;
        };
        ensure!(
            stored_agent == agent_id,
            "fixed Wait source belongs to another agent"
        );
        let payload: String = connection.query_row(
            "SELECT payload_json FROM messages WHERE evidence_id=?1",
            [&id],
            |row| row.get(0),
        )?;
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
    for (kind, keys) in [("turn", turns), ("task", tasks), ("trigger", triggers)] {
        for source in keys {
            let mut ids = BTreeSet::new();
            if root_table_available(tables, "wait_conditions") {
                let predicate=match kind {
                    "turn"=>"last_turn_id=?2 OR json_extract(payload_json,'$.turn_id')=?2",
                    "task"=>"EXISTS(SELECT 1 FROM json_each(payload_json,'$.wake_sources') w WHERE json_extract(w.value,'$.kind')='task_result' AND json_extract(w.value,'$.task_id')=?2)",
                    _=>"trigger_message_id=?2 OR json_extract(payload_json,'$.trigger_message_id')=?2",
                };
                let mut query=connection.prepare(&format!("SELECT wait_condition_id,agent_id FROM wait_conditions WHERE agent_id=?1 AND ({predicate}) ORDER BY wait_condition_id"))?;
                for row in query.query_map(params![agent_id, source], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })? {
                    let (id, agent) = row?;
                    ensure!(agent == agent_id, "fixed Wait relation scope mismatch");
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
