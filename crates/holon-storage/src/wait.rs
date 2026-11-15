//! Durable Wait rows shared by runtime writes and SQLite tests.

use anyhow::Result;
use holon_domain::WaitConditionRecord;
use rusqlite::{params, Statement, Transaction};

pub struct WaitRow<'a> {
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub expires_at: Option<&'a str>,
    pub resolved_at: Option<&'a str>,
    pub cancelled_at: Option<&'a str>,
    pub triggered_at: Option<&'a str>,
    pub wake_sources_json: &'a str,
    pub continuation_json: Option<&'a str>,
    pub payload_json: &'a str,
}

pub fn upsert_wait_row_tx(
    tx: &Transaction<'_>,
    record: &WaitConditionRecord,
    status: &str,
    kind: &str,
    row: WaitRow<'_>,
) -> Result<usize> {
    Ok(tx.execute(
        "INSERT INTO wait_conditions (
            wait_condition_id, agent_id, work_item_id, status, kind, source,
            subject_ref, waiting_for, created_at, updated_at, expires_at,
            resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
            wake_sources_json, continuation_json, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(wait_condition_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            work_item_id = excluded.work_item_id,
            status = excluded.status,
            kind = excluded.kind,
            source = excluded.source,
            subject_ref = excluded.subject_ref,
            waiting_for = excluded.waiting_for,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            expires_at = excluded.expires_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            last_turn_id = excluded.last_turn_id,
            trigger_message_id = excluded.trigger_message_id,
            triggered_at = excluded.triggered_at,
            wake_sources_json = excluded.wake_sources_json,
            continuation_json = excluded.continuation_json,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= wait_conditions.updated_at",
        params![
            record.id,
            record.agent_id,
            record.work_item_id,
            status,
            kind,
            record.source,
            record.subject_ref,
            record.waiting_for,
            row.created_at,
            row.updated_at,
            row.expires_at,
            row.resolved_at,
            row.cancelled_at,
            record.turn_id,
            record.trigger_message_id(),
            row.triggered_at,
            row.wake_sources_json,
            row.continuation_json,
            row.payload_json,
        ],
    )?)
}

pub fn cancel_wait_row_tx(
    tx: &Transaction<'_>,
    wait_id: &str,
    updated_at: &str,
    payload_json: &str,
) -> Result<usize> {
    Ok(tx.execute(
        "UPDATE wait_conditions
             SET status = 'cancelled',
                 updated_at = ?1,
                 resolved_at = NULL,
                 cancelled_at = ?1,
                 payload_json = ?2
             WHERE wait_condition_id = ?3",
        params![updated_at, payload_json, wait_id],
    )?)
}

pub fn prepare_unresolved_replacements<'tx>(tx: &'tx Transaction<'_>) -> Result<Statement<'tx>> {
    tx.prepare(
        "SELECT wait_condition_id, payload_json FROM wait_conditions
             WHERE agent_id = ?1
               AND work_item_id IS ?2
               AND wait_condition_id != ?3
               AND status IN ('active', 'triggered')",
    )
    .map_err(Into::into)
}
