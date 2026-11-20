//! Durable Wait rows shared by runtime writes and SQLite tests.

use anyhow::{Context, Result};
use holon_domain::{WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource};
use rusqlite::{params, Statement, Transaction};

use crate::codec::{parse_optional_timestamp, parse_timestamp};

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

/// Convert an [`anyhow::Error`] from row decoding into a [`rusqlite::Error`]
/// that preserves the original message. Previously these errors were mapped to
/// [`rusqlite::Error::InvalidQuery`] whose Display text ("Query is not
/// read-only") is completely unrelated to the actual failure (e.g. a malformed
/// timestamp or invalid enum value), making diagnosis nearly impossible.
pub fn wait_condition_decode_error(e: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{e:#}").into(),
    )
}

pub fn decode_wait_condition_row(row: &rusqlite::Row<'_>) -> Result<WaitConditionRecord> {
    let id: String = row.get(0)?;
    let agent_id: String = row.get(1)?;
    let work_item_id: Option<String> = row.get(2)?;
    let status_str: String = row.get(3)?;
    let kind_str: String = row.get(4)?;
    let source: Option<String> = row.get(5)?;
    let subject_ref: Option<String> = row.get(6)?;
    let waiting_for: String = row.get(7)?;
    let created_at_str: String = row.get(8)?;
    let updated_at_str: String = row.get(9)?;
    let expires_at_str: Option<String> = row.get(10)?;
    let resolved_at_str: Option<String> = row.get(11)?;
    let cancelled_at_str: Option<String> = row.get(12)?;
    let turn_id: Option<String> = row.get(13)?;
    let trigger_message_id: Option<String> = row.get(14)?;
    let triggered_at_str: Option<String> = row.get(15)?;
    let wake_sources_json: String = row.get(16)?;
    let continuation_json: Option<String> = row.get(17)?;
    let status: WaitConditionStatus = serde_json::from_value(serde_json::Value::String(status_str))
        .context("parsing wait condition status")?;
    let kind: WaitConditionKind = serde_json::from_value(serde_json::Value::String(kind_str))
        .context("parsing wait condition kind")?;
    let wake_sources: Vec<WakeSource> =
        serde_json::from_str(&wake_sources_json).context("parsing wake_sources_json")?;
    let continuation: Option<serde_json::Value> = continuation_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("parsing continuation_json")?;
    Ok(WaitConditionRecord {
        id,
        agent_id,
        work_item_id,
        status,
        kind,
        source,
        subject_ref,
        waiting_for,
        wake_sources,
        continuation,
        created_at: parse_timestamp(&created_at_str)?,
        updated_at: parse_timestamp(&updated_at_str)?,
        expires_at: parse_optional_timestamp(expires_at_str.as_deref())?,
        resolved_at: parse_optional_timestamp(resolved_at_str.as_deref())?,
        cancelled_at: parse_optional_timestamp(cancelled_at_str.as_deref())?,
        turn_id,
        trigger_message_id,
        triggered_at: parse_optional_timestamp(triggered_at_str.as_deref())?,
    })
}
