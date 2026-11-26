use super::*;
use holon_domain::{
    execution_protocol::TaskRejoinSource, task_result_settlement::TaskResultSettlementState,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

use crate::migrations::MIGRATIONS;

#[test]
fn result_source_column_probe_preserves_legacy_without_reading_bodies() -> Result<()> {
    for (columns, expected) in [
        ("payload_json TEXT", false),
        ("payload_json TEXT, command_creation_id TEXT", true),
    ] {
        let connection = rusqlite::Connection::open_in_memory()?;
        connection.execute_batch(&format!("CREATE TABLE task_result_settlements ({columns})"))?;
        connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "task_result_settlements",
                column_name: "payload_json",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }));
        assert_eq!(has_result_source_column(&connection)?, expected);
    }
    Ok(())
}

#[test]
fn result_source_column_probe_propagates_authorizer_errors() -> Result<()> {
    let connection = rusqlite::Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE task_result_settlements (command_creation_id TEXT)")?;
    connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "task_result_settlements",
            column_name: "command_creation_id",
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }));
    let error = has_result_source_column(&connection).unwrap_err();
    assert!(
        error.to_string().contains("prohibited"),
        "unexpected probe error: {error:#}"
    );
    Ok(())
}

#[test]
fn result_source_column_probe_propagates_missing_table_errors() -> Result<()> {
    let connection = rusqlite::Connection::open_in_memory()?;
    let error = has_result_source_column(&connection).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no such table: main.task_result_settlements"),
        "unexpected probe error: {error:#}"
    );
    Ok(())
}

fn settlement_database() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    // Apply the migrations that create and rebuild this ledger up to the
    // version before the command-creation source column is added.
    for version in [65, 69, 79, 83, 84] {
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
            .unwrap();
        connection.execute_batch(migration.sql)?;
    }
    Ok(connection)
}

fn pending(id: &str) -> Result<TaskResultSettlementRecord> {
    Ok(serde_json::from_value(serde_json::json!({
        "result_identity": id,
        "agent_id": "agent-a",
        "task_id": "task-a",
        "message_id": format!("message-{id}"),
        "work_item_id": "work-a",
        "rejoin_generation": 1,
        "parent_turn_id": "turn-a",
        "task_status": "completed",
        "state": "persisted_pending",
        "created_at": "2026-11-26T00:00:00Z",
        "updated_at": "2026-11-26T00:00:00Z",
    }))?)
}

#[test]
fn pending_insert_preserves_legacy_and_current_source_encodings() -> Result<()> {
    let mut connection = settlement_database()?;
    let tx = connection.transaction()?;
    let parent = pending("parent")?;
    let mut command = pending("command")?;
    command.source = TaskRejoinSource::CommandCreation {
        command_creation_id: command.task_id.clone(),
    };
    assert!(insert_pending_tx(&tx, &parent)?);
    assert_eq!(
        latest_for_message_on(&tx, &parent.message_id)?,
        Some(parent)
    );
    let error = insert_pending_tx(&tx, &command).unwrap_err();
    assert!(error
        .to_string()
        .contains("require the creation-source schema"));
    assert!(latest_for_message_on(&tx, &command.message_id)?.is_none());
    tx.commit()?;

    connection.execute_batch(crate::schema::COMMAND_TASK_CREATION_SCHEMA)?;
    let tx = connection.transaction()?;
    assert!(insert_pending_tx(&tx, &command)?);
    let columns: (String, String) = tx.query_row(
        "SELECT parent_turn_id, command_creation_id FROM task_result_settlements
         WHERE result_identity = ?1",
        [&command.result_identity],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(columns, (String::new(), command.task_id.clone()));
    assert_eq!(
        diagnostic_for_agent_message_on(&tx, "agent-a", &command.message_id)?,
        Some(command.clone())
    );
    assert!(diagnostic_for_agent_message_on(&tx, "other-agent", &command.message_id)?.is_none());
    tx.commit()?;
    Ok(())
}

#[test]
fn activation_readback_observes_updates_until_the_caller_rolls_back() -> Result<()> {
    let mut connection = settlement_database()?;
    let tx = connection.transaction()?;
    let first = pending("a")?;
    let second = pending("b")?;
    insert_pending_tx(&tx, &second)?;
    insert_pending_tx(&tx, &first)?;
    tx.commit()?;

    let admit = |mut record: TaskResultSettlementRecord| {
        record.state = TaskResultSettlementState::CallerAdmitted;
        record.admitted_at = Some(record.updated_at);
        record.activation_id = Some("activation-a".into());
        record
    };
    let first_admitted = admit(first.clone());
    let second_admitted = admit(second);
    let tx = connection.transaction()?;
    assert!(update_tx(&tx, &second_admitted)?);
    assert!(update_tx(&tx, &first_admitted)?);
    let expected = vec![first_admitted, second_admitted];
    assert_eq!(
        admitted_for_activation_tx(&tx, "agent-a", "activation-a")?,
        expected
    );
    assert_eq!(
        context_admitted_settlements_on(&tx, "agent-a", "activation-a")?,
        expected
    );
    assert!(context_admitted_settlements_on(&tx, "other-agent", "activation-a")?.is_empty());
    tx.rollback()?;

    assert!(context_admitted_settlements_on(&connection, "agent-a", "activation-a")?.is_empty());
    assert_eq!(
        latest_for_message_on(&connection, &first.message_id)?,
        Some(first)
    );
    Ok(())
}

#[test]
fn deferred_write_leaves_admission_and_recovery_columns_untouched() -> Result<()> {
    let mut connection = settlement_database()?;
    let tx = connection.transaction()?;
    let mut record = pending("deferred")?;
    insert_pending_tx(&tx, &record)?;
    tx.execute_batch(
        "CREATE TRIGGER reject_full_settlement_update
         BEFORE UPDATE OF state, activation_id, admitted_at, recovery_epoch, recovery_attempts
           ON task_result_settlements
         BEGIN
            SELECT RAISE(ABORT, 'defer must leave admission and recovery columns untouched');
         END;",
    )?;
    let retry_at = "2026-11-26T00:01:00Z".parse()?;
    record.next_recheck_at = Some(retry_at);
    record.deferred_at = Some(record.updated_at);
    record.deferred_reason = Some("pending recovery".into());
    assert!(defer_update_tx(&tx, &record)?);
    let stored: String = tx.query_row(
        "SELECT next_recheck_at FROM task_result_settlements WHERE result_identity = ?1",
        [&record.result_identity],
        |row| row.get(0),
    )?;
    assert_eq!(stored, timestamp(retry_at));
    assert_eq!(
        latest_for_message_on(&tx, &record.message_id)?,
        Some(record)
    );
    tx.commit()?;
    Ok(())
}
