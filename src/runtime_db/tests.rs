// Common imports used by both `test_support` and `tests` submodules via `use super::*`.
#[cfg(test)]
use crate::runtime_db::connection::{is_retryable_db_error, open_connection};
#[cfg(test)]
use crate::runtime_db::evidence::insert_audit_event_tx;
#[cfg(test)]
use crate::runtime_db::migrations::{
    backfill_wait_condition_payload_columns, backfill_work_item_recheck_columns,
    current_schema_version, ensure_migration_table, max_known_migration_version, table_exists,
    MIGRATIONS,
};
#[cfg(test)]
use crate::runtime_db::storage_domain::upsert_storage_domain;
#[cfg(test)]
use crate::runtime_db::{
    RuntimeDb, RuntimeDbRetryableError, TASK_PAYLOAD_ARRAY_LIMIT, TASK_PAYLOAD_STRING_LIMIT,
};
#[cfg(test)]
use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, CallbackDeliveryMode,
    ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus, MessageEnvelope,
    QueueEntryRecord, QueueEntryStatus, TaskRecord, TaskStatus, ToolExecutionRecord,
    WorkItemRecord, WorkItemState, WorkspaceEntry, WorkspaceOccupancyRecord,
};
#[cfg(test)]
use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use rusqlite::params;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::runtime_db::migrations::timestamp;
#[cfg(test)]
use crate::runtime_db::RuntimeDbLock;
#[cfg(test)]
use rusqlite::{ffi::ErrorCode, TransactionBehavior};

#[cfg(test)]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct TempRuntimeDb {
        pub db: RuntimeDb,
        _temp_dir: TempDir,
    }

    impl TempRuntimeDb {
        pub fn new() -> Result<Self> {
            let temp_dir = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                temp_dir.path().join("state/runtime.sqlite"),
                temp_dir.path().join("state/runtime.lock"),
            )?;
            Ok(Self {
                db,
                _temp_dir: temp_dir,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        system::WorkspaceAccessMode,
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
            AgentVisibility, BriefKind,
        },
    };
    use std::process::Command;
    use tempfile::tempdir;

    fn temp_paths() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("state/runtime.sqlite");
        let lock_path = temp_dir.path().join("state/runtime.lock");
        Ok((temp_dir, db_path, lock_path))
    }

    fn wait_until(mut condition: impl FnMut() -> Result<bool>, label: &str) -> Result<()> {
        let started_at = Instant::now();
        loop {
            if condition()? {
                return Ok(());
            }
            if started_at.elapsed() > Duration::from_secs(2) {
                bail!("{label} did not become true");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn runtime_db_retryable_error_classification_survives_context() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        let error: anyhow::Error = RuntimeDbRetryableError::new(
            "starting immediate transaction",
            &db_path,
            "database is locked",
        )
        .into();
        let error = error.context("processing message");
        assert!(is_retryable_db_error(&error));
        assert!(!is_retryable_db_error(&anyhow!("not a db lock")));
        Ok(())
    }

    #[test]
    fn runtime_db_raw_sqlite_lock_errors_are_retryable() {
        let locked: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseLocked,
                extended_code: 0,
            },
            Some("database is locked".to_string()),
        )
        .into();
        let busy: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy,
                extended_code: 0,
            },
            Some("database is busy".to_string()),
        )
        .into();
        let constraint: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::ConstraintViolation,
                extended_code: 0,
            },
            None,
        )
        .into();

        assert!(is_retryable_db_error(
            &locked.context("inserting audit event")
        ));
        assert!(is_retryable_db_error(
            &busy.context("updating transcript entry")
        ));
        assert!(!is_retryable_db_error(&constraint));
    }

    #[test]
    fn runtime_db_sync_transaction_retries_retryable_body_error() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut attempts = 0usize;

        db.transaction(|tx| {
            attempts += 1;
            if attempts == 1 {
                return Err(RuntimeDbRetryableError::new(
                    "inserting audit event",
                    &db_path,
                    "database is locked",
                )
                .into());
            }
            insert_audit_event_tx(
                tx,
                Some("agent-a"),
                &AuditEvent::new("runtime_db_retry_body", serde_json::json!({})),
            )
        })?;

        assert_eq!(attempts, 2);
        let events = db.audit_events().recent(Some("agent-a"), 10)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "runtime_db_retry_body");
        Ok(())
    }

    fn task_record(id: &str, agent_id: &str, status: TaskStatus, offset: i64) -> TaskRecord {
        let created_at = Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: crate::types::TaskKind::CommandTask,
            status,
            created_at,
            updated_at: created_at + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: Some(serde_json::json!({
                "cmd": "printf test",
                "output_path": format!("/tmp/{id}.log"),
                "output_summary": format!("{id} summary"),
                "exit_status": 0,
                "accepts_input": true,
                "input_target": "stdin"
            })),
            recovery: None,
        }
    }

    fn external_trigger_record(
        id: &str,
        agent_id: &str,
        status: ExternalTriggerStatus,
        offset: i64,
    ) -> ExternalTriggerRecord {
        let created_at = Utc::now() + chrono::Duration::seconds(offset);
        ExternalTriggerRecord {
            external_trigger_id: id.into(),
            target_agent_id: agent_id.into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            trigger_url: Some(format!("https://example.test/{id}")),
            token_hash: format!("hash-{id}"),
            status,
            created_at,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        }
    }

    fn workspace_entry(id: &str, updated_offset: i64) -> WorkspaceEntry {
        let created_at = Utc::now();
        let mut entry = WorkspaceEntry::new(
            id,
            PathBuf::from(format!("/tmp/{id}")),
            Some(format!("repo-{id}")),
        );
        entry.workspace_alias = Some(format!("alias-{id}"));
        entry.workspace_kind = Some("project".into());
        entry.owner_agent_id = Some("agent-a".into());
        entry.created_at = created_at;
        entry.updated_at = created_at + chrono::Duration::seconds(updated_offset);
        entry
    }

    fn workspace_occupancy(id: &str, released_offset: Option<i64>) -> WorkspaceOccupancyRecord {
        let acquired_at = Utc::now();
        WorkspaceOccupancyRecord {
            occupancy_id: id.into(),
            execution_root_id: format!("exec-{id}"),
            workspace_id: format!("ws-{id}"),
            holder_agent_id: "agent-a".into(),
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            acquired_at,
            released_at: released_offset
                .map(|offset| acquired_at + chrono::Duration::seconds(offset)),
        }
    }

    fn agent_identity(agent_id: &str, updated_offset: i64) -> AgentIdentityRecord {
        let mut identity = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.created_at = Utc::now();
        identity.updated_at = identity.created_at + chrono::Duration::seconds(updated_offset);
        identity.status = AgentRegistryStatus::Active;
        identity
    }

    #[test]
    fn runtime_db_fresh_migration_creates_foundation_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;

        let version = db.current_schema_version()?;
        assert_eq!(version, max_known_migration_version());
        for table in [
            "schema_migrations",
            "storage_domains",
            "agents",
            "audit_events",
            "work_items",
            "tasks",
            "external_triggers",
            "messages",
            "transcript_entries",
            "tool_executions",
            "model_requests",
            "model_responses",
            "briefs",
            "delivery_summaries",
            "artifact_metadata",
            "wait_conditions",
            "queue_entries",
            "timers",
            "turn_records",
            "agent_states",
            "workspace_entries",
            "workspace_occupancies",
            "agent_identities",
            "context_episode_anchors",
        ] {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "missing table {table}");
        }

        Ok(())
    }

    #[test]
    fn runtime_db_context_episode_anchors_schema_replaces_legacy_episode_table() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        assert!(!table_exists(&connection, "context_episodes")?);
        let mut statement = connection.prepare("PRAGMA table_info(context_episode_anchors)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        assert!(!columns.iter().any(|column| column == "summary"));
        assert!(columns.iter().any(|column| column == "payload_json"));
        Ok(())
    }

    #[test]
    fn runtime_db_migration_drops_unreleased_context_episodes_table() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
CREATE TABLE storage_domains (
  domain TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL,
  import_status TEXT NOT NULL,
  canonical_source TEXT NOT NULL,
  source_checkpoint_json TEXT,
  imported_at TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE context_episodes (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  summary TEXT NOT NULL,
  payload_json TEXT NOT NULL
);
CREATE INDEX idx_context_episodes_agent_turn
  ON context_episodes(agent_id, end_turn_index);
CREATE INDEX idx_context_episodes_work_item
  ON context_episodes(work_item_id);
INSERT INTO context_episodes (
  episode_id, agent_id, workspace_id, boundary_reason,
  start_turn_index, end_turn_index, started_at, ended_at, summary, payload_json
) VALUES (
  'episode-old', 'default', 'agent_home', 'hard_turn_cap',
  1, 2, '2026-06-10T00:00:00Z', '2026-06-10T00:01:00Z',
  'legacy summary', '{}'
);
INSERT INTO storage_domains (
  domain, schema_version, import_status, canonical_source, updated_at
) VALUES (
  'context_episodes', 1, 'complete', 'db', '2026-06-10T00:01:00Z'
);
"#,
            )?;
            for migration in &MIGRATIONS[..12] {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (
                        migration.version,
                        migration.name,
                        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                )?;
            }
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;

        assert!(!table_exists(&connection, "context_episodes")?);
        assert!(table_exists(&connection, "context_episode_anchors")?);
        let old_domain_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM storage_domains WHERE domain = 'context_episodes'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(old_domain_count, 0);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_is_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, max_known_migration_version());
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn runtime_db_migration_compacts_queue_entries_to_current_view() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
CREATE TABLE queue_entries (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);
INSERT INTO queue_entries (
  message_id, agent_id, priority, status, created_at, updated_at, payload_json
) VALUES
  (
    'message-1', 'agent-a', 'normal', 'queued',
    '2026-06-17T00:00:00.000Z', '2026-06-17T00:00:00.000Z',
    '{"message_id":"message-1","agent_id":"agent-a","priority":"normal","status":"queued","created_at":"2026-06-17T00:00:00.000Z","updated_at":"2026-06-17T00:00:00.000Z"}'
  ),
  (
    'message-1', 'agent-a', 'normal', 'processed',
    '2026-06-17T00:00:00.000Z', '2026-06-17T00:01:00.000Z',
    '{"message_id":"message-1","agent_id":"agent-a","priority":"normal","status":"processed","created_at":"2026-06-17T00:00:00.000Z","updated_at":"2026-06-17T00:01:00.000Z"}'
  ),
  (
    'message-2', 'agent-a', 'interject', 'queued',
    '2026-06-17T00:02:00.000Z', '2026-06-17T00:02:00.000Z',
    '{"message_id":"message-2","agent_id":"agent-a","priority":"interject","status":"queued","created_at":"2026-06-17T00:02:00.000Z","updated_at":"2026-06-17T00:02:00.000Z"}'
  );
"#,
            )?;
            for migration in &MIGRATIONS[..17] {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (
                        migration.version,
                        migration.name,
                        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                )?;
            }
        }

        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let entries = db.queue_entries().latest_all()?;
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| {
            entry.message_id == "message-1" && entry.status == QueueEntryStatus::Processed
        }));
        assert!(entries.iter().any(|entry| {
            entry.message_id == "message-2" && entry.status == QueueEntryStatus::Queued
        }));

        let connection = open_connection(&db_path)?;
        let duplicate = connection.execute(
            "INSERT INTO queue_entries (
                message_id, agent_id, priority, status, created_at, updated_at, payload_json
             ) VALUES (
                'message-2', 'agent-a', 'interject', 'dequeued',
                '2026-06-17T00:02:00.000Z', '2026-06-17T00:03:00.000Z', '{}'
             )",
            [],
        );
        assert!(duplicate.is_err());

        Ok(())
    }

    #[test]
    fn runtime_db_read_connection_opens_while_external_writer_holds_lock() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut external = open_connection(&db_path)?;
        let _external_write = external.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let read = db.connection()?;
        let value: i64 = read.query_row("SELECT 1", [], |row| row.get(0))?;
        assert_eq!(value, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_async_append_retries_temporarily_locked_writer() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut external = open_connection(&db_path)?;
        let external_write = external.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let writer = db.clone();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || -> Result<()> {
            attempt_tx
                .send(())
                .map_err(|_| anyhow!("failed to signal writer attempt"))?;
            writer.append(|tx| {
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::new(
                        "runtime_db_locked_retry",
                        serde_json::json!({ "source": "test" }),
                    ),
                )
            })
        });

        attempt_rx
            .recv_timeout(Duration::from_secs(1))
            .context("writer thread did not start")?;
        std::thread::sleep(Duration::from_millis(100));
        drop(external_write);

        handle
            .join()
            .map_err(|_| anyhow!("writer thread panicked"))??;
        wait_until(
            || {
                let events = db.audit_events().recent(Some("agent-a"), 1)?;
                Ok(events.len() == 1 && events[0].kind == "runtime_db_locked_retry")
            },
            "locked async append retry",
        )?;
        Ok(())
    }

    #[test]
    fn runtime_db_clones_serialize_concurrent_writes_through_shared_writer() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut handles = Vec::new();

        for index in 0..8 {
            let writer = db.clone();
            handles.push(std::thread::spawn(move || -> Result<()> {
                writer.audit_events().append(
                    Some("agent-a"),
                    &AuditEvent::new(
                        format!("runtime_db_concurrent_write_{index}"),
                        serde_json::json!({ "index": index }),
                    ),
                )
            }));
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow!("writer thread panicked"))??;
        }

        wait_until(
            || {
                let connection = db.connection()?;
                let count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE agent_id = 'agent-a'",
                    [],
                    |row| row.get(0),
                )?;
                Ok(count == 8)
            },
            "concurrent queued writes",
        )?;
        Ok(())
    }

    #[test]
    fn runtime_db_transactions_are_queued_across_db_instances() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_writer = first.clone();
        let first_handle = std::thread::spawn(move || -> Result<()> {
            first_writer.transaction(|tx| {
                entered_tx
                    .send(())
                    .map_err(|_| anyhow!("failed to signal first write"))?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .context("release signal not received")?;
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::new("runtime_db_queue_first", serde_json::json!({})),
                )
            })
        });

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .context("first write did not enter transaction")?;

        let second_writer = second.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let second_handle = std::thread::spawn(move || -> Result<()> {
            second_writer.transaction(|tx| {
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::new("runtime_db_queue_second", serde_json::json!({})),
                )
            })?;
            done_tx
                .send(())
                .map_err(|_| anyhow!("failed to signal second write"))?;
            Ok(())
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second write committed before the first queued write completed"
        );
        release_tx
            .send(())
            .map_err(|_| anyhow!("failed to release first write"))?;

        first_handle
            .join()
            .map_err(|_| anyhow!("first writer thread panicked"))??;
        second_handle
            .join()
            .map_err(|_| anyhow!("second writer thread panicked"))??;

        let events = second.audit_events().recent(Some("agent-a"), 2)?;
        let kinds = events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["runtime_db_queue_first", "runtime_db_queue_second"]
        );
        Ok(())
    }

    #[test]
    fn runtime_db_append_accepts_without_waiting_for_commit() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = db.clone();
        let handle = std::thread::spawn(move || -> Result<()> {
            blocker.transaction(|tx| {
                entered_tx
                    .send(())
                    .map_err(|_| anyhow!("failed to signal blocking write"))?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .context("release signal not received")?;
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::new("runtime_db_append_blocker", serde_json::json!({})),
                )
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .context("blocking write did not enter transaction")?;

        db.append(|tx| {
            insert_audit_event_tx(
                tx,
                Some("agent-a"),
                &AuditEvent::new("runtime_db_append_async", serde_json::json!({})),
            )
        })?;
        assert_eq!(db.audit_events().recent(Some("agent-a"), 10)?.len(), 0);

        release_tx
            .send(())
            .map_err(|_| anyhow!("failed to release blocking write"))?;
        handle
            .join()
            .map_err(|_| anyhow!("blocking writer thread panicked"))??;

        let started_at = Instant::now();
        loop {
            let events = db.audit_events().recent(Some("agent-a"), 10)?;
            if events.len() == 2 {
                let kinds = events
                    .iter()
                    .map(|event| event.kind.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    kinds,
                    vec!["runtime_db_append_blocker", "runtime_db_append_async"]
                );
                return Ok(());
            }
            if started_at.elapsed() > Duration::from_secs(2) {
                bail!("queued append did not commit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn runtime_db_migration_drops_unreleased_working_memory_deltas() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE working_memory_deltas (
  memory_delta_id TEXT PRIMARY KEY,
  from_revision INTEGER NOT NULL,
  to_revision INTEGER NOT NULL,
  created_at_turn INTEGER NOT NULL,
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);
"#,
            )?;
            connection.execute(
                "INSERT INTO working_memory_deltas (
                    memory_delta_id, from_revision, to_revision, created_at_turn,
                    reason, created_at, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    "memory-delta-1-2-7",
                    1_i64,
                    2_i64,
                    7_i64,
                    "task_rejoined",
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    "{}",
                ),
            )?;
            connection.execute(
                "INSERT OR REPLACE INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "working_memory_deltas",
                    max_known_migration_version(),
                    "complete",
                    "db",
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                ),
            )?;
            connection.execute("DELETE FROM schema_migrations WHERE version = 14", [])?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        assert!(!table_exists(&connection, "working_memory_deltas")?);
        assert!(!table_exists(
            &connection,
            "working_memory_deltas_unscoped_legacy"
        )?);
        let domain_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM storage_domains WHERE domain = ?1",
            ["working_memory_deltas"],
            |row| row.get(0),
        )?;
        assert_eq!(domain_count, 0);
        Ok(())
    }

    #[test]
    fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
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
    fn runtime_db_migration_name_mismatch_fails() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (1_i64, "wrong_name", Utc::now().to_rfc3339()),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error.to_string().contains("name mismatch"));
        Ok(())
    }

    #[test]
    fn runtime_db_accepts_released_message_search_migration_name() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 14)
            {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (migration.version, migration.name, Utc::now().to_rfc3339()),
                )?;
            }
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (15_i64, "message_search_index", Utc::now().to_rfc3339()),
            )?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        assert!(!table_exists(&connection, "message_search_index")?);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_rejects_newer_schema_version() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (
                    max_known_migration_version() + 1,
                    "future_test",
                    Utc::now().to_rfc3339(),
                ),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error
            .to_string()
            .contains("newer than this binary supports"));
        Ok(())
    }

    #[test]
    fn runtime_db_recent_payloads_keep_evidence_id_ascending_after_reverse() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let created_at = Utc::now();
        let mut later_id = BriefRecord::new("agent-a", BriefKind::Result, "later id", None, None);
        later_id.id = "brief-b".into();
        later_id.created_at = created_at;
        let mut earlier_id =
            BriefRecord::new("agent-a", BriefKind::Result, "earlier id", None, None);
        earlier_id.id = "brief-a".into();
        earlier_id.created_at = created_at;

        db.evidence().append_brief(&later_id)?;
        db.evidence().append_brief(&earlier_id)?;

        wait_until(
            || Ok(db.evidence().recent_briefs("agent-a", 2)?.len() == 2),
            "recent brief writes",
        )?;
        let records = db.evidence().recent_briefs("agent-a", 2)?;
        assert_eq!(
            records
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            vec!["brief-a", "brief-b"]
        );
        Ok(())
    }

    #[test]
    fn queue_claim_allows_only_one_consumer_for_queued_message() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let record = QueueEntryRecord {
            message_id: "message-1".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&record)?;

        let mut claim = record.clone();
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(1);
        assert!(db.queue_entries().try_claim_queued_message(&claim)?);

        let mut duplicate_claim = claim.clone();
        duplicate_claim.updated_at = now + chrono::Duration::seconds(2);
        assert!(!db
            .queue_entries()
            .try_claim_queued_message(&duplicate_claim)?);

        let latest = db.queue_entries().latest_all()?;
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].message_id, "message-1");
        assert_eq!(latest[0].status, QueueEntryStatus::Dequeued);
        Ok(())
    }

    #[test]
    fn queue_entries_table_uses_message_id_as_current_state_key() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let primary_key_columns: Vec<String> = {
            let mut statement = connection.prepare("PRAGMA table_info(queue_entries)")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|(name, pk)| (pk > 0).then_some(name))
                .collect()
        };
        assert_eq!(primary_key_columns, vec!["message_id"]);

        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-current".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        db.queue_entries().upsert(&queued)?;
        db.queue_entries().upsert(&processed)?;

        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM queue_entries", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        assert!(db.queue_entries().queued_for_agent("agent-a")?.is_empty());
        Ok(())
    }

    #[test]
    fn message_repository_orders_null_message_seq_as_legacy_history() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let base = Utc::now();

        let mut sequenced_1 = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator { actor_id: None },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: "one".into() },
        );
        sequenced_1.id = "msg-seq-1".into();
        sequenced_1.message_seq = Some(1);
        sequenced_1.created_at = base;

        let mut sequenced_2 = sequenced_1.clone();
        sequenced_2.id = "msg-seq-2".into();
        sequenced_2.message_seq = Some(2);
        sequenced_2.created_at = base + chrono::Duration::seconds(1);

        let mut legacy_without_sequence = sequenced_1.clone();
        legacy_without_sequence.id = "msg-legacy".into();
        legacy_without_sequence.message_seq = None;
        legacy_without_sequence.created_at = base + chrono::Duration::seconds(2);

        db.messages().upsert_many(&[
            sequenced_1.clone(),
            sequenced_2.clone(),
            legacy_without_sequence.clone(),
        ])?;

        let all_ids = db
            .messages()
            .all(Some("agent-a"))?
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        assert_eq!(all_ids, vec!["msg-legacy", "msg-seq-1", "msg-seq-2"]);

        let recent_ids = db
            .messages()
            .recent(Some("agent-a"), 2)?
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        assert_eq!(recent_ids, vec!["msg-seq-1", "msg-seq-2"]);
        Ok(())
    }

    #[test]
    fn queue_claim_rejects_message_whose_current_status_is_terminal() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-1".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        db.queue_entries().upsert(&queued)?;
        db.queue_entries().upsert(&processed)?;

        let mut claim = queued;
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(2);
        assert!(!db.queue_entries().try_claim_queued_message(&claim)?);

        Ok(())
    }

    #[test]
    fn queued_for_agent_reads_current_queue_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let stale_queued = QueueEntryRecord {
            message_id: "stale-message".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut stale_processed = stale_queued.clone();
        stale_processed.status = QueueEntryStatus::Processed;
        stale_processed.updated_at = now + chrono::Duration::seconds(1);

        let fresh_queued = QueueEntryRecord {
            message_id: "fresh-message".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Interject,
            status: QueueEntryStatus::Queued,
            created_at: now + chrono::Duration::seconds(2),
            updated_at: now + chrono::Duration::seconds(2),
        };

        db.queue_entries().upsert(&stale_queued)?;
        db.queue_entries().upsert(&stale_processed)?;
        db.queue_entries().upsert(&fresh_queued)?;

        assert!(db.queue_entries().has_queued_for_agent("agent-a")?);
        let queued = db.queue_entries().queued_for_agent("agent-a")?;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message_id, "fresh-message");

        let mut fresh_dequeued = fresh_queued.clone();
        fresh_dequeued.status = QueueEntryStatus::Dequeued;
        fresh_dequeued.updated_at = now + chrono::Duration::seconds(3);
        db.queue_entries().upsert(&fresh_dequeued)?;

        assert!(!db.queue_entries().has_queued_for_agent("agent-a")?);
        assert!(db.queue_entries().queued_for_agent("agent-a")?.is_empty());

        Ok(())
    }

    #[test]
    fn queued_for_agent_includes_interrupted_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();

        let queued_entry = QueueEntryRecord {
            message_id: "msg-queued".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let interrupted_entry = QueueEntryRecord {
            message_id: "msg-interrupted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Interrupted,
            created_at: now + chrono::Duration::seconds(1),
            updated_at: now + chrono::Duration::seconds(1),
        };
        let aborted_entry = QueueEntryRecord {
            message_id: "msg-aborted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Aborted,
            created_at: now + chrono::Duration::seconds(2),
            updated_at: now + chrono::Duration::seconds(2),
        };

        db.queue_entries().upsert(&queued_entry)?;
        db.queue_entries().upsert(&interrupted_entry)?;
        db.queue_entries().upsert(&aborted_entry)?;

        let entries = db.queue_entries().queued_for_agent("agent-a")?;
        let message_ids: Vec<_> = entries.iter().map(|e| e.message_id.as_str()).collect();
        assert!(
            message_ids.contains(&"msg-queued"),
            "Queued entry should be included"
        );
        assert!(
            message_ids.contains(&"msg-interrupted"),
            "Interrupted entry should be included for recovery replay"
        );
        assert!(
            !message_ids.contains(&"msg-aborted"),
            "Aborted entry should NOT be included"
        );

        Ok(())
    }

    #[test]
    fn try_claim_succeeds_for_interrupted_entry() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();

        let record = QueueEntryRecord {
            message_id: "msg-interrupted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Interrupted,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&record)?;

        // An Interrupted entry must be claimable, otherwise recovery would
        // silently drop it. See PR #2052 review feedback.
        assert!(db.queue_entries().has_queued_for_agent("agent-a")?);
        let mut claim = record.clone();
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(1);
        assert!(
            db.queue_entries().try_claim_queued_message(&claim)?,
            "Interrupted entry should be claimable for replay"
        );
        assert_eq!(
            db.queue_entries().latest_all()?[0].status,
            QueueEntryStatus::Dequeued
        );
        Ok(())
    }

    #[test]
    fn runtime_db_foreign_keys_are_enabled_per_connection() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(enabled, 1);
        Ok(())
    }

    #[test]
    fn agent_state_repository_upserts_latest_turn_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut current = AgentState::new("agent-a");
        current.status = AgentStatus::AwakeIdle;
        current.turn_index = 3;
        current.current_work_item_id = Some("work-current".into());
        db.agent_states().import_legacy(Some(current.clone()))?;

        let mut stale = current.clone();
        stale.status = AgentStatus::Stopped;
        stale.turn_index = 2;
        stale.current_work_item_id = Some("work-stale".into());
        db.agent_states().upsert(&stale)?;

        let persisted = db.agent_states().latest("agent-a")?.expect("agent state");
        assert_eq!(persisted.status, AgentStatus::AwakeIdle);
        assert_eq!(persisted.turn_index, 3);
        assert_eq!(
            persisted.current_work_item_id.as_deref(),
            Some("work-current")
        );
        Ok(())
    }

    #[test]
    fn workspace_entry_import_is_idempotent_and_keeps_latest_update() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older = workspace_entry("ws-a", 1);
        let mut newer = workspace_entry("ws-a", 5);
        newer.workspace_alias = Some("alias-newer".into());

        db.workspace_entries()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.workspace_entries().import_legacy(vec![older, newer])?;

        let entries = db.workspace_entries().latest_all()?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].workspace_id, "ws-a");
        assert_eq!(entries[0].workspace_alias.as_deref(), Some("alias-newer"));
        let rows: i64 =
            db.connection()?
                .query_row("SELECT COUNT(*) FROM workspace_entries", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn workspace_occupancy_import_is_idempotent_and_keeps_released_record() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let active = workspace_occupancy("occ-a", None);
        let released = workspace_occupancy("occ-a", Some(10));

        db.workspace_occupancies()
            .import_legacy(vec![active.clone(), released.clone()])?;
        db.workspace_occupancies()
            .import_legacy(vec![active, released])?;

        let occupancies = db.workspace_occupancies().latest_all()?;
        assert_eq!(occupancies.len(), 1);
        assert_eq!(occupancies[0].occupancy_id, "occ-a");
        assert!(occupancies[0].released_at.is_some());
        let rows: i64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM workspace_occupancies",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn agent_identity_repository_imports_latest_and_reads_by_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older = agent_identity("agent-a", 1);
        let mut newer = agent_identity("agent-a", 5);
        newer.status = AgentRegistryStatus::Archived;
        newer.archived_at = Some(newer.updated_at);

        db.agent_identities()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.agent_identities().import_legacy(vec![older, newer])?;

        let identity = db
            .agent_identities()
            .latest("agent-a")?
            .expect("agent identity");
        assert_eq!(identity.status, AgentRegistryStatus::Archived);
        assert!(identity.archived_at.is_some());
        let identities = db.agent_identities().latest_all()?;
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].agent_id, "agent-a");
        Ok(())
    }

    #[test]
    fn runtime_db_transaction_helper_commits_and_rolls_back() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                ("test", 1_i64, "pending", "jsonl", Utc::now().to_rfc3339()),
            )?;
            Ok(())
        })?;
        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);

        let error = db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO storage_domains (
                        domain, schema_version, import_status, canonical_source, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        "rollback",
                        1_i64,
                        "pending",
                        "jsonl",
                        Utc::now().to_rfc3339(),
                    ),
                )?;
                Err::<(), anyhow::Error>(anyhow!("force rollback"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "force rollback");

        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn storage_domain_import_failure_is_visible_and_retryable() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let error = db
            .evidence()
            .import_legacy(
                vec![serde_json::json!({ "turn_index": 1 })],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain evidence"));
        let failed = db
            .storage_domain("evidence")?
            .expect("failed storage domain row");
        assert_eq!(failed.import_status, "failed");
        assert_eq!(failed.canonical_source, "jsonl");
        assert!(failed
            .source_checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("restart runtime to retry")));

        db.run_storage_domain_import("evidence", "jsonl", "db", |tx| {
            let checkpoint: Option<String> = tx.query_row(
                "SELECT source_checkpoint_json FROM storage_domains WHERE domain = 'evidence'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(checkpoint, failed.source_checkpoint_json);
            Ok(serde_json::json!({ "imported_records": 0 }))
        })?;
        let complete = db
            .storage_domain("evidence")?
            .expect("complete storage domain row");
        assert_eq!(complete.import_status, "complete");
        assert_eq!(complete.canonical_source, "db");
        Ok(())
    }

    #[test]
    fn audit_event_import_failure_is_retryable_and_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut invalid = AuditEvent::new("legacy_audit", serde_json::json!({ "n": 1 }));
        invalid.id = "audit-1".into();
        invalid.event_seq = u64::MAX;

        let error = db
            .audit_events()
            .import_legacy(Some("agent-a"), vec![invalid])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain audit_events"));
        let failed = db
            .storage_domain("audit_events")?
            .expect("failed storage domain row");
        assert_eq!(failed.import_status, "failed");

        let mut valid = AuditEvent::new("legacy_audit", serde_json::json!({ "n": 1 }));
        valid.id = "audit-1".into();
        valid.event_seq = 7;
        db.audit_events()
            .import_legacy(Some("agent-a"), vec![valid.clone()])?;
        db.audit_events()
            .import_legacy(Some("agent-a"), vec![valid])?;

        let complete = db
            .storage_domain("audit_events")?
            .expect("complete storage domain row");
        assert_eq!(complete.import_status, "complete");
        assert_eq!(complete.canonical_source, "db");
        let imported = db.audit_events().recent(Some("agent-a"), 10)?;
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].id, "audit-1");
        assert_eq!(imported[0].event_seq, 7);
        Ok(())
    }

    #[test]
    fn cutover_diagnostics_detect_missing_failed_and_mixed_sources() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let missing = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(missing
            .iter()
            .any(|diagnostic| diagnostic.contains("storage domain work_items is missing")));

        db.transaction(|tx| {
            upsert_storage_domain(tx, "work_items", "complete", "jsonl", None)?;
            upsert_storage_domain(
                tx,
                "tasks",
                "failed",
                "jsonl",
                Some(serde_json::json!({ "error": "forced failure" })),
            )?;
            upsert_storage_domain(tx, "external_triggers", "complete", "db", None)?;
            upsert_storage_domain(tx, "evidence", "complete", "db", None)?;
            upsert_storage_domain(tx, "audit_events", "complete", "db", None)?;
            Ok(())
        })?;

        let diagnostics = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain work_items has canonical_source=jsonl")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain tasks import failed")
                && diagnostic.contains("forced failure")
        }));
        assert!(db
            .validate_expected_storage_domains(RuntimeDb::expected_storage_domains())
            .is_err());
        Ok(())
    }

    #[test]
    fn turn_record_repository_imports_legacy_evidence_without_turns_jsonl() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut message = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "derive a turn record".into(),
            },
        );
        message.id = "msg-1".into();
        message.message_seq = Some(7);
        message.turn_id = Some("turn-a".into());
        let mut brief = BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "derived result",
            Some("msg-1".into()),
            None,
        );
        brief.id = "brief-1".into();
        brief.turn_id = Some("turn-a".into());
        brief.turn_index = Some(7);
        let tool = ToolExecutionRecord {
            id: "tool-1".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-1".into()),
            turn_index: 7,
            turn_id: Some("turn-a".into()),
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: crate::types::AuthorityClass::RuntimeInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "true" }),
            output: serde_json::json!({ "exit": 0 }),
            summary: "Run command: true".into(),
            invocation_surface: None,
        };

        db.turn_records().import_legacy(
            vec![serde_json::to_value(&message)?],
            vec![tool],
            vec![brief],
            Vec::new(),
            Vec::new(),
        )?;

        let records = db.turn_records().recent_for_agent("agent-a", 10)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].turn_id, "turn-a");
        assert_eq!(records[0].turn_index, 7);
        assert_eq!(records[0].input_message_ids, vec!["msg-1"]);
        assert_eq!(records[0].produced_brief_ids, vec!["brief-1"]);
        assert_eq!(records[0].tool_execution_ids, vec!["tool-1"]);
        assert_eq!(records[0].current_work_item_id.as_deref(), Some("work-1"));
        let domain = db
            .storage_domain("turn_records")?
            .expect("turn_records domain");
        assert_eq!(domain.canonical_source, "db");
        assert!(domain
            .source_checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("turns.jsonl")));
        Ok(())
    }

    #[test]
    fn runtime_db_temp_helper_uses_isolated_state_dir() -> Result<()> {
        let temp_db = test_support::TempRuntimeDb::new()?;
        assert!(temp_db.db.path().ends_with("state/runtime.sqlite"));
        assert!(temp_db.db.lock_path().ends_with("state/runtime.lock"));
        assert_eq!(
            temp_db.db.current_schema_version()?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn external_trigger_import_normalizes_to_one_default_active_per_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older =
            external_trigger_record("trigger-older", "agent-a", ExternalTriggerStatus::Active, 0);
        let newer = external_trigger_record(
            "trigger-newer",
            "agent-a",
            ExternalTriggerStatus::Active,
            10,
        );

        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;

        let active = db
            .external_triggers()
            .active_default_for_agent("agent-a")?
            .expect("active default trigger");
        assert_eq!(active.external_trigger_id, "trigger-newer");
        assert_eq!(active.scope, ExternalTriggerScope::Agent);
        assert_eq!(active.delivery_mode, CallbackDeliveryMode::WakeHint);

        let all = db.external_triggers().latest_for_agent("agent-a")?;
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.into_iter()
                .filter(|record| record.status == ExternalTriggerStatus::Active)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn external_trigger_latest_for_agent_limit_uses_bounded_recent_results() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;

        for index in 0..4 {
            db.external_triggers().upsert(&external_trigger_record(
                &format!("trigger-{index}"),
                "agent-a",
                ExternalTriggerStatus::Revoked,
                index,
            ))?;
        }
        db.external_triggers().upsert(&external_trigger_record(
            "trigger-other-agent",
            "agent-b",
            ExternalTriggerStatus::Revoked,
            10,
        ))?;

        let recent = db
            .external_triggers()
            .latest_for_agent_limit("agent-a", 2)?;
        assert_eq!(
            recent
                .into_iter()
                .map(|record| record.external_trigger_id)
                .collect::<Vec<_>>(),
            vec!["trigger-3", "trigger-2"]
        );
        assert!(db
            .external_triggers()
            .latest_for_agent_limit("agent-a", 0)?
            .is_empty());
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_tracks_delivery_and_token_lookup() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let mut trigger = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        trigger.delivery_mode = CallbackDeliveryMode::WakeHint;
        db.external_triggers().upsert(&trigger)?;

        trigger.delivery_count = 2;
        trigger.last_delivered_at = Some(trigger.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&trigger)?;

        let by_token = db
            .external_triggers()
            .active_by_token_hash("hash-trigger-active")?
            .expect("active trigger by token");
        assert_eq!(by_token.delivery_count, 2);
        assert_eq!(by_token.last_delivered_at, trigger.last_delivered_at);
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_does_not_revert_newer_revocation() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let active = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        db.external_triggers().upsert(&active)?;

        let mut revoked = active.clone();
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(active.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&revoked)?;
        db.external_triggers().upsert(&active)?;

        let latest = db
            .external_triggers()
            .latest("trigger-active")?
            .expect("latest trigger");
        assert_eq!(latest.status, ExternalTriggerStatus::Revoked);
        assert_eq!(latest.revoked_at, revoked.revoked_at);
        Ok(())
    }

    #[test]
    fn work_item_import_is_idempotent_and_preserves_latest_revision() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut older = WorkItemRecord::new("agent-a", "older objective", WorkItemState::Open);
        older.id = "work-test".into();
        older.revision = 1;
        older.updated_at = older.created_at;
        let mut newer = older.clone();
        newer.objective = "newer objective".into();
        newer.revision = 3;
        newer.updated_at = older.updated_at + chrono::Duration::seconds(10);

        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()], Some("work-test"))?;
        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()], Some("work-test"))?;

        let imported = db
            .work_items()
            .latest("work-test")?
            .expect("work item imported");
        assert_eq!(imported.revision, 3);
        assert_eq!(imported.objective, "newer objective");
        let connection = db.connection()?;
        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM work_items", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let current_focus: i64 = connection.query_row(
            "SELECT current_focus FROM work_items WHERE work_item_id = 'work-test'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(current_focus, 1);
        Ok(())
    }

    #[test]
    fn work_item_upsert_rejects_revision_rollback() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new(), None)?;
        let mut current = WorkItemRecord::new("agent-a", "current", WorkItemState::Open);
        current.id = "work-revision".into();
        current.revision = 5;
        db.work_items().upsert(&current, false)?;

        let mut stale = current.clone();
        stale.objective = "stale".into();
        stale.revision = 4;
        stale.updated_at = current.updated_at + chrono::Duration::seconds(10);
        db.work_items().upsert(&stale, false)?;

        let persisted = db
            .work_items()
            .latest("work-revision")?
            .expect("work item persisted");
        assert_eq!(persisted.revision, 5);
        assert_eq!(persisted.objective, "current");
        Ok(())
    }

    #[test]
    fn work_item_listing_is_partitioned_by_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new(), None)?;
        let mut first = WorkItemRecord::new("agent-a", "first", WorkItemState::Open);
        first.id = "work-first".into();
        let mut second = WorkItemRecord::new("agent-b", "second", WorkItemState::Open);
        second.id = "work-second".into();
        db.work_items().upsert(&first, false)?;
        db.work_items().upsert(&second, false)?;

        let agent_items = db.work_items().latest_for_agent("agent-a", 20)?;
        assert_eq!(agent_items.len(), 1);
        assert_eq!(agent_items[0].id, "work-first");
        Ok(())
    }

    #[test]
    fn task_import_is_idempotent_and_preserves_latest_lifecycle_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-import", "agent-a", TaskStatus::Queued, 0);
        let completed = task_record("task-import", "agent-a", TaskStatus::Completed, 10);

        db.tasks()
            .import_legacy(vec![queued.clone(), completed.clone()])?;
        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-import")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-import.log")
        );
        let connection = db.connection()?;
        let rows: i64 = connection.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let terminal_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'completed' AND completed_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(terminal_rows, 1);
        Ok(())
    }

    #[test]
    fn task_import_merges_legacy_metadata_when_latest_update_is_sparse() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-sparse", "agent-a", TaskStatus::Queued, 0);
        let mut completed = task_record("task-sparse", "agent-a", TaskStatus::Completed, 10);
        completed.summary = None;
        completed.detail = None;
        completed.recovery = None;

        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-sparse")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(imported.summary.as_deref(), Some("task-sparse"));
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-sparse.log")
        );
        Ok(())
    }

    #[test]
    fn task_parent_agent_column_is_only_set_for_child_agent_tasks() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let command = task_record("task-command", "agent-a", TaskStatus::Running, 0);
        let mut child = task_record("task-child", "agent-a", TaskStatus::Running, 1);
        child.detail = Some(serde_json::json!({
            "child_agent_id": "child-a",
            "input_target": "child_followup"
        }));

        db.tasks().upsert(&command)?;
        db.tasks().upsert(&child)?;

        let connection = db.connection()?;
        let command_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-command'",
            [],
            |row| row.get(0),
        )?;
        let child_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-child'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(command_parent, None);
        assert_eq!(child_parent.as_deref(), Some("agent-a"));
        Ok(())
    }

    #[test]
    fn task_payload_json_slimguards_large_preview_fields() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut task = task_record("task-large", "agent-a", TaskStatus::Running, 0);
        task.detail = Some(serde_json::json!({
            "output_path": "/tmp/task-large.log",
            "initial_output": "i".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "output_summary": "s".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "lines": (0..(TASK_PAYLOAD_ARRAY_LIMIT + 10)).collect::<Vec<_>>()
        }));

        db.tasks().upsert(&task)?;

        let connection = db.connection()?;
        let (payload_json, result_summary): (String, Option<String>) = connection.query_row(
            "SELECT payload_json, result_summary FROM tasks WHERE task_id = 'task-large'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
        let detail = &payload["detail"];
        assert!(detail.get("initial_output").is_none());
        assert_eq!(
            detail["output_summary"].as_str().expect("summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        assert_eq!(
            detail["lines"].as_array().expect("lines").len(),
            TASK_PAYLOAD_ARRAY_LIMIT
        );
        assert_eq!(
            result_summary.expect("result summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        Ok(())
    }

    #[test]
    fn task_active_listing_is_partitioned_by_agent_and_excludes_terminal() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.tasks().import_legacy(Vec::new())?;
        db.tasks().upsert(&task_record(
            "agent-a-running",
            "agent-a",
            TaskStatus::Running,
            1,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-a-completed",
            "agent-a",
            TaskStatus::Completed,
            2,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-b-running",
            "agent-b",
            TaskStatus::Running,
            3,
        ))?;

        let active = db.tasks().active_for_agent("agent-a", 20)?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "agent-a-running");
        let all_agent_a = db.tasks().latest_for_agent("agent-a", 20)?;
        assert_eq!(all_agent_a.len(), 2);
        Ok(())
    }

    #[test]
    fn runtime_db_lock_rejects_second_nonblocking_holder() -> Result<()> {
        if let Ok(lock_path) = std::env::var("HOLON_RUNTIME_DB_LOCK_CHILD_PATH") {
            RuntimeDbLock::try_lock(lock_path).expect_err("second process should not get lock");
            return Ok(());
        }

        let temp_dir = tempdir()?;
        let lock_path = temp_dir.path().join("state/runtime.lock");
        let first = RuntimeDbLock::lock(&lock_path)?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_db::tests::runtime_db_lock_rejects_second_nonblocking_holder")
            .arg("--nocapture")
            .env("HOLON_RUNTIME_DB_LOCK_CHILD_PATH", &lock_path)
            .output()?;
        assert!(
            output.status.success(),
            "child lock assertion failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(first);

        let second = RuntimeDbLock::try_lock(&lock_path)?;
        assert_eq!(second.path(), lock_path.as_path());
        Ok(())
    }
    #[test]
    fn backfill_wait_condition_payload_columns_adds_columns_and_fills_data() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
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

        super::backfill_wait_condition_payload_columns(&conn)?;

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
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
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

        super::backfill_wait_condition_payload_columns(&conn)?;

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
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        super::backfill_wait_condition_payload_columns(&conn)?;
        Ok(())
    }

    #[test]
    fn backfill_work_item_recheck_columns_adds_columns_and_fills_data() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
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

        super::backfill_work_item_recheck_columns(&conn)?;

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
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
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

        super::backfill_work_item_recheck_columns(&conn)?;

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
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        super::backfill_work_item_recheck_columns(&conn)?;
        Ok(())
    }
}
