//! The immutable input mutation receipt ledger on the caller's SQLite view.

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use holon_domain::{
    input_mutation_receipt::{PreparedInputMutation, ReceiptError},
    InputEvidenceScope,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

pub struct StoredInputMutationReceipt {
    pub prepared: PreparedInputMutation,
    pub outcome_json: String,
    pub committed_at: String,
    pub result_digest: String,
}

/// Verify the stored command identity and complete scope group before the
/// caller decodes and checks the original business outcome.
pub fn lookup_receipt_on(
    connection: &Connection,
    prepared: &PreparedInputMutation,
) -> Result<Option<StoredInputMutationReceipt>> {
    prepared.validate()?;
    type Row = (
        String,
        u32,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT family, phase_version, command_digest, scope_digest, scopes_json,
                    prepared_at, prepared_json, outcome_json, committed_at, result_digest
             FROM input_mutation_receipts WHERE command_nonce = ?1",
            [prepared.command_nonce()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        family,
        version,
        digest,
        scope_digest,
        scopes_json,
        prepared_at,
        prepared_json,
        outcome_json,
        committed_at,
        result_digest,
    )) = row
    else {
        return Ok(None);
    };
    let stored: PreparedInputMutation = serde_json::from_str(&prepared_json)
        .context("decoding original input mutation descriptor")?;
    stored.validate()?;
    let scopes: Vec<InputEvidenceScope> = serde_json::from_str(&scopes_json)?;
    ensure!(
        stored.family().as_str() == family
            && stored.phase_version() == version
            && stored.command_nonce() == prepared.command_nonce()
            && stored.command_digest() == digest
            && stored.scope_digest() == scope_digest
            && stored.scopes() == scopes.as_slice()
            && timestamp(stored.prepared_at()) == prepared_at,
        ReceiptError::CorruptReceipt("normalized descriptor differs from payload")
    );
    ensure!(
        stored.scopes() == prepared.scopes(),
        ReceiptError::GroupConflict
    );
    ensure!(stored == *prepared, ReceiptError::CommandConflict);
    Ok(Some(StoredInputMutationReceipt {
        prepared: stored,
        outcome_json,
        committed_at,
        result_digest,
    }))
}

/// Insert the already validated receipt alongside the caller's business writes.
pub fn insert_receipt_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputMutation,
    outcome_json: &str,
    committed_at: DateTime<Utc>,
    result_digest: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO input_mutation_receipts (
            command_nonce, family, phase_version, command_digest, scope_digest,
            scopes_json, prepared_at, prepared_json, outcome_json, committed_at, result_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            prepared.command_nonce(),
            prepared.family().as_str(),
            prepared.phase_version(),
            prepared.command_digest(),
            prepared.scope_digest(),
            serde_json::to_string(prepared.scopes())?,
            timestamp(prepared.prepared_at()),
            serde_json::to_string(prepared)?,
            outcome_json,
            timestamp(committed_at),
            result_digest,
        ],
    )?;
    Ok(())
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests;
