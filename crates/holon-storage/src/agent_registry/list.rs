//! Identity-fenced Agent list and record reads.

use std::collections::HashMap;

use anyhow::Result;
use holon_domain::{
    ExternalTriggerRecord, QueueEntryStatus, RecordListStatus, TaskRecord, ToolExecutionRecord,
    WaitConditionRecord, WorkItemContinuationFrame, WorkItemRecord, WorkItemState,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::codec::enum_string;
use crate::conversation::{active_agent_identity, ConversationIdentity, ConversationReadError};
use crate::task::{decode_task_payload, query_tasks_for_agent};
use crate::wait::{decode_wait_condition_row, wait_condition_decode_error};
use crate::work_item::decode_work_item_payload;

use super::{decode_external_trigger_payload, decode_work_item_continuation_payload};

/// Per-agent posture derivation facts from one committed read view.
///
/// `RuntimeReadModels` fills this bundle from its per-agent queries; the
/// batch list reader fills it from one transaction. The pure projection
/// rules in the runtime read model consume it, so both paths share
/// one implementation of the posture contract.
///
/// `current_work_item_id` belongs to the committed AgentState payload and
/// is filled by the caller that decodes that payload; the batch reader
/// leaves it `None`.
#[derive(Debug, Clone, Default)]
pub struct AgentPostureFacts {
    pub current_work_item_id: Option<String>,
    pub has_queued_input: bool,
    /// Open work items; scheduler candidates are naturally bounded.
    pub open_work_items: Vec<WorkItemRecord>,
    /// Most recent non-open work items, bounded by
    /// the caller-supplied history window.
    pub non_open_work_items: Vec<WorkItemRecord>,
    /// Latest external trigger records, ordered exactly like
    /// `ExternalTriggerRepository::latest_for_agent`.
    pub external_triggers: Vec<ExternalTriggerRecord>,
    /// Active wait conditions before the live-scope filter, ordered
    /// exactly like `WaitConditionRepository::active_for_agent`.
    pub active_wait_conditions: Vec<WaitConditionRecord>,
    /// Active continuation frames for the agent.
    pub active_continuations: Vec<WorkItemContinuationFrame>,
}

/// One agent's rows from the shared list read view. `agent_state_json`
/// stays as the stored payload so assembly keeps its per-agent
/// stopped-placeholder fallback for unreadable state. `facts` captures any
/// fact decode failure for that agent alone.
#[derive(Debug, Clone)]
pub struct AgentListPostureRow {
    pub agent_id: String,
    pub agent_state_json: Option<String>,
    pub has_queued_input: bool,
    pub facts: std::result::Result<AgentPostureFacts, String>,
}

#[derive(Debug, Clone)]
pub struct AgentListPostureRows {
    pub rows: Vec<AgentListPostureRow>,
}

pub fn agent_tasks_snapshot(
    connection: &mut Connection,
    agent_id: &str,
    expected_incarnation: Option<u64>,
    terminal: bool,
    limit: usize,
) -> Result<Option<(ConversationIdentity, Vec<TaskRecord>)>> {
    read_agent_record_snapshot(connection, agent_id, expected_incarnation, |view| {
        query_tasks_for_agent(
            view,
            if terminal {
                "owner_agent_id = ?1 AND status IN ('completed', 'failed', 'cancelled', 'interrupted')"
            } else {
                "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')"
            },
            [agent_id],
            limit,
        )
    })
}

pub fn agent_work_item_snapshot(
    connection: &mut Connection,
    agent_id: &str,
    work_item_id: &str,
    expected_incarnation: Option<u64>,
) -> Result<Option<(ConversationIdentity, Option<AgentListPostureRow>)>> {
    read_agent_record_snapshot(connection, agent_id, expected_incarnation, |view| {
        let record = view
            .query_row(
                "SELECT payload_json FROM work_items
                 WHERE agent_id = ?1 AND work_item_id = ?2",
                params![agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_payload(&payload))
            .transpose()?;
        let Some(record) = record else {
            return Ok(None);
        };
        let mut rows = collect_agent_list_posture_rows_filtered(
            view,
            &[agent_id.to_owned()],
            RecordListStatus::All,
            0,
        )?;
        let Some(mut row) = rows.rows.pop() else {
            return Ok(None);
        };
        if record.state != WorkItemState::Open {
            row.facts
                .as_mut()
                .map_err(|error| anyhow::anyhow!("{error}"))?
                .non_open_work_items
                .push(record);
        }
        Ok(Some(row))
    })
}

pub fn agent_task_snapshot(
    connection: &mut Connection,
    agent_id: &str,
    task_id: &str,
    expected_incarnation: Option<u64>,
) -> Result<Option<(ConversationIdentity, Option<TaskRecord>)>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let Some(identity) = fenced_agent_identity(&transaction, agent_id, expected_incarnation)?
    else {
        return Ok(None);
    };
    let task = transaction
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_task_payload(&payload))
        .transpose()?;
    transaction.commit()?;
    Ok(Some((identity, task)))
}

pub fn agent_tool_execution_snapshot(
    connection: &mut Connection,
    agent_id: &str,
    tool_execution_id: &str,
    expected_incarnation: Option<u64>,
) -> Result<Option<(ConversationIdentity, Option<ToolExecutionRecord>)>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let Some(identity) = fenced_agent_identity(&transaction, agent_id, expected_incarnation)?
    else {
        return Ok(None);
    };
    let record = transaction
        .query_row(
            "SELECT payload_json FROM tool_executions
             WHERE agent_id = ?1 AND evidence_id = ?2
             LIMIT 1",
            params![agent_id, tool_execution_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str(&payload))
        .transpose()?;
    transaction.commit()?;
    Ok(Some((identity, record)))
}

/// Both identity admission and records use the same deferred read transaction.
pub fn read_agent_record_snapshot<T>(
    connection: &mut Connection,
    agent_id: &str,
    expected_incarnation: Option<u64>,
    read_records: impl FnOnce(&Connection) -> Result<T>,
) -> Result<Option<(ConversationIdentity, T)>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let Some(identity) = fenced_agent_identity(&transaction, agent_id, expected_incarnation)?
    else {
        return Ok(None);
    };
    let records = read_records(&transaction)?;
    transaction.commit()?;
    Ok(Some((identity, records)))
}

pub fn read_work_queue_snapshot_filtered(
    connection: &mut Connection,
    agent_id: &str,
    expected_incarnation: Option<u64>,
    after_identity: impl FnOnce(),
    status: RecordListStatus,
    limit: usize,
) -> Result<Option<(ConversationIdentity, AgentListPostureRow)>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let Some(identity) = fenced_agent_identity(&transaction, agent_id, expected_incarnation)?
    else {
        return Ok(None);
    };
    after_identity();
    let rows = collect_agent_list_posture_rows_filtered(
        &transaction,
        &[agent_id.to_owned()],
        status,
        limit,
    )?;
    transaction.commit()?;
    Ok(rows.rows.into_iter().next().map(|row| (identity, row)))
}

fn fenced_agent_identity(
    connection: &Connection,
    agent_id: &str,
    expected_incarnation: Option<u64>,
) -> Result<Option<ConversationIdentity>> {
    let Some(identity) = active_agent_identity(connection, agent_id)? else {
        if expected_incarnation.is_some() {
            return Err(ConversationReadError::IdentityChanged.into());
        }
        return Ok(None);
    };
    if expected_incarnation.is_some_and(|expected| expected != identity.agent_incarnation) {
        return Err(ConversationReadError::IdentityChanged.into());
    }
    Ok(Some(identity))
}

pub fn read_posture_rows(
    connection: &mut Connection,
    agent_ids: &[String],
    non_open_window: usize,
) -> Result<AgentListPostureRows> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let rows = collect_agent_list_posture_rows_filtered(
        &transaction,
        agent_ids,
        RecordListStatus::All,
        non_open_window,
    )?;
    transaction.commit()?;
    Ok(rows)
}

pub fn collect_agent_list_posture_rows_filtered(
    connection: &Connection,
    agent_ids: &[String],
    status: RecordListStatus,
    non_open_window: usize,
) -> Result<AgentListPostureRows> {
    let queued_status = enum_string(&QueueEntryStatus::Queued)?;
    let interrupted_status = enum_string(&QueueEntryStatus::Interrupted)?;

    let mut agent_states = HashMap::<String, String>::new();
    let mut queued_agents = std::collections::BTreeSet::<String>::new();
    let mut open_work_items = HashMap::<String, Vec<WorkItemRecord>>::new();
    let mut non_open_work_items = HashMap::<String, Vec<WorkItemRecord>>::new();
    let mut external_triggers = HashMap::<String, Vec<ExternalTriggerRecord>>::new();
    let mut active_wait_conditions = HashMap::<String, Vec<WaitConditionRecord>>::new();
    let mut active_continuations = HashMap::<String, Vec<WorkItemContinuationFrame>>::new();
    let mut fact_errors = HashMap::<String, String>::new();

    for chunk in agent_ids.chunks(128) {
        let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let ids: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

        {
            let sql = format!(
                "SELECT agent_id, payload_json FROM agent_states
                 WHERE agent_id IN ({placeholders})"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(ids.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent_id, payload) = row?;
                agent_states.insert(agent_id, payload);
            }
        }

        {
            let sql = format!(
                "SELECT DISTINCT agent_id FROM queue_entries
                 WHERE agent_id IN ({placeholders}) AND status IN (?, ?)"
            );
            let mut statement = connection.prepare(&sql)?;
            let mut params: Vec<&dyn rusqlite::ToSql> = ids.clone();
            params.push(&queued_status);
            params.push(&interrupted_status);
            let rows = statement.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
            for row in rows {
                queued_agents.insert(row?);
            }
        }

        if status != RecordListStatus::Terminal {
            // No ORDER BY, mirroring WorkItemRepository::open_for_agent;
            // the queue read model keys items by id.
            let predicate = if status == RecordListStatus::Active {
                "state != 'completed'"
            } else {
                "state = 'open'"
            };
            let sql = format!(
                "SELECT agent_id, payload_json FROM work_items
                 WHERE agent_id IN ({placeholders}) AND {predicate}"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(ids.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent_id, payload) = row?;
                match decode_work_item_payload(&payload) {
                    Ok(record) => open_work_items.entry(agent_id).or_default().push(record),
                    Err(error) => {
                        fact_errors
                            .entry(agent_id)
                            .or_insert_with(|| error.to_string());
                    }
                }
            }
        }

        if status != RecordListStatus::Active && non_open_window > 0 {
            // Per-agent terminal window equal to
            // WorkItemRepository::latest_non_open_for_agent.
            let predicate = if status == RecordListStatus::Terminal {
                "state = 'completed'"
            } else {
                "state != 'open'"
            };
            let sql = format!(
                "SELECT agent_id, payload_json FROM (
                   SELECT agent_id, payload_json,
                          ROW_NUMBER() OVER (
                            PARTITION BY agent_id
                            ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
                          ) AS rn
                   FROM work_items
                   WHERE agent_id IN ({placeholders}) AND {predicate}
                 ) WHERE rn <= ?"
            );
            let window = i64::try_from(non_open_window).unwrap_or(i64::MAX);
            let mut statement = connection.prepare(&sql)?;
            let mut params: Vec<&dyn rusqlite::ToSql> = ids.clone();
            params.push(&window);
            let rows = statement.query_map(params.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent_id, payload) = row?;
                match decode_work_item_payload(&payload) {
                    Ok(record) => non_open_work_items
                        .entry(agent_id)
                        .or_default()
                        .push(record),
                    Err(error) => {
                        fact_errors
                            .entry(agent_id)
                            .or_insert_with(|| error.to_string());
                    }
                }
            }
        }

        {
            // created_at DESC ordering mirrors
            // ExternalTriggerRepository::latest_for_agent.
            let sql = format!(
                "SELECT target_agent_id, payload_json FROM external_triggers
                 WHERE target_agent_id IN ({placeholders})
                 ORDER BY created_at DESC, external_trigger_id ASC"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(ids.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent_id, payload) = row?;
                match decode_external_trigger_payload(&payload) {
                    Ok(record) => external_triggers.entry(agent_id).or_default().push(record),
                    Err(error) => {
                        fact_errors
                            .entry(agent_id)
                            .or_insert_with(|| error.to_string());
                    }
                }
            }
        }

        {
            // The agent_id ASC prefix preserves each agent's
            // active_for_agent ordering inside the grouped result.
            let sql = format!(
                "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                        subject_ref, waiting_for, created_at, updated_at, expires_at,
                        resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                        wake_sources_json, continuation_json
                 FROM wait_conditions
                 WHERE agent_id IN ({placeholders}) AND status = 'active'
                 ORDER BY agent_id ASC, updated_at DESC, created_at DESC, wait_condition_id ASC"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(ids.as_slice(), |row| {
                decode_wait_condition_row(row).map_err(wait_condition_decode_error)
            })?;
            for row in rows {
                let record = row?;
                active_wait_conditions
                    .entry(record.agent_id.clone())
                    .or_default()
                    .push(record);
            }
        }

        {
            let sql = format!(
                "SELECT agent_id, payload_json FROM work_item_continuations
                 WHERE agent_id IN ({placeholders}) AND state = 'active'
                 ORDER BY agent_id ASC, updated_at DESC, created_at DESC, continuation_id ASC"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(ids.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent_id, payload) = row?;
                match decode_work_item_continuation_payload(&payload) {
                    Ok(record) => active_continuations
                        .entry(agent_id)
                        .or_default()
                        .push(record),
                    Err(error) => {
                        fact_errors
                            .entry(agent_id)
                            .or_insert_with(|| error.to_string());
                    }
                }
            }
        }
    }

    let rows = agent_ids
        .iter()
        .map(|agent_id| {
            let facts = match fact_errors.remove(agent_id) {
                Some(error) => Err(error),
                None => Ok(AgentPostureFacts {
                    current_work_item_id: None,
                    has_queued_input: queued_agents.contains(agent_id),
                    open_work_items: open_work_items.remove(agent_id).unwrap_or_default(),
                    non_open_work_items: non_open_work_items.remove(agent_id).unwrap_or_default(),
                    external_triggers: external_triggers.remove(agent_id).unwrap_or_default(),
                    active_wait_conditions: active_wait_conditions
                        .remove(agent_id)
                        .unwrap_or_default(),
                    active_continuations: active_continuations.remove(agent_id).unwrap_or_default(),
                }),
            };
            AgentListPostureRow {
                agent_id: agent_id.clone(),
                agent_state_json: agent_states.remove(agent_id),
                has_queued_input: queued_agents.contains(agent_id),
                facts,
            }
        })
        .collect();
    Ok(AgentListPostureRows { rows })
}
