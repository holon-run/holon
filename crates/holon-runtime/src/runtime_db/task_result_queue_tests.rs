use super::*;
use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
use rusqlite::{params, Connection};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn fixture() -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    ensure_migration_table(&db)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version < 110)
    {
        apply_migration(&mut db, migration)?;
    }
    Ok(db)
}

fn entry(
    db: &Connection,
    id: &str,
    agent: &str,
    status: &str,
    sequence: Option<i64>,
) -> Result<()> {
    db.execute("INSERT INTO messages(evidence_id,agent_id,message_id,message_seq,created_at,kind,payload_json) VALUES(?1,?2,?1,?3,'2026-11-14','operator_prompt','{}')", params![id,agent,sequence])?;
    let payload = serde_json::json!({"message_id":id,"agent_id":agent,"status":status}).to_string();
    db.execute("INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json) VALUES(?1,?2,'normal',?3,'2026-11-14','2026-11-14',?4)",params![id,agent,status,payload])?;
    Ok(())
}

fn observe(db: &mut Connection) -> Result<(Vec<String>, usize)> {
    let ticks = Arc::new(AtomicUsize::new(0));
    db.progress_handler(
        1,
        Some({
            let ticks = Arc::clone(&ticks);
            move || {
                ticks.fetch_add(1, Ordering::Relaxed);
                false
            }
        }),
    );
    let result: Result<Vec<String>> = (|| {
        let tx = db.transaction()?;
        let (count, bytes) = address_footprint(&tx, "agent-a")?;
        let ids = addresses(&tx, "agent-a")?;
        assert_eq!(count, ids.len());
        assert_eq!(bytes, ids.iter().map(|id| id.len() as u64).sum::<u64>());
        Ok(ids)
    })();
    db.progress_handler(0, None::<fn() -> bool>);
    Ok((result?, ticks.load(Ordering::Relaxed)))
}

#[test]
fn migration_bounds_recovery_observation_by_active_queue_without_statistics() -> Result<()> {
    let mut db = fixture()?;
    db.execute_batch("WITH RECURSIVE history(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM history WHERE n<20000)
        INSERT INTO messages(evidence_id,agent_id,message_id,message_seq,created_at,kind,payload_json)
        SELECT printf('old-%05d',n),'agent-a',printf('old-%05d',n),n,'2026-10-01','operator_prompt','{}' FROM history;
        INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json)
        SELECT message_id,agent_id,'normal','processed',created_at,created_at,'invalid historical JSON' FROM messages;")?;
    for (id, status, seq) in [
        ("latest", "queued", Some(20002)),
        ("previous", "interrupted", Some(20001)),
        ("legacy-z", "dequeued", None),
        ("legacy-a", "future_active", None),
        ("done", "processed", None),
        ("aborted", "aborted", None),
        ("dropped", "dropped", None),
        ("quarantined", "quarantined", None),
    ] {
        entry(&db, id, "agent-a", status, seq)?;
    }
    entry(&db, "foreign-agent", "agent-b", "queued", None)?;
    db.execute(
        "INSERT INTO queue_entries VALUES('orphan','agent-a','normal','queued','now','now','{}')",
        [],
    )?;
    let (before, old_steps) = observe(&mut db)?;
    assert_eq!(before, ["latest", "previous", "legacy-z", "legacy-a"]);
    let migration = MIGRATIONS
        .iter()
        .find(|row| row.name == "index_active_recovery_queue")
        .unwrap();
    apply_migration(&mut db, migration)?;
    let (after, new_steps) = observe(&mut db)?;
    assert_eq!(after, before);
    assert!(
        old_steps > 100_000,
        "fixture must exercise history: {old_steps}"
    );
    assert!(
        new_steps < 10_000,
        "recovery scanned history: {new_steps} VM steps"
    );
    // No ANALYZE is required to select the new sparse driving index.
    let plan = db
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT m.message_id FROM {ACTIVE_FROM} WHERE {ACTIVE_WHERE}"
        ))?
        .query_map(["agent-a"], |row| row.get::<_, String>(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_queue_entries_active_recovery")),
        "{plan:?}"
    );
    let indexes: usize = db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name='idx_queue_entries_active_recovery'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(indexes, 1);
    db.execute_batch(migration.sql)?;
    assert_eq!(observe(&mut db)?.0, after);
    db.execute_batch("WITH RECURSIVE history(n) AS (VALUES(20001) UNION ALL SELECT n+1 FROM history WHERE n<100000)
        INSERT INTO messages(evidence_id,agent_id,message_id,message_seq,created_at,kind,payload_json)
        SELECT printf('old-%05d',n),'agent-a',printf('old-%05d',n),n+2,'2026-10-01','operator_prompt','{}' FROM history;
        INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json)
        SELECT message_id,agent_id,'normal','processed',created_at,created_at,'invalid historical JSON' FROM messages
        WHERE message_seq BETWEEN 20003 AND 100002 AND message_id LIKE 'old-%';")?;
    let (expanded, expanded_steps) = observe(&mut db)?;
    assert_eq!(expanded, after);
    assert!(
        expanded_steps < 10_000,
        "recovery scanned expanded history: {expanded_steps} VM steps"
    );
    eprintln!("recovery queue VM steps: before={old_steps} after={new_steps} expanded_history={expanded_steps}");
    Ok(())
}

#[test]
fn recovery_observation_preserves_payload_and_foreign_scope_errors() -> Result<()> {
    let mut db = fixture()?;
    db.execute_batch(
        MIGRATIONS
            .iter()
            .find(|row| row.version == 110)
            .unwrap()
            .sql,
    )?;
    assert!(observe(&mut db)?.0.is_empty());
    entry(&db, "message", "agent-a", "queued", Some(1))?;
    let payload =
        serde_json::json!({"message_id":"message","agent_id":"agent-b","status":"queued"})
            .to_string();
    db.execute(
        "UPDATE queue_entries SET agent_id='agent-b',payload_json=?1 WHERE message_id='message'",
        [payload],
    )?;
    let error = observe(&mut db).unwrap_err();
    assert!(
        error.to_string().contains("belongs to another agent"),
        "{error:#}"
    );
    for payload in [
        "invalid JSON",
        r#"{"message_id":"different","agent_id":"agent-a","status":"queued"}"#,
        r#"{"message_id":"message","agent_id":"agent-a","status":"processed"}"#,
    ] {
        db.execute("UPDATE queue_entries SET agent_id='agent-a',payload_json=?1 WHERE message_id='message'",[payload])?;
        let error = observe(&mut db).unwrap_err();
        assert!(
            error.to_string().contains("normalized observation differs"),
            "{error:#}"
        );
    }
    db.execute("UPDATE queue_entries SET status='processed',payload_json='invalid historical JSON' WHERE message_id='message'",[])?;
    assert!(observe(&mut db)?.0.is_empty());
    Ok(())
}

#[test]
fn runtime_upgrade_and_reopen_keep_recovery_addresses() -> Result<()> {
    for old_version in [108, 109] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.sqlite");
        let lock = directory.path().join("runtime.lock");
        let mut connection = Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|row| row.version <= old_version) {
            apply_migration(&mut connection, migration)?;
        }
        entry(&connection, "active", "agent-a", "interrupted", Some(1))?;
        entry(&connection, "done", "agent-a", "processed", Some(2))?;
        drop(connection);
        for _ in 0..2 {
            let db = crate::runtime_db::RuntimeDb::open_and_migrate(&path, &lock)?;
            db.read_for_preparation(|tx| {
                assert_eq!(address_footprint(tx, "agent-a")?, (1, 6));
                assert_eq!(addresses(tx, "agent-a")?, ["active"]);
                let version: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(version, MIGRATIONS.last().unwrap().version);
                for (version, name) in [
                    (109, "index_startup_recovery_sources"),
                    (110, "index_active_recovery_queue"),
                ] {
                    let installed: String = tx.query_row(
                        "SELECT name FROM schema_migrations WHERE version=?1",
                        [version],
                        |row| row.get(0),
                    )?;
                    assert_eq!(installed, name);
                }
                let count: usize = tx.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name IN (
                        'idx_queue_entries_active_recovery',
                        'idx_turn_records_startup_terminal_source',
                        'idx_execution_protocol_interrupted_agents')",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 3);
                Ok(())
            })?;
        }
    }
    Ok(())
}
