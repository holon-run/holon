//! Stable SQLite sequence scope keys.

pub fn agent_sequence_scope(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}

pub fn audit_event_sequence_scope(agent_id: Option<&str>) -> String {
    agent_id.map_or_else(|| "host".to_string(), agent_sequence_scope)
}

use anyhow::{Context, Result};
use rusqlite::{params, Transaction};

pub fn allocate_runtime_sequence_tx(
    tx: &Transaction<'_>,
    domain: &str,
    scope_key: &str,
) -> Result<u64> {
    let value = tx.query_row(
        "INSERT INTO runtime_sequences(domain, scope_key, last_value)
         VALUES (?1, ?2, 1)
         ON CONFLICT(domain, scope_key) DO UPDATE SET
           last_value = runtime_sequences.last_value + 1
         RETURNING last_value",
        params![domain, scope_key],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(value).context("runtime sequence is negative")
}

pub fn observe_runtime_sequence_tx(
    tx: &Transaction<'_>,
    domain: &str,
    scope_key: &str,
    value: u64,
) -> Result<()> {
    let value = i64::try_from(value).context("runtime sequence exceeds SQLite integer range")?;
    tx.execute(
        "INSERT INTO runtime_sequences(domain, scope_key, last_value)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(domain, scope_key) DO UPDATE SET
           last_value = MAX(runtime_sequences.last_value, excluded.last_value)",
        params![domain, scope_key, value],
    )?;
    Ok(())
}
