use holon_domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};
use rusqlite::Connection;

use super::*;
use crate::execution::{load_committed_work_item_tx, partition_exists_on};
use crate::migrations::MIGRATIONS;

const AGENT: &str = "agent-a";
const KIND: &str = "reconcile_work_item_source_revision";

fn database() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.name == "execution_protocol_authority")
        .unwrap();
    connection.execute_batch(migration.sql)?;
    Ok(connection)
}

fn runnable(source_revision: u64) -> WorkItemExecutionRecord {
    WorkItemExecutionRecord {
        source_revision,
        state: WorkItemExecutionState::Runnable {
            generation: 1,
            recovery_ref: None,
        },
    }
}

fn receipt(id: &str, hash: &str) -> ExecutionCommandReceipt {
    ExecutionCommandReceipt {
        command_kind: KIND,
        command_identity: id.into(),
        payload_hash: hash.into(),
        references: vec!["changed".into()],
    }
}

#[test]
fn command_batch_reads_back_changed_rows_and_keeps_legacy_bytes() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let mut state = ExecutionProtocolState::empty(AGENT);
    state.work_items.insert("changed".into(), runnable(1));
    state.work_items.insert("legacy".into(), runnable(1));
    persist_command_batch_tx(&tx, &state, &|_, _| true, &[])?;
    let legacy = r#"{ "source_revision": 1, "state": {"state":"runnable","generation":1} }"#;
    tx.execute(
        "UPDATE execution_protocol_work_items SET payload_json = ?1
         WHERE agent_id = ?2 AND work_item_id = 'legacy'",
        params![legacy, AGENT],
    )?;

    state.work_items.insert("changed".into(), runnable(2));
    let result = receipt("revision-2", "hash-2");
    persist_command_batch_tx(
        &tx,
        &state,
        &|kind, id| matches!(kind, ExecutionRowKind::WorkItem) && id == "changed",
        &[result],
    )?;
    assert_eq!(
        load_committed_work_item_tx(&tx, AGENT, "changed")?,
        runnable(2)
    );
    let stored: String = tx.query_row(
        "SELECT payload_json FROM execution_protocol_work_items
         WHERE agent_id = ?1 AND work_item_id = 'legacy'",
        [AGENT],
        |row| row.get(0),
    )?;
    assert_eq!(stored, legacy);
    tx.commit()?;

    let tx = connection.transaction()?;
    let repair = receipt("revision-2", "offline-hash");
    insert_repair_command_receipt_tx(&tx, AGENT, &repair)?;
    assert_eq!(
        stored_command_hash_tx(&tx, AGENT, KIND, "revision-2")?,
        Some("hash-2".into()),
    );
    tx.commit()?;
    Ok(())
}

#[test]
fn duplicate_strict_receipt_rolls_back_state_and_earlier_receipts() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let mut state = ExecutionProtocolState::empty(AGENT);
    state.work_items.insert("changed".into(), runnable(1));
    persist_command_batch_tx(
        &tx,
        &state,
        &|_, _| true,
        &[receipt("recorded", "original-hash")],
    )?;
    tx.commit()?;

    state.work_items.insert("changed".into(), runnable(2));
    let tx = connection.transaction()?;
    let new = receipt("new", "new-hash");
    let duplicate = receipt("recorded", "conflict");
    assert!(persist_command_batch_tx(&tx, &state, &|_, _| true, &[new, duplicate]).is_err());
    assert_eq!(
        load_committed_work_item_tx(&tx, AGENT, "changed")?,
        runnable(2)
    );
    assert_eq!(
        stored_command_hash_tx(&tx, AGENT, KIND, "new")?,
        Some("new-hash".into()),
    );
    tx.rollback()?;

    let tx = connection.transaction()?;
    assert_eq!(
        load_committed_work_item_tx(&tx, AGENT, "changed")?,
        runnable(1)
    );
    assert_eq!(stored_command_hash_tx(&tx, AGENT, KIND, "new")?, None);
    assert_eq!(
        stored_command_hash_tx(&tx, AGENT, KIND, "recorded")?,
        Some("original-hash".into()),
    );
    Ok(())
}

#[test]
fn failed_state_write_leaves_no_receipt_or_partition_after_rollback() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let mut state = ExecutionProtocolState::empty(AGENT);
    state
        .work_items
        .insert("changed".into(), runnable(u64::MAX));
    let result = receipt("invalid", "hash");
    assert!(persist_command_batch_tx(&tx, &state, &|_, _| true, &[result]).is_err());
    assert!(partition_exists_on(&tx, AGENT)?);
    assert_eq!(stored_command_hash_tx(&tx, AGENT, KIND, "invalid")?, None);
    tx.rollback()?;
    assert!(!partition_exists_on(&connection, AGENT)?);
    Ok(())
}
