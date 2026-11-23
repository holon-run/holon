use super::*;
use rusqlite::TransactionBehavior;

const SOURCES: &[&str] = &[
    "agent_identities",
    "agent_states",
    "queue_entries",
    "audit_events",
    "audit_event_retention_watermarks",
    "runtime_sequences",
    "briefs",
    "work_items",
    "messages",
    "transcript_entries",
];

#[test]
fn ordered_borrowed_members_ignore_hidden_corruption_and_retry_in_one_view() -> Result<()> {
    let mut connection = migrated_connection()?;
    // Deliberately insert the members in the opposite order to verification.
    for id in ["z-public", "a-public", "hidden", "deleted"] {
        let tx = connection.transaction()?;
        write_identity(
            &tx,
            &AgentIdentityRecord::new(
                id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        tx.commit()?;
    }
    connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE agent_identities SET visibility = 'private', payload_json = 'null'
           WHERE agent_id = 'hidden';
         UPDATE agent_identities SET status = 'deleted', payload_json = 'null'
           WHERE agent_id = 'deleted';
         PRAGMA ignore_check_constraints = OFF;",
    )?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    assert!(SYNC.verify_projection_snapshot_view(&transaction)?);
    for id in ["z-public", "a-public"] {
        // Valid JSON with an invalid typed State: the selected member must
        // still undergo the original per-projection assembly validation.
        transaction.execute(
            "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
             VALUES (?1, 'stopped', '2026-01-01T00:00:00Z', 'null')",
            [id],
        )?;
    }
    let error = SYNC
        .verify_projection_snapshot_view(&transaction)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("unreadable projection agent state payload for a-public"),
        "{error:#}"
    );
    transaction.execute("DELETE FROM agent_states WHERE agent_id = 'a-public'", [])?;
    let error = SYNC
        .verify_projection_snapshot_view(&transaction)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("unreadable projection agent state payload for z-public"),
        "{error:#}"
    );
    transaction.execute("DELETE FROM agent_states WHERE agent_id = 'z-public'", [])?;
    assert!(SYNC.verify_projection_snapshot_view(&transaction)?);
    transaction.rollback()?;
    assert!(connection.is_autocommit());
    assert!(SYNC.verify_projection_snapshot_view(&connection)?);
    Ok(())
}

#[test]
fn empty_members_still_require_metadata_and_every_projection_source() -> Result<()> {
    let mut connection = migrated_connection()?;
    assert!(SYNC.verify_projection_snapshot_view(&connection)?);
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM runtime_metadata WHERE key = ?1", [key])?;
        let error = SYNC
            .verify_projection_snapshot_view(&transaction)
            .unwrap_err();
        assert!(format!("{error:#}").contains(key), "{error:#}");
        transaction.rollback()?;
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE runtime_metadata SET value = 'not-a-generation'
         WHERE key = 'visibility_policy_generation'",
        [],
    )?;
    let error = SYNC
        .verify_projection_snapshot_view(&transaction)
        .unwrap_err();
    assert!(format!("{error:#}").contains("invalid visibility policy generation"));
    transaction.rollback()?;
    assert!(SYNC.verify_projection_snapshot_view(&connection)?);

    // The source-presence gate must run before attempting metadata or rows.
    let connection = Connection::open_in_memory()?;
    for table in SOURCES {
        connection.execute_batch(&format!("CREATE TABLE {table} (unused INTEGER)"))?;
    }
    for table in SOURCES {
        connection.execute_batch(&format!("DROP TABLE {table}"))?;
        assert!(
            !SYNC.verify_projection_snapshot_view(&connection)?,
            "{table}"
        );
        connection.execute_batch(&format!("CREATE TABLE {table} (unused INTEGER)"))?;
    }
    Ok(())
}

#[test]
fn empty_members_validate_original_roster_columns_and_retry_after_rollback() -> Result<()> {
    let mut connection = migrated_connection()?;
    for (table, columns) in [
        (
            "agent_identities",
            &["agent_id", "payload_json", "status", "visibility"][..],
        ),
        (
            "runtime_sequences",
            &["domain", "scope_key", "last_value"][..],
        ),
        (
            "audit_event_retention_watermarks",
            &["scope_key", "oldest_retained_seq"][..],
        ),
        (
            "briefs",
            &[
                "agent_id",
                "evidence_id",
                "created_event_seq",
                "created_at",
                "preview",
            ][..],
        ),
        ("agent_states", &["agent_id", "payload_json"][..]),
        ("queue_entries", &["agent_id", "status"][..]),
    ] {
        for column in columns {
            let transaction = connection.transaction()?;
            transaction.execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN {column} TO saved_{column}"
            ))?;
            let before: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM observer_sync_capability_verifications",
                [],
                |row| row.get(0),
            )?;
            let error = SYNC
                .verify_projection_snapshot_view(&transaction)
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("no such column"),
                "{table}.{column}: {error:#}"
            );
            let after: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM observer_sync_capability_verifications",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(before, after);
            transaction.rollback()?;
            assert!(connection.is_autocommit());
            assert!(SYNC.verify_projection_snapshot_view(&connection)?);
        }
    }
    Ok(())
}
