//! Address-only recovery observations. The caller admits the footprint before IDs.
use anyhow::{ensure, Result};
use rusqlite::Transaction;

// The order is deliberate: message history must not drive a sparse active queue
// lookup, even before SQLite has collected statistics for the partial index.
const ACTIVE_FROM: &str = "queue_entries q CROSS JOIN messages m ON m.message_id=q.message_id";
const ACTIVE_WHERE: &str =
    "m.agent_id=?1 AND q.status NOT IN ('processed','aborted','dropped','quarantined')";

pub(crate) fn address_footprint(tx: &Transaction<'_>, agent: &str) -> Result<(usize, u64)> {
    Ok(tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(m.message_id AS BLOB))),0) FROM {ACTIVE_FROM} WHERE {ACTIVE_WHERE}"),
        [agent], |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub(crate) fn addresses(tx: &Transaction<'_>, agent: &str) -> Result<Vec<String>> {
    let corrupt: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {ACTIVE_FROM} WHERE {ACTIVE_WHERE} AND CASE WHEN json_valid(q.payload_json) THEN json_extract(q.payload_json,'$.message_id') IS NOT q.message_id OR json_extract(q.payload_json,'$.agent_id') IS NOT q.agent_id OR json_extract(q.payload_json,'$.status') IS NOT q.status ELSE 1 END)"),
        [agent], |row| row.get(0),
    )?;
    ensure!(
        !corrupt,
        "recovery Queue normalized observation differs from payload"
    );
    // Do not filter q.agent_id: a foreign Queue for this agent's Message is an
    // integrity error, not an address to silently omit.
    let foreign: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {ACTIVE_FROM} WHERE {ACTIVE_WHERE} AND q.agent_id<>?1)"
        ),
        [agent],
        |row| row.get(0),
    )?;
    ensure!(!foreign, "recovery Queue address belongs to another agent");
    let mut statement = tx.prepare(&format!(
        "SELECT m.message_id FROM {ACTIVE_FROM} WHERE {ACTIVE_WHERE} ORDER BY m.message_seq IS NULL ASC,m.message_seq DESC,m.created_at DESC,m.message_id DESC"
    ))?;
    let ids = statement
        .query_map([agent], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
#[path = "task_result_queue_tests.rs"]
mod tests;
