//! Full invariant validation with bounded row ownership for memory closure.
use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use holon_domain::execution_protocol::{
    self, ExecutionAttempt, ExecutionAttemptState, ExecutionOutcomeRecord, WorkItemExecutionRecord,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;

use super::{decode_outcome_record_with_migration, read::load_payload_map, ExecutionReadMaterial};
use crate::codec::borrowed_sql_text;

pub struct ValidatedClosureProtocol {
    agent_id: String,
    work_items: BTreeMap<String, WorkItemExecutionRecord>,
}
impl ValidatedClosureProtocol {
    pub fn into_work_items(
        self,
        agent_id: &str,
    ) -> Result<BTreeMap<String, WorkItemExecutionRecord>> {
        anyhow::ensure!(
            self.agent_id == agent_id,
            "closure protocol projection crossed Agent scope"
        );
        Ok(self.work_items)
    }
}

/// SQL sizes precede every decode. History remains subject to the original
/// inventory bound; only the Work map and individually validated rows are owned.
pub fn closure_protocol_material_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<(usize, usize)> {
    let (works, work_bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(work_item_id AS BLOB))),0) FROM execution_protocol_work_items WHERE agent_id=?1",
        [agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut inventory_records = works;
    let mut inventory_bytes = work_bytes;
    let mut bytes = work_bytes
        .checked_mul(4)
        .and_then(|n| {
            n.checked_add(agent_id.len() + std::mem::size_of::<ValidatedClosureProtocol>())
        })
        .context("closure Work material overflow")?;
    for (table, id) in [
        ("execution_protocol_attempts", "attempt_id"),
        ("execution_protocol_outcomes", "outcome_id"),
    ] {
        let (count, total): (usize, usize) = tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM {table} WHERE agent_id=?1"), [agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        inventory_records = inventory_records
            .checked_add(count)
            .context("closure inventory overflow")?;
        inventory_bytes = inventory_bytes
            .checked_add(total)
            .context("closure inventory overflow")?;

        let peak: usize = tx.query_row(&format!("SELECT COALESCE(MAX(length(CAST(payload_json AS BLOB))+length(CAST({id} AS BLOB))),0) FROM {table} WHERE agent_id=?1"), [agent_id], |r| r.get(0))?;
        bytes = bytes
            .checked_add(
                peak.checked_mul(2)
                    .context("closure row material overflow")?,
            )
            .context("closure material overflow")?;
    }
    Ok((
        bytes,
        works
            .checked_mul(4)
            .and_then(|n| n.checked_add(4))
            .context("closure record material overflow")?,
    ))
}

fn related<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
    id: &str,
) -> Result<Option<T>> {
    tx.query_row(sql, params![agent_id, id], |row| {
        let raw = borrowed_sql_text(row, 0)?;
        serde_json::from_str(raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .optional()
    .map_err(Into::into)
}

/// Outcome rows decode through the single migration-aware entry; a bare
/// `related` decode would reject the retired WorkItem `needs_repair` encoding
/// that the closure read must tolerate.
fn related_outcome(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
    id: &str,
) -> Result<Option<ExecutionOutcomeRecord>> {
    let payload: Option<String> = tx
        .query_row(sql, params![agent_id, id], |row| {
            borrowed_sql_text(row, 0).map(str::to_owned)
        })
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    Ok(Some(decode_outcome_record_with_migration(&payload)?))
}

/// Stream validated history rows after the caller has admitted the read material.
pub fn read_closure_rows_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    material: ExecutionReadMaterial,
) -> Result<ValidatedClosureProtocol> {
    // One map survives the traversal. Each decoded history row and its one
    // reciprocal row die before advancing the cursor; no history Vec/map grows.
    let work_items = load_payload_map(
        tx,
        "SELECT work_item_id,payload_json FROM execution_protocol_work_items WHERE agent_id=?1",
        agent_id,
        material,
    )?;
    let mut attempts = tx.prepare(
        "SELECT attempt_id,payload_json FROM execution_protocol_attempts WHERE agent_id=?1",
    )?;
    let mut rows = attempts.query([agent_id])?;
    let mut open = 0usize;
    while let Some(row) = rows.next()? {
        let id = borrowed_sql_text(row, 0)?;
        let attempt: ExecutionAttempt = serde_json::from_str(borrowed_sql_text(row, 1)?)?;
        if attempt.state == ExecutionAttemptState::Open {
            open += 1;
            if open > 1 {
                bail!("execution partition contains more than one open attempt");
            }
        }
        let outcome: Option<ExecutionOutcomeRecord> = attempt.terminal_outcome_id.as_deref().map(|id|
            related_outcome(tx, "SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2", agent_id, id)
        ).transpose()?.flatten();
        execution_protocol::validate_attempt_invariants(
            agent_id,
            id,
            &attempt,
            &work_items,
            outcome.as_ref(),
        )
        .map_err(anyhow::Error::msg)?;
    }
    drop(rows);
    for work in work_items.values() {
        execution_protocol::validate_work_invariants(work).map_err(anyhow::Error::msg)?;
    }
    let mut outcomes = tx.prepare(
        "SELECT outcome_id,payload_json FROM execution_protocol_outcomes WHERE agent_id=?1",
    )?;
    let mut rows = outcomes.query([agent_id])?;
    while let Some(row) = rows.next()? {
        let id = borrowed_sql_text(row, 0)?;
        let outcome = decode_outcome_record_with_migration(borrowed_sql_text(row, 1)?)?;
        let attempt: Option<ExecutionAttempt> = related(tx,
            "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2", agent_id, &outcome.attempt_id)?;
        execution_protocol::validate_outcome_invariants(id, &outcome, attempt.as_ref())
            .map_err(anyhow::Error::msg)?;
    }
    Ok(ValidatedClosureProtocol {
        agent_id: agent_id.to_owned(),
        work_items,
    })
}

#[cfg(test)]
mod tests;
