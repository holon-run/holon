//! SQLite ledger and chain budgets for Agent message deliveries.

use anyhow::{Context, Result};
use chrono::Utc;
use holon_domain::{AgentMessageDeliveryRecord, AgentMessageDeliveryState};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::codec::{enum_string, timestamp};

/// Fixed allowance for one chain, including the first accepted delivery.
pub const MESSAGE_CHAIN_MAX_DELIVERIES: i64 = 16;

pub fn delivery_rowid_on(connection: &Connection, delivery_id: &str) -> rusqlite::Result<i64> {
    connection.query_row(
        "SELECT rowid FROM agent_message_deliveries WHERE delivery_id = ?1",
        [delivery_id],
        |row| row.get(0),
    )
}

pub fn delivery_by_id(
    connection: &Connection,
    delivery_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    connection
        .query_row(
            "SELECT payload_json
             FROM agent_message_deliveries
             WHERE delivery_id = ?1",
            [delivery_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_delivery(&payload))
        .transpose()
}

pub fn delivery_by_idempotency_scope_tx(
    tx: &Transaction<'_>,
    idempotency_scope: &str,
    idempotency_key_digest: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE idempotency_scope = ?1 AND idempotency_key_digest = ?2",
        params![idempotency_scope, idempotency_key_digest],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_delivery(&payload))
    .transpose()
}

pub fn delivery_by_message_id_on(
    connection: &Connection,
    message_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    connection
        .query_row(
            "SELECT payload_json
         FROM agent_message_deliveries
         WHERE message_id = ?1",
            [message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_delivery(&payload))
        .transpose()
}

/// Scan in admission order and decode only through the first match.
pub fn first_accepted_delivery_after_on(
    connection: &Connection,
    target_agent_id: &str,
    after_rowid: i64,
    matches: impl Fn(&AgentMessageDeliveryRecord) -> bool,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE rowid > ?1
           AND target_agent_id = ?2
           AND outcome = 'accepted'
         ORDER BY rowid ASC",
    )?;
    let rows = statement.query_map(params![after_rowid, target_agent_id], |row| {
        row.get::<_, String>(0)
    })?;
    for payload in rows {
        let record = decode_delivery(&payload?)?;
        if matches(&record) {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

pub fn active_deliveries_for_target_tx(
    tx: &Transaction<'_>,
    target_agent_id: &str,
) -> Result<Vec<AgentMessageDeliveryRecord>> {
    let queued = enum_string(&AgentMessageDeliveryState::Queued)?;
    let dispatched = enum_string(&AgentMessageDeliveryState::Dispatched)?;
    let mut statement = tx.prepare(
        "SELECT payload_json
             FROM agent_message_deliveries
             WHERE target_agent_id = ?1 AND state IN (?2, ?3)
             ORDER BY created_at ASC, delivery_id ASC",
    )?;
    let rows = statement.query_map(params![target_agent_id, queued, dispatched], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|payload| decode_delivery(&payload))
        .collect()
}

pub fn insert_delivery_tx(tx: &Transaction<'_>, record: &AgentMessageDeliveryRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO agent_message_deliveries (
           delivery_id, target_agent_id, message_id, idempotency_scope,
           idempotency_key_digest, request_digest, outcome, state, state_version,
           rejection_code, retryable, accepted_at, terminal_at, created_at,
           updated_at, payload_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
         )",
        params![
            record.delivery_id,
            record.target_agent_id,
            record.message_id,
            record.idempotency_scope,
            record.idempotency_key_digest,
            record.request_digest,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub fn replace_delivery_tx(
    tx: &Transaction<'_>,
    record: &AgentMessageDeliveryRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET message_id = ?1, outcome = ?2, state = ?3, state_version = ?4,
             rejection_code = ?5, retryable = ?6, accepted_at = ?7,
             terminal_at = ?8, updated_at = ?9, payload_json = ?10
         WHERE delivery_id = ?11",
        params![
            record.message_id,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.updated_at),
            payload_json,
            record.delivery_id,
        ],
    )?;
    anyhow::ensure!(changed == 1, "delivery {} not found", record.delivery_id);
    Ok(())
}

pub fn compare_and_set_delivery_state_tx(
    tx: &Transaction<'_>,
    delivery_id: &str,
    expected_state: AgentMessageDeliveryState,
    next: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    let payload_json = serde_json::to_string(next)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET state = ?1, state_version = ?2, terminal_at = ?3,
             updated_at = ?4, payload_json = ?5
         WHERE delivery_id = ?6 AND state = ?7 AND state_version = ?8",
        params![
            enum_string(&next.state)?,
            i64::try_from(next.state_version).context("delivery state version exceeds SQLite")?,
            next.terminal_at.map(timestamp),
            timestamp(next.updated_at),
            payload_json,
            delivery_id,
            enum_string(&expected_state)?,
            i64::try_from(next.state_version.saturating_sub(1))
                .context("delivery state version exceeds SQLite")?,
        ],
    )?;
    Ok(changed == 1)
}

pub fn advance_delivery_state_for_message_tx(
    tx: &Transaction<'_>,
    message_id: &str,
    next_state: AgentMessageDeliveryState,
    diagnostic: Option<&str>,
) -> Result<bool> {
    let Some(mut record) = delivery_by_message_id_on(tx, message_id)? else {
        return Ok(false);
    };
    let expected_state = record.state;
    if !record.advance_state(next_state, Utc::now(), diagnostic) {
        return Ok(false);
    }
    compare_and_set_delivery_state_tx(tx, &record.delivery_id, expected_state, &record)
}

pub fn chain_budget_exists_tx(tx: &Transaction<'_>, chain_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_message_chain_budgets WHERE chain_id = ?1)",
        [chain_id],
        |row| row.get(0),
    )
}

/// Check the limit before any target queue or wait writes in the admission transaction.
pub fn exhausted_chain_limit_tx(tx: &Transaction<'_>, chain_id: &str) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT max_deliveries FROM agent_message_chain_budgets
             WHERE chain_id = ?1 AND accepted_count = max_deliveries",
            [chain_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Charge only after the queue write applies, in the same transaction that
/// checked the limit and will commit the delivery ledger.
pub fn charge_chain_budget_tx(tx: &Transaction<'_>, chain_id: &str) -> Result<()> {
    let changed = tx.execute(
        "UPDATE agent_message_chain_budgets
         SET accepted_count = accepted_count + 1
         WHERE chain_id = ?1 AND accepted_count < max_deliveries",
        [chain_id],
    )?;
    if changed == 1 {
        return Ok(());
    }
    // A compatibility anchor already stands for one accepted delivery.
    let compatibility_anchor: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries
         WHERE delivery_id = ?1
           AND json_extract(payload_json, '$.outcome') = 'accepted'
           AND json_extract(payload_json, '$.message_chain_id') IS NULL)",
        [chain_id],
        |row| row.get(0),
    )?;
    let initial_count = if compatibility_anchor { 2 } else { 1 };
    tx.execute(
        "INSERT INTO agent_message_chain_budgets
           (chain_id, max_deliveries, accepted_count, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            chain_id,
            MESSAGE_CHAIN_MAX_DELIVERIES,
            initial_count,
            timestamp(Utc::now()),
        ],
    )?;
    Ok(())
}

fn decode_delivery(payload: &str) -> Result<AgentMessageDeliveryRecord> {
    serde_json::from_str(payload).context("decoding agent message delivery payload")
}

#[cfg(test)]
mod tests;
