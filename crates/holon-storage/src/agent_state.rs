//! Agent state persistence.

use anyhow::Result;
use chrono::{DateTime, Utc};
use holon_domain::{AgentState, AgentStatus};
use rusqlite::{params, Transaction};

use crate::codec::timestamp;

pub fn upsert_agent_state_at_tx(
    tx: &Transaction<'_>,
    record: &AgentState,
    updated_at: DateTime<Utc>,
    admit_payload: fn(&AgentState) -> Result<()>,
    admit_timestamp: fn(&DateTime<Utc>) -> Result<()>,
) -> Result<()> {
    if record.current_run_id.is_some() || record.status == AgentStatus::AwakeRunning {
        crate::reclamation::ensure_work_admission_tx(tx, &record.id)?;
    }
    admit_payload(record)?;
    let payload_json = serde_json::to_string(record)?;
    let status = match record.status {
        AgentStatus::Booting => "booting",
        AgentStatus::AwakeIdle => "awake_idle",
        AgentStatus::AwakeRunning => "awake_running",
        AgentStatus::AwaitingTask => "awaiting_task",
        AgentStatus::Asleep => "asleep",
        AgentStatus::Stopped => "stopped",
    };
    admit_timestamp(&updated_at)?;
    let now = timestamp(updated_at);
    tx.execute(
        "INSERT INTO agent_states (
            agent_id, status, turn_index, current_run_id, current_work_item_id,
            active_workspace_id, updated_at, payload_json, control_revision
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, json_remove(?8, '$.pending'), 1)
         ON CONFLICT(agent_id) DO UPDATE SET
            status = excluded.status,
            turn_index = excluded.turn_index,
            current_run_id = excluded.current_run_id,
            current_work_item_id = excluded.current_work_item_id,
            active_workspace_id = excluded.active_workspace_id,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json,
            control_revision = agent_states.control_revision + 1
         WHERE excluded.turn_index >= agent_states.turn_index",
        params![
            record.id,
            status,
            record.turn_index as i64,
            record.current_run_id,
            record.current_work_item_id,
            record
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.as_str()),
            now,
            payload_json,
        ],
    )?;
    Ok(())
}
