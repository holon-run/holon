//! Task Result settlement ledger on the caller's connection or transaction.

use anyhow::{ensure, Result};
use holon_domain::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementRecord,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::codec::timestamp;

const ADMITTED_FOR_ACTIVATION_SQL: &str = "SELECT payload_json
     FROM task_result_settlements
     WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'caller_admitted'
     ORDER BY created_at ASC, result_identity ASC";

pub fn has_result_source_column(connection: &Connection) -> Result<bool> {
    // Probe only metadata: SELECT * would also authorize protected body columns.
    match connection.prepare("SELECT command_creation_id FROM main.task_result_settlements LIMIT 0")
    {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(error, Some(message)))
        | Err(rusqlite::Error::SqlInputError {
            error,
            msg: message,
            ..
        }) if error.extended_code == rusqlite::ffi::SQLITE_ERROR
            && message == "no such column: command_creation_id" =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub fn latest_for_message_on(
    connection: &Connection,
    message_id: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    connection
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE message_id = ?1",
            params![message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(Into::into)
}

pub fn diagnostic_for_agent_message_on(
    connection: &Connection,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    connection
        .query_row(
            "SELECT payload_json FROM task_result_settlements
             WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(Into::into)
}

pub fn context_admitted_settlements_on(
    connection: &Connection,
    agent_id: &str,
    activation_id: &str,
) -> Result<Vec<TaskResultSettlementRecord>> {
    let mut statement = connection.prepare(ADMITTED_FOR_ACTIVATION_SQL)?;
    let rows = statement.query_map(params![agent_id, activation_id], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| {
        let payload = row?;
        serde_json::from_str(&payload).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(Into::into)
}

pub fn admitted_for_activation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation_id: &str,
) -> Result<Vec<TaskResultSettlementRecord>> {
    let mut statement = tx.prepare(ADMITTED_FOR_ACTIVATION_SQL)?;
    let rows = statement.query_map(params![agent_id, activation_id], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| serde_json::from_str(&row?).map_err(Into::into))
        .collect()
}

pub fn insert_pending_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let payload = serde_json::to_string(record)?;
    let has_source = has_result_source_column(tx)?;
    ensure!(
        has_source || record.source.parent_turn_id().is_some(),
        "Command creation Results require the creation-source schema"
    );
    let sql = if has_source {
        "INSERT INTO task_result_settlements (
           result_identity, agent_id, task_id, message_id, work_item_id,
           rejoin_generation, parent_turn_id, state, activation_id, disposition,
           created_at, updated_at, admitted_at, settled_at, deferred_reason, deferred_at, next_recheck_at, payload_json, command_creation_id
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
         )"
    } else {
        // Historical migration fixtures retain their original ParentTurn writer.
        "INSERT INTO task_result_settlements (
           result_identity, agent_id, task_id, message_id, work_item_id,
           rejoin_generation, parent_turn_id, state, activation_id, disposition,
           created_at, updated_at, admitted_at, settled_at, deferred_reason, deferred_at, next_recheck_at, payload_json
         ) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18
           WHERE ?19 IS NULL"
    };
    let inserted = tx.execute(
        sql,
        params![
            record.result_identity,
            record.agent_id,
            record.task_id,
            record.message_id,
            record.work_item_id,
            record.rejoin_generation,
            record.source.parent_turn_id().unwrap_or(""),
            record.state.as_str(),
            record.activation_id,
            record
                .disposition
                .map(TaskResultSettlementDisposition::as_str),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.admitted_at.map(timestamp),
            record.settled_at.map(timestamp),
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            payload,
            record.source.command_creation_id(),
        ],
    )?;
    Ok(inserted == 1)
}

pub fn update_tx(tx: &Transaction<'_>, record: &TaskResultSettlementRecord) -> Result<bool> {
    let payload = serde_json::to_string(record)?;
    let affected = tx.execute(
        "UPDATE task_result_settlements
         SET state = ?2, activation_id = ?3, disposition = ?4, updated_at = ?5,
             admitted_at = ?6, settled_at = ?7, deferred_reason = ?8,
           deferred_at = ?9, next_recheck_at = ?10, payload_json = ?11,
           recovery_epoch = ?12, recovery_attempts = ?13,
           last_recovery_root_attempt_id = ?14, last_recovery_admitted_at = ?15,
           recovery_paused_at = ?16, recovery_diagnostic_at = ?17
         WHERE result_identity = ?1",
        params![
            record.result_identity,
            record.state.as_str(),
            record.activation_id,
            record
                .disposition
                .map(TaskResultSettlementDisposition::as_str),
            timestamp(record.updated_at),
            record.admitted_at.map(timestamp),
            record.settled_at.map(timestamp),
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            payload,
            record.recovery_epoch,
            record.recovery_attempts,
            record.last_recovery_root_attempt_id,
            record.last_recovery_admitted_at.map(timestamp),
            record.recovery_paused_at.map(timestamp),
            record.recovery_diagnostic_at.map(timestamp),
        ],
    )?;
    Ok(affected == 1)
}

/// Narrowed UPDATE for the deferred-recheck hot path. The recovery loop
/// rewrites the same due rows every recheck interval; a full-column UPDATE
/// touches `state`/`activation_id` and churns every index referencing them
/// even though those values never change on deferral.
pub fn defer_update_tx(tx: &Transaction<'_>, record: &TaskResultSettlementRecord) -> Result<bool> {
    let payload = serde_json::to_string(record)?;
    let affected = tx.execute(
        "UPDATE task_result_settlements
         SET deferred_reason = ?2, deferred_at = ?3, next_recheck_at = ?4,
             updated_at = ?5, payload_json = ?6
         WHERE result_identity = ?1",
        params![
            record.result_identity,
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            timestamp(record.updated_at),
            payload,
        ],
    )?;
    Ok(affected == 1)
}

#[cfg(test)]
mod tests;
