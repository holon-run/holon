//! Durable receipts and state writes for validated execution commands.

use anyhow::Result;
use chrono::Utc;
use holon_domain::execution_protocol::ExecutionProtocolState;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{persist_state_rows_tx, ExecutionRowKind};

pub struct ExecutionCommandReceipt {
    pub command_kind: &'static str,
    pub command_identity: String,
    pub payload_hash: String,
    pub references: Vec<String>,
}

pub fn stored_command_hash_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command_kind: &str,
    command_identity: &str,
) -> Result<Option<String>> {
    tx.query_row(
        "SELECT payload_hash
         FROM execution_protocol_command_results
         WHERE agent_id = ?1
           AND command_kind = ?2
           AND command_identity = ?3",
        params![agent_id, command_kind, command_identity],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Write selected state rows and strict receipts in the caller's transaction.
/// The caller validates the commands and rolls the transaction back on error.
pub fn persist_command_batch_tx(
    tx: &Transaction<'_>,
    state: &ExecutionProtocolState,
    includes: &dyn Fn(ExecutionRowKind, &str) -> bool,
    receipts: &[ExecutionCommandReceipt],
) -> Result<()> {
    persist_state_rows_tx(tx, state, None, includes)?;
    for receipt in receipts {
        insert_command_receipt_tx(
            tx,
            "INSERT INTO execution_protocol_command_results (
               agent_id, command_kind, command_identity,
               payload_hash, references_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &state.agent_id,
            receipt,
        )?;
    }
    Ok(())
}

/// Offline repair retains any existing receipt for the same command identity.
pub fn insert_repair_command_receipt_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    receipt: &ExecutionCommandReceipt,
) -> Result<()> {
    insert_command_receipt_tx(
        tx,
        "INSERT OR IGNORE INTO execution_protocol_command_results (
           agent_id, command_kind, command_identity,
           payload_hash, references_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        agent_id,
        receipt,
    )
}

fn insert_command_receipt_tx(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
    receipt: &ExecutionCommandReceipt,
) -> Result<()> {
    tx.execute(
        sql,
        params![
            agent_id,
            receipt.command_kind,
            receipt.command_identity,
            receipt.payload_hash,
            serde_json::to_string(&receipt.references)?,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
