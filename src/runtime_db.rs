use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::types::{TaskRecord, TaskStatus, WorkItemRecord, WorkItemState};

const TASK_PAYLOAD_STRING_LIMIT: usize = 2048;
const TASK_PAYLOAD_ARRAY_LIMIT: usize = 64;

#[derive(Debug, Clone)]
pub struct RuntimeDb {
    path: PathBuf,
    lock_path: PathBuf,
}

pub struct WorkItemRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TaskRepository<'a> {
    db: &'a RuntimeDb,
}

impl WorkItemRepository<'_> {
    pub fn import_legacy(
        &self,
        records: Vec<WorkItemRecord>,
        current_work_item_id: Option<&str>,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let domain = read_storage_domain(tx, "work_items")?;
            if domain.as_ref().is_some_and(|domain| {
                domain.import_status == "complete" && domain.canonical_source == "db"
            }) {
                return Ok(());
            }

            upsert_storage_domain(tx, "work_items", "importing", "jsonl", None)?;
            let mut latest = BTreeMap::<String, WorkItemRecord>::new();
            for record in records {
                let should_replace = latest
                    .get(&record.id)
                    .is_none_or(|existing| newer_work_item_record(&record, existing));
                if should_replace {
                    latest.insert(record.id.clone(), record);
                }
            }
            for record in latest.values() {
                upsert_work_item_tx(tx, record, current_work_item_id == Some(record.id.as_str()))?;
            }
            upsert_storage_domain(
                tx,
                "work_items",
                "complete",
                "db",
                Some(serde_json::json!({ "imported_records": latest.len() })),
            )?;
            Ok(())
        })
    }

    pub fn upsert(&self, record: &WorkItemRecord, current_focus: bool) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_tx(tx, record, current_focus))
    }

    pub fn set_current_focus(&self, agent_id: &str, work_item_id: Option<&str>) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET current_focus = 0 WHERE agent_id = ?1 AND current_focus != 0",
                [agent_id],
            )?;
            if let Some(work_item_id) = work_item_id {
                tx.execute(
                    "UPDATE work_items SET current_focus = 1 WHERE agent_id = ?1 AND work_item_id = ?2",
                    params![agent_id, work_item_id],
                )?;
            }
            Ok(())
        })
    }

    pub fn latest(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                [work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<WorkItemRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }
}

impl TaskRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TaskRecord>) -> Result<()> {
        self.db.transaction(|tx| {
            let domain = read_storage_domain(tx, "tasks")?;
            if domain.as_ref().is_some_and(|domain| {
                domain.import_status == "complete" && domain.canonical_source == "db"
            }) {
                return Ok(());
            }

            upsert_storage_domain(tx, "tasks", "importing", "jsonl", None)?;
            let latest = reduce_task_records(records);
            for record in latest.values() {
                upsert_task_tx(tx, record)?;
            }
            let active_records = latest
                .values()
                .filter(|record| is_active_task_status(&record.status))
                .count();
            upsert_storage_domain(
                tx,
                "tasks",
                "complete",
                "db",
                Some(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                })),
            )?;
            Ok(())
        })
    }

    pub fn upsert(&self, record: &TaskRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_task_tx(tx, record))
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_task_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TaskRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM tasks
             ORDER BY updated_at DESC, created_at DESC, task_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(agent_id, "owner_agent_id = ?1", [agent_id], limit)
    }

    pub fn active_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(
            agent_id,
            "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
            [agent_id],
            limit,
        )
    }

    fn query_for_agent(
        &self,
        _agent_id: &str,
        where_clause: &str,
        params: [&str; 1],
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let sql = format!(
            "SELECT payload_json
             FROM tasks
             WHERE {where_clause}
             ORDER BY updated_at DESC, created_at DESC, task_id ASC
             LIMIT {limit}",
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }
}

#[derive(Debug)]
struct StorageDomainState {
    import_status: String,
    canonical_source: String,
}

fn read_storage_domain(tx: &Transaction<'_>, domain: &str) -> Result<Option<StorageDomainState>> {
    tx.query_row(
        "SELECT import_status, canonical_source FROM storage_domains WHERE domain = ?1",
        [domain],
        |row| {
            Ok(StorageDomainState {
                import_status: row.get(0)?,
                canonical_source: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn upsert_storage_domain(
    tx: &Transaction<'_>,
    domain: &str,
    import_status: &str,
    canonical_source: &str,
    checkpoint: Option<serde_json::Value>,
) -> Result<()> {
    let now = timestamp(Utc::now());
    let imported_at = (import_status == "complete").then(|| now.clone());
    let checkpoint = checkpoint.map(|value| value.to_string());
    tx.execute(
        "INSERT INTO storage_domains (
            domain, schema_version, import_status, canonical_source,
            source_checkpoint_json, imported_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(domain) DO UPDATE SET
            schema_version = excluded.schema_version,
            import_status = excluded.import_status,
            canonical_source = excluded.canonical_source,
            source_checkpoint_json = excluded.source_checkpoint_json,
            imported_at = excluded.imported_at,
            updated_at = excluded.updated_at",
        params![
            domain,
            max_known_migration_version(),
            import_status,
            canonical_source,
            checkpoint,
            imported_at,
            now
        ],
    )?;
    Ok(())
}

fn upsert_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    current_focus: bool,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let readiness = enum_string(&record.readiness())?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    tx.execute(
        "INSERT INTO work_items (
            work_item_id, agent_id, state, objective, plan_status, readiness,
            revision, current_focus, created_at, updated_at, completed_at,
            plan_artifact_path, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(work_item_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            state = excluded.state,
            objective = excluded.objective,
            plan_status = excluded.plan_status,
            readiness = excluded.readiness,
            revision = excluded.revision,
            current_focus = excluded.current_focus,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            plan_artifact_path = excluded.plan_artifact_path,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision >= work_items.revision",
        params![
            record.id,
            record.agent_id,
            state,
            record.objective,
            plan_status,
            readiness,
            record.revision as i64,
            i64::from(current_focus),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_task_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<()> {
    let kind = record.kind.as_str();
    let status = enum_string(&record.status)?;
    let child_agent_id = task_detail_string(&record.detail, "child_agent_id");
    let parent_agent_id = child_agent_id.as_ref().map(|_| record.agent_id.clone());
    let input_target = task_detail_string(&record.detail, "input_target");
    let wait_policy = enum_string(&record.wait_policy())?;
    let output_path = task_detail_string(&record.detail, "output_path");
    let result_summary = task_detail_string(&record.detail, "output_summary")
        .map(|summary| truncate_task_payload_string(&summary));
    let exit_status = task_detail_i64(&record.detail, "exit_status");
    let terminal_reentry = i64::from(record.terminal_reentry());
    let completed_at =
        is_terminal_task_status(&record.status).then(|| timestamp(record.updated_at));
    let payload_json = serde_json::to_string(&slim_task_record_for_payload(record))?;
    tx.execute(
        "INSERT INTO tasks (
            task_id, owner_agent_id, parent_agent_id, child_agent_id, kind, status,
            summary, input_target, wait_policy, output_path, result_summary,
            exit_status, terminal_reentry, revision, created_at, updated_at,
            completed_at, last_message_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(task_id) DO UPDATE SET
            owner_agent_id = excluded.owner_agent_id,
            parent_agent_id = excluded.parent_agent_id,
            child_agent_id = excluded.child_agent_id,
            kind = excluded.kind,
            status = excluded.status,
            summary = excluded.summary,
            input_target = excluded.input_target,
            wait_policy = excluded.wait_policy,
            output_path = excluded.output_path,
            result_summary = excluded.result_summary,
            exit_status = excluded.exit_status,
            terminal_reentry = excluded.terminal_reentry,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            last_message_id = excluded.last_message_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision >= tasks.revision",
        params![
            record.id,
            record.agent_id,
            parent_agent_id,
            child_agent_id,
            kind,
            status,
            record.summary,
            input_target,
            wait_policy,
            output_path,
            result_summary,
            exit_status,
            terminal_reentry,
            task_revision(record),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            record.parent_message_id,
            payload_json,
        ],
    )?;
    Ok(())
}

fn newer_work_item_record(candidate: &WorkItemRecord, existing: &WorkItemRecord) -> bool {
    candidate
        .revision
        .cmp(&existing.revision)
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

fn reduce_task_records(records: Vec<TaskRecord>) -> BTreeMap<String, TaskRecord> {
    let mut latest = BTreeMap::<String, TaskRecord>::new();
    for record in records {
        if let Some(previous) = latest.get(&record.id) {
            let mut merged = record.clone();
            if merged.summary.is_none() {
                merged.summary = previous.summary.clone();
            }
            if merged.detail.is_none() {
                merged.detail = previous.detail.clone();
            }
            if merged.recovery.is_none() {
                merged.recovery = previous.recovery.clone();
            }
            if newer_task_record(&merged, previous) {
                latest.insert(record.id.clone(), merged);
            }
        } else {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

fn slim_task_record_for_payload(record: &TaskRecord) -> TaskRecord {
    let mut slim = record.clone();
    slim.detail = slim.detail.as_ref().map(slim_task_detail_value);
    slim
}

fn slim_task_detail_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut slim = serde_json::Map::new();
            for (key, value) in map {
                if key == "initial_output" {
                    continue;
                }
                slim.insert(key.clone(), slim_task_detail_value(value));
            }
            serde_json::Value::Object(slim)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(TASK_PAYLOAD_ARRAY_LIMIT)
                .map(slim_task_detail_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_task_payload_string(value))
        }
        _ => value.clone(),
    }
}

fn truncate_task_payload_string(value: &str) -> String {
    value.chars().take(TASK_PAYLOAD_STRING_LIMIT).collect()
}

fn newer_task_record(candidate: &TaskRecord, existing: &TaskRecord) -> bool {
    task_revision(candidate)
        .cmp(&task_revision(existing))
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

fn task_revision(record: &TaskRecord) -> i64 {
    record.updated_at.timestamp_millis()
}

fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}

fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

fn task_detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn task_detail_i64(detail: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_i64())
}

fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Debug)]
pub struct RuntimeDbLock {
    file: File,
    path: PathBuf,
}

#[derive(Debug)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "runtime_db_foundation",
        sql: r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS storage_domains (
  domain TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL,
  import_status TEXT NOT NULL,
  canonical_source TEXT NOT NULL,
  source_checkpoint_json TEXT,
  imported_at TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
  agent_id TEXT PRIMARY KEY,
  status TEXT,
  visibility TEXT,
  ownership TEXT,
  profile_preset TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT
);

CREATE TABLE IF NOT EXISTS audit_events (
  audit_event_id TEXT PRIMARY KEY,
  event_seq INTEGER,
  agent_id TEXT,
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_storage_domains_import_status
  ON storage_domains(import_status);

CREATE INDEX IF NOT EXISTS idx_agents_status
  ON agents(status);

CREATE INDEX IF NOT EXISTS idx_audit_events_agent_created
  ON audit_events(agent_id, created_at);

CREATE INDEX IF NOT EXISTS idx_audit_events_event_seq
  ON audit_events(event_seq);
"#,
    },
    Migration {
        version: 2,
        name: "work_items_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_items (
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
);

CREATE INDEX IF NOT EXISTS idx_work_items_agent
  ON work_items(agent_id);

CREATE INDEX IF NOT EXISTS idx_work_items_state
  ON work_items(state);

CREATE INDEX IF NOT EXISTS idx_work_items_readiness
  ON work_items(readiness);

CREATE INDEX IF NOT EXISTS idx_work_items_current_focus
  ON work_items(agent_id, current_focus);
"#,
    },
    Migration {
        version: 3,
        name: "tasks_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS tasks (
  task_id TEXT PRIMARY KEY,
  owner_agent_id TEXT NOT NULL,
  parent_agent_id TEXT,
  child_agent_id TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  summary TEXT,
  input_target TEXT,
  wait_policy TEXT,
  output_path TEXT,
  result_summary TEXT,
  exit_status INTEGER,
  terminal_reentry INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  last_turn_id TEXT,
  last_message_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_owner_agent
  ON tasks(owner_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_parent_agent
  ON tasks(parent_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_child_agent
  ON tasks(child_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_status
  ON tasks(status);

CREATE INDEX IF NOT EXISTS idx_tasks_owner_active
  ON tasks(owner_agent_id, status, updated_at);
"#,
    },
];

impl RuntimeDb {
    pub fn open_and_migrate(
        path: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let db = Self {
            path: path.into(),
            lock_path: lock_path.into(),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn connection(&self) -> Result<Connection> {
        open_connection(&self.path)
    }

    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        match f(&transaction) {
            Ok(value) => {
                transaction.commit()?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    pub fn current_schema_version(&self) -> Result<i64> {
        let connection = self.connection()?;
        current_schema_version(&connection)
    }

    pub fn work_items(&self) -> WorkItemRepository<'_> {
        WorkItemRepository { db: self }
    }

    pub fn tasks(&self) -> TaskRepository<'_> {
        TaskRepository { db: self }
    }

    pub fn migrate(&self) -> Result<()> {
        let _lock = RuntimeDbLock::lock(&self.lock_path)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
        }
        let mut connection = self.connection()?;
        ensure_migration_table(&connection)?;
        let current_version = current_schema_version(&connection)?;
        let max_known_version = max_known_migration_version();
        if current_version > max_known_version {
            bail!(
                "runtime db schema version {} is newer than this binary supports ({})",
                current_version,
                max_known_version
            );
        }
        for migration in MIGRATIONS {
            apply_migration(&mut connection, migration)?;
        }
        Ok(())
    }
}

impl RuntimeDbLock {
    pub fn lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::Blocking)
    }

    pub fn try_lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::NonBlocking)
    }

    fn open(path: PathBuf, mode: LockMode) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime lock directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening runtime db lock {}", path.display()))?;
        flock(&file, mode).with_context(|| format!("locking runtime db {}", path.display()))?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RuntimeDbLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Blocking,
    NonBlocking,
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
"#,
    )?;
    Ok(())
}

fn ensure_migration_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
"#,
    )?;
    Ok(())
}

fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<()> {
    let existing_name: Option<String> = connection
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = ?1",
            [migration.version],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_name) = existing_name {
        if existing_name != migration.name {
            bail!(
                "runtime db migration {} name mismatch: expected {}, found {}",
                migration.version,
                migration.name,
                existing_name
            );
        }
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        (
            migration.version,
            migration.name,
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    )?;
    transaction.commit()?;
    Ok(())
}

fn current_schema_version(connection: &Connection) -> Result<i64> {
    ensure_migration_table(connection)?;
    let version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

fn max_known_migration_version() -> i64 {
    MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

#[cfg(unix)]
fn flock(file: &File, mode: LockMode) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut operation = libc::LOCK_EX;
    if matches!(mode, LockMode::NonBlocking) {
        operation |= libc::LOCK_NB;
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(mode, LockMode::NonBlocking) {
        if let Some(raw_error) = error.raw_os_error() {
            if raw_error == libc::EWOULDBLOCK || raw_error == libc::EAGAIN {
                return Err(anyhow!("runtime db lock is already held"));
            }
        }
    }
    Err(error.into())
}

#[cfg(unix)]
fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
fn flock(_file: &File, _mode: LockMode) -> Result<()> {
    bail!("runtime db file lock is only implemented on Unix platforms")
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> Result<()> {
    Ok(())
}

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
    use std::process::Command;
    use tempfile::tempdir;

    fn temp_paths() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("state/runtime.sqlite");
        let lock_path = temp_dir.path().join("state/runtime.lock");
        Ok((temp_dir, db_path, lock_path))
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
                "input_target": "stdin",
            })),
            recovery: None,
        }
    }

    #[test]
    fn runtime_db_fresh_migration_creates_foundation_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;

        let version = db.current_schema_version()?;
        assert_eq!(version, 3);
        for table in [
            "schema_migrations",
            "storage_domains",
            "agents",
            "audit_events",
            "work_items",
            "tasks",
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
    fn runtime_db_migration_is_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, 3);
        assert_eq!(current_schema_version(&connection)?, 3);
        Ok(())
    }

    #[test]
    fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            (7_i64, "future_test", Utc::now().to_rfc3339()),
        )?;

        assert_eq!(current_schema_version(&connection)?, 7);
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
    fn runtime_db_foreign_keys_are_enabled_per_connection() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(enabled, 1);
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
    fn runtime_db_temp_helper_uses_isolated_state_dir() -> Result<()> {
        let temp_db = test_support::TempRuntimeDb::new()?;
        assert!(temp_db.db.path().ends_with("state/runtime.sqlite"));
        assert!(temp_db.db.lock_path().ends_with("state/runtime.lock"));
        assert_eq!(temp_db.db.current_schema_version()?, 3);
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
            "input_target": "child_followup",
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
            "lines": (0..(TASK_PAYLOAD_ARRAY_LIMIT + 10)).collect::<Vec<_>>(),
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
}
