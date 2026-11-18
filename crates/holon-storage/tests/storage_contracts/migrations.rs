use anyhow::Result;
use chrono::Utc;
use holon_storage::migrations::{
    backfill_wait_condition_payload_columns, backfill_work_item_recheck_columns,
    current_schema_version, ensure_migration_table, max_known_migration_version, timestamp,
};
use rusqlite::{params, Connection};

#[test]
fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    connection.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        (
            max_known_migration_version() + 1,
            "future_test",
            Utc::now().to_rfc3339(),
        ),
    )?;

    assert_eq!(
        current_schema_version(&connection)?,
        max_known_migration_version() + 1
    );
    Ok(())
}

#[test]
fn backfill_wait_condition_payload_columns_adds_columns_and_fills_data() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE wait_conditions (
            wait_condition_id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            work_item_id TEXT,
            status TEXT NOT NULL,
            kind TEXT NOT NULL,
            source TEXT,
            subject_ref TEXT,
            waiting_for TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            expires_at TEXT,
            resolved_at TEXT,
            cancelled_at TEXT,
            last_turn_id TEXT,
            payload_json TEXT NOT NULL
        );",
    )?;

    let now = chrono::Utc::now();
    let payload = serde_json::json!({
        "id": "wc-1",
        "agent_id": "agent-a",
        "status": "active",
        "kind": "external",
        "source": "test",
        "subject_ref": "github:owner/repo#1",
        "waiting_for": "external",
        "wake_sources": [{"kind": "external_ingress", "external_trigger_id": "trigger-123"}],
        "continuation": {"action": "check_pr"},
        "created_at": now.to_rfc3339(),
        "updated_at": now.to_rfc3339()
    });
    let payload_json = serde_json::to_string(&payload)?;

    conn.execute(
        "INSERT INTO wait_conditions (wait_condition_id, agent_id, status, kind, waiting_for, created_at, updated_at, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params!["wc-1", "agent-a", "active", "external", "external", timestamp(now), timestamp(now), payload_json],
    )?;

    backfill_wait_condition_payload_columns(&conn)?;

    let wake_sources: String = conn.query_row(
        "SELECT wake_sources_json FROM wait_conditions WHERE wait_condition_id = 'wc-1'",
        [],
        |row| row.get(0),
    )?;
    assert!(wake_sources.contains("external_ingress"));

    let continuation: String = conn.query_row(
        "SELECT continuation_json FROM wait_conditions WHERE wait_condition_id = 'wc-1'",
        [],
        |row| row.get(0),
    )?;
    assert!(continuation.contains("check_pr"));

    Ok(())
}

#[test]
fn backfill_wait_condition_payload_columns_skips_existing_values() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE wait_conditions (
            wait_condition_id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            work_item_id TEXT,
            status TEXT NOT NULL,
            kind TEXT NOT NULL,
            source TEXT,
            subject_ref TEXT,
            waiting_for TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            expires_at TEXT,
            resolved_at TEXT,
            cancelled_at TEXT,
            last_turn_id TEXT,
            payload_json TEXT NOT NULL,
            wake_sources_json TEXT NOT NULL DEFAULT '[]',
            continuation_json TEXT
        );",
    )?;

    let now = chrono::Utc::now();
    conn.execute(
        "INSERT INTO wait_conditions (wait_condition_id, agent_id, status, kind, waiting_for, created_at, updated_at, payload_json, wake_sources_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params!["wc-2", "agent-a", "active", "external", "external", timestamp(now), timestamp(now), "{}", "[\"existing\"]"],
    )?;

    backfill_wait_condition_payload_columns(&conn)?;

    let wake_sources: String = conn.query_row(
        "SELECT wake_sources_json FROM wait_conditions WHERE wait_condition_id = 'wc-2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(wake_sources, "[\"existing\"]");

    Ok(())
}

#[test]
fn backfill_wait_condition_payload_columns_handles_missing_table() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    backfill_wait_condition_payload_columns(&conn)?;
    Ok(())
}

#[test]
fn backfill_work_item_recheck_columns_adds_columns_and_fills_data() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE work_items (
            work_item_id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            state TEXT NOT NULL,
            objective TEXT NOT NULL,
            plan_status TEXT,
            readiness TEXT,
            revision INTEGER NOT NULL,
            current_focus INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT,
            plan_artifact_path TEXT,
            last_turn_id TEXT,
            last_message_id TEXT,
            causation_id TEXT,
            correlation_id TEXT,
            payload_json TEXT NOT NULL
        );",
    )?;

    let now = chrono::Utc::now();
    let recheck_time = now + chrono::Duration::hours(1);
    let payload = serde_json::json!({
        "id": "wi-1",
        "agent_id": "agent-a",
        "workspace_id": "ws-test",
        "revision": 1,
        "objective": "Test work item",
        "state": "open",
        "plan_status": "draft",
        "blocked_by": "external:github:owner/repo#1",
        "recheck_at": recheck_time.to_rfc3339(),
        "created_at": now.to_rfc3339(),
        "updated_at": now.to_rfc3339()
    });
    let payload_json = serde_json::to_string(&payload)?;

    conn.execute(
        "INSERT INTO work_items (work_item_id, agent_id, state, objective, revision, current_focus, created_at, updated_at, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params!["wi-1", "agent-a", "open", "Test work item", 1, 0, timestamp(now), timestamp(now), payload_json],
    )?;

    backfill_work_item_recheck_columns(&conn)?;

    let blocked_by: String = conn.query_row(
        "SELECT blocked_by FROM work_items WHERE work_item_id = 'wi-1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(blocked_by, "external:github:owner/repo#1");

    let recheck_at: String = conn.query_row(
        "SELECT recheck_at FROM work_items WHERE work_item_id = 'wi-1'",
        [],
        |row| row.get(0),
    )?;
    assert!(!recheck_at.is_empty());

    Ok(())
}

#[test]
fn backfill_work_item_recheck_columns_skips_when_no_values() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE work_items (
            work_item_id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            state TEXT NOT NULL,
            objective TEXT NOT NULL,
            plan_status TEXT,
            readiness TEXT,
            revision INTEGER NOT NULL,
            current_focus INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT,
            plan_artifact_path TEXT,
            last_turn_id TEXT,
            last_message_id TEXT,
            causation_id TEXT,
            correlation_id TEXT,
            payload_json TEXT NOT NULL
        );",
    )?;

    let now = chrono::Utc::now();
    let payload = serde_json::json!({
        "id": "wi-2",
        "agent_id": "agent-a",
        "workspace_id": "ws-test",
        "revision": 1,
        "objective": "Test work item",
        "state": "open",
        "plan_status": "draft",
        "created_at": now.to_rfc3339(),
        "updated_at": now.to_rfc3339()
    });
    let payload_json = serde_json::to_string(&payload)?;

    conn.execute(
        "INSERT INTO work_items (work_item_id, agent_id, state, objective, revision, current_focus, created_at, updated_at, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params!["wi-2", "agent-a", "open", "Test work item", 1, 0, timestamp(now), timestamp(now), payload_json],
    )?;

    backfill_work_item_recheck_columns(&conn)?;

    let blocked_by: Option<String> = conn.query_row(
        "SELECT blocked_by FROM work_items WHERE work_item_id = 'wi-2'",
        [],
        |row| row.get(0),
    )?;
    assert!(blocked_by.is_none());

    Ok(())
}

#[test]
fn backfill_work_item_recheck_columns_handles_missing_table() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    backfill_work_item_recheck_columns(&conn)?;
    Ok(())
}
