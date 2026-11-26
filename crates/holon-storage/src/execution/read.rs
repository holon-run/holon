//! Execution protocol state reads on the caller's connection.

use std::{collections::BTreeMap, io};

use anyhow::{anyhow, Context, Result};
use holon_domain::execution_protocol::{
    self, ExecutionOutcomeRecord, ExecutionProtocolState, WorkItemExecutionRecord,
};
use rusqlite::{Connection, Transaction};
use serde::de::DeserializeOwned;

use super::{decode_outcome_record_with_migration, project_legacy_in_flight_work_item};
use crate::codec::borrowed_sql_text;

/// Caller's read scope charges map metadata and admits JSON before decoding.
#[derive(Clone, Copy)]
pub struct ExecutionReadMaterial {
    pub charge_owned_metadata: fn(u64, usize) -> io::Result<()>,
    pub admit_typed_json_payload: fn(&str) -> Result<()>,
}

pub fn partition_exists_on(connection: &Connection, agent_id: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
           SELECT 1 FROM execution_protocol_partitions WHERE agent_id = ?1
         )",
            [agent_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

/// The pre-v47 offline fallback can open a database before schema migrations.
/// Project its retired rows as Runnable only for the repair's before-image;
/// the fallback itself advances those generations when it writes the repair.
///
/// The revision marker is rewritten together with the payload: a row projected
/// to a canonical `runnable` record must not still claim the retired
/// `in_flight` lifecycle, or the shared pre-migration decoder sees a marker and
/// payload that disagree (#4571).
pub fn load_retired_scheduler_state_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    material: ExecutionReadMaterial,
) -> Result<ExecutionProtocolState> {
    load_state_unchecked_with_work_items_on(
        tx,
        agent_id,
        "SELECT work_item_id,
                CASE WHEN lifecycle_state = 'in_flight'
                     THEN json_set(payload_json, '$.state', json_object(
                          'state', 'runnable', 'generation', generation))
                     ELSE payload_json END,
                source_revision||':'||generation||':'||
                CASE WHEN lifecycle_state = 'in_flight'
                     THEN 'runnable' ELSE lifecycle_state END
         FROM execution_protocol_work_items WHERE agent_id = ?1",
        material,
    )
}

/// Load rows without checking the partition marker or cross-row invariants.
///
/// The repair caller validates the repaired state before committing.
pub fn load_state_unchecked_on(
    connection: &Connection,
    agent_id: &str,
    material: ExecutionReadMaterial,
) -> Result<ExecutionProtocolState> {
    load_state_unchecked_with_work_items_on(
        connection,
        agent_id,
        "SELECT work_item_id, payload_json, source_revision||':'||generation||':'||lifecycle_state
         FROM execution_protocol_work_items WHERE agent_id = ?1",
        material,
    )
}

fn load_state_unchecked_with_work_items_on(
    connection: &Connection,
    agent_id: &str,
    work_items_sql: &str,
    material: ExecutionReadMaterial,
) -> Result<ExecutionProtocolState> {
    Ok(ExecutionProtocolState {
        agent_id: agent_id.to_owned(),
        attempts: load_payload_map(
            connection,
            "SELECT attempt_id, payload_json
             FROM execution_protocol_attempts WHERE agent_id = ?1",
            agent_id,
            material,
        )?,
        work_items: load_work_items_with_migration(connection, agent_id, work_items_sql)?,
        outcomes: load_outcomes_with_migration(connection, agent_id)?,
    })
}

/// Load the live Work authority for an initialized partition.
pub fn load_live_work_items_on(
    connection: &Connection,
    agent_id: &str,
) -> Result<BTreeMap<String, WorkItemExecutionRecord>> {
    load_work_items_with_migration(
        connection,
        agent_id,
        "SELECT work_item_id, payload_json, source_revision||':'||generation||':'||lifecycle_state
         FROM execution_protocol_work_items
         WHERE agent_id=?1 AND lifecycle_state!='terminal'
           AND EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id=?1)",
    )
}

fn load_work_items_with_migration(
    tx: &Connection,
    agent_id: &str,
    sql: &str,
) -> Result<BTreeMap<String, WorkItemExecutionRecord>> {
    let mut statement = tx.prepare(sql)?;
    let mut rows = statement.query([agent_id])?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id = borrowed_sql_text(row, 0)?.to_owned();
        let payload = borrowed_sql_text(row, 1)?;
        let marker = borrowed_sql_text(row, 2)?;
        let (record, legacy_projection) =
            project_legacy_in_flight_work_item(tx, agent_id, &id, payload, marker)
                .with_context(|| format!("decoding execution protocol record {}", id))?;
        if let Some(evidence) = legacy_projection {
            tracing::info!(
                agent_id = %evidence.agent_id,
                work_item_id = %evidence.work_item_id,
                lifecycle = %evidence.original_lifecycle,
                attempt_id = %evidence.attempt_id,
                matching_open_attempt = evidence.matching_open_attempt,
                wait_source = ?evidence.wait_source,
                wait_id = evidence.wait_id.as_deref(),
                generation_before = evidence.generation_before,
                generation_after = evidence.generation_after,
                "projected legacy in_flight work item from a pre-migration database"
            );
        }
        records.insert(id, record);
    }
    Ok(records)
}

fn load_outcomes_with_migration(
    tx: &Connection,
    agent_id: &str,
) -> Result<BTreeMap<String, ExecutionOutcomeRecord>> {
    let mut statement = tx.prepare(
        "SELECT outcome_id, payload_json
         FROM execution_protocol_outcomes WHERE agent_id = ?1",
    )?;
    let mut rows = statement.query([agent_id])?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id = borrowed_sql_text(row, 0)?.to_owned();
        let payload = borrowed_sql_text(row, 1)?;
        let record = decode_outcome_record_with_migration(payload)
            .with_context(|| format!("decoding execution protocol record {}", id))?;
        records.insert(id, record);
    }
    Ok(records)
}

/// Load and validate the state of an initialized partition on this connection.
pub fn load_state_on(
    connection: &Connection,
    agent_id: &str,
    material: ExecutionReadMaterial,
) -> Result<Option<ExecutionProtocolState>> {
    if !partition_exists_on(connection, agent_id)? {
        return Ok(None);
    }
    let state = load_state_unchecked_on(connection, agent_id, material)?;
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution protocol state is invalid: {error}"))?;
    Ok(Some(state))
}

pub(super) fn load_payload_map<T: DeserializeOwned>(
    tx: &Connection,
    sql: &str,
    agent_id: &str,
    material: ExecutionReadMaterial,
) -> Result<BTreeMap<String, T>> {
    let mut statement = tx.prepare(sql)?;
    let mut rows = statement.query([agent_id])?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id = borrowed_sql_text(row, 0)?;
        let payload = borrowed_sql_text(row, 1)?;
        let metadata = id
            .len()
            .checked_add(std::mem::size_of::<(String, T)>())
            .and_then(|bytes| bytes.checked_add(64))
            .context("execution source map metadata overflow")?;
        (material.charge_owned_metadata)(u64::try_from(metadata)?, 1)?;
        (material.admit_typed_json_payload)(payload)?;
        let value = serde_json::from_str(payload)
            .with_context(|| format!("decoding execution protocol record {id}"))?;
        records.insert(id.to_owned(), value);
    }
    Ok(records)
}
