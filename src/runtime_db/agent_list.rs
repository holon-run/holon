//! Batched read facts for the `/agents/list` projection.
//!
//! Assembling one `AgentListEntry` per public active agent needs per-agent
//! facts from six record families (committed AgentState, queued input,
//! work items, external triggers, wait conditions, continuation frames).
//! Reading them per agent opens one SQLite connection per small query
//! (N+1); this module reads every family for every requested agent
//! through one connection inside one deferred read transaction, so a list
//! response observes a single committed database view with O(1)
//! connection churn.
//!
//! Decode failures are captured per agent instead of failing the whole
//! read: `/agents/list` degrades that one agent to placeholder facts
//! (matching the previous per-agent read-error behavior) while the rest of
//! the roster stays servable.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

use crate::types::{
    ExternalTriggerRecord, QueueEntryStatus, WaitConditionRecord, WorkItemContinuationFrame,
    WorkItemRecord,
};

use super::repositories::{
    decode_external_trigger_payload, decode_wait_condition_row,
    decode_work_item_continuation_payload, decode_work_item_payload, enum_string,
    wait_condition_decode_error,
};
use super::RuntimeDb;

/// Per-agent posture derivation facts from one committed read view.
///
/// `RuntimeReadModels` fills this bundle from its per-agent queries; the
/// batch list reader fills it from one transaction. The pure projection
/// rules in `crate::storage::read_models` consume it, so both paths share
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
    /// [`crate::storage::read_models::WORK_QUEUE_NON_OPEN_WINDOW`].
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

impl RuntimeDb {
    /// Reads posture facts for every requested agent through one
    /// connection inside one deferred read transaction. Returns exactly
    /// one row per requested agent id (empty facts when the agent has no
    /// rows in a family). An empty request opens no connection.
    pub fn agent_list_posture_rows(&self, agent_ids: &[String]) -> Result<AgentListPostureRows> {
        if agent_ids.is_empty() {
            return Ok(AgentListPostureRows { rows: Vec::new() });
        }
        let started_at = std::time::Instant::now();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let rows = collect_agent_list_posture_rows(&transaction, agent_ids)?;
        transaction.commit()?;
        crate::diagnostics::record_agent_list_projection(started_at.elapsed());
        Ok(rows)
    }
}

fn collect_agent_list_posture_rows(
    connection: &Connection,
    agent_ids: &[String],
) -> Result<AgentListPostureRows> {
    let non_open_window = crate::storage::read_models::WORK_QUEUE_NON_OPEN_WINDOW;
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

        {
            // No ORDER BY, mirroring WorkItemRepository::open_for_agent;
            // the queue read model keys items by id.
            let sql = format!(
                "SELECT agent_id, payload_json FROM work_items
                 WHERE agent_id IN ({placeholders}) AND state = 'open'"
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

        {
            // Per-agent terminal window equal to
            // WorkItemRepository::latest_non_open_for_agent.
            let sql = format!(
                "SELECT agent_id, payload_json FROM (
                   SELECT agent_id, payload_json,
                          ROW_NUMBER() OVER (
                            PARTITION BY agent_id
                            ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
                          ) AS rn
                   FROM work_items
                   WHERE agent_id IN ({placeholders}) AND state != 'open'
                 ) WHERE rn <= ?"
            );
            let window = non_open_window as i64;
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
