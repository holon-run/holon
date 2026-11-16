use super::*;

#[test]
fn fresh_schema_keeps_live_tables_without_history_ack_storage() -> Result<()> {
    let f = Fixture::new()?;
    let retired_objects: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE name IN ('event_history_acks', 'event_history_ack_address')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retired_objects, 0);
    let live_tables: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN ('current_read_revisions', 'current_read_qualification')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(live_tables, 2);
    let live_indexes: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'current_read_known_address'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(live_indexes, 1);
    let applied: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version = 103",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(applied, 1);
    Ok(())
}

#[test]
fn reopen_and_offline_writes_ignore_dropped_or_residual_history_ack_storage() -> Result<()> {
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
    };

    for retain_legacy_table in [false, true] {
        let dir = tempfile::tempdir()?;
        let db_path = dir.path().join("runtime.sqlite");
        let lock_path = dir.path().join("runtime.lock");
        {
            let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            db.agent_identities().upsert(&AgentIdentityRecord::new(
                "history-schema-agent",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))?;
            db.connection()?.execute_batch(
                "CREATE TABLE event_history_acks (
                   agent_id TEXT NOT NULL,
                   identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
                   audit_event_id TEXT NOT NULL CHECK(length(CAST(audit_event_id AS BLOB)) > 0),
                   event_seq INTEGER NOT NULL CHECK(event_seq > 0),
                   content_digest TEXT NOT NULL CHECK(length(content_digest) = 64),
                   PRIMARY KEY(agent_id, identity_incarnation, audit_event_id));
                 CREATE INDEX event_history_ack_address ON event_history_acks(audit_event_id);
                 INSERT INTO event_history_acks VALUES
                   ('history-schema-agent', 1, 'retired-ack', 1, hex(zeroblob(32)));
                 CREATE TRIGGER retired_ack_insert BEFORE INSERT ON event_history_acks
                   BEGIN SELECT RAISE(ABORT, 'retired history ACK insert'); END;
                 CREATE TRIGGER retired_ack_update BEFORE UPDATE ON event_history_acks
                   BEGIN SELECT RAISE(ABORT, 'retired history ACK update'); END;
                 CREATE TRIGGER retired_ack_delete BEFORE DELETE ON event_history_acks
                   BEGIN SELECT RAISE(ABORT, 'retired history ACK delete'); END;",
            )?;
            if !retain_legacy_table {
                db.connection()?
                    .execute_batch("DROP TABLE event_history_acks;")?;
            }
        }

        let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let event = AuditEvent::legacy("offline-without-history-acks", serde_json::json!({}));
        db.append_offline_observation_event("history-schema-agent", &event)?;
        let count: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE audit_event_id = ?1",
            [&event.id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        if retain_legacy_table {
            let count: usize = db.connection()?.query_row(
                "SELECT COUNT(*) FROM event_history_acks",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1);
        } else {
            let count: usize = db.connection()?.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE name IN ('event_history_acks', 'event_history_ack_address')",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "reopen must not recreate retired ACK storage");
        }
    }
    Ok(())
}
