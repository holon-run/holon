//! Runtime database facade: connection management, write queue, and domain repositories.
//!
//! This module is split into submodules by storage responsibility:
//! - [`types`]: public types (repository handles, index outbox, storage domain).
//! - [`write_queue`]: background writer task and write queue.
//! - [`connection`]: SQLite connection setup, retry, and file locking.
//! - [`migrations`]: schema migrations.
//! - [`storage_domain`]: storage domain checkpoint helpers.
//! - [`evidence`]: evidence insertion and query types.
//! - [`repositories`]: domain repository implementations.
//! - [`index_outbox`]: runtime index outbox repository.

pub mod connection;
pub mod evidence;
pub mod migrations;
pub mod repositories;
pub mod storage_domain;
pub(crate) mod transitions;
pub mod types;
pub mod write_queue;

mod index_outbox;

// Re-export public types that are referenced as `crate::runtime_db::Type`.
pub use crate::runtime_db::evidence::{
    EvidenceKind, EvidencePayloadRow, EvidenceQuery, EvidenceRow,
};
pub use crate::runtime_db::index_outbox::{
    RuntimeIndexChange, RuntimeIndexOperation, RuntimeIndexOutboxRepository, RuntimeIndexOutboxRow,
};
pub use crate::runtime_db::storage_domain::{ExpectedStorageDomain, StorageDomainSnapshot};
pub use crate::runtime_db::types::{
    AgentIdentityRepository, AgentStateRepository, AuditEventSink, ContextEpisodeRepository,
    EvidenceRepository, ExecutionRootEntryRepository, ExternalTriggerRepository, MessageRepository,
    OperatorDeliveryRepository, OperatorNotificationRepository, OperatorTransportBindingRepository,
    QueueEntryRepository, TaskRepository, TimerRepository, TranscriptRepository,
    TurnRecordRepository, WaitConditionRepository, WorkItemContinuationRepository,
    WorkItemDelegationRepository, WorkItemRepository, WorkspaceEntryRepository,
    WorkspaceOccupancyRepository,
};
#[cfg(test)]
mod tests;

use std::{
    error::Error as StdError,
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::runtime_db::connection::{
    configure_persistent_database, flock, open_connection, unlock, LockMode,
};
use crate::runtime_db::migrations::{
    apply_migration, backfill_wait_condition_payload_columns, backfill_work_item_recheck_columns,
    current_schema_version, ensure_migration_table, max_known_migration_version, MIGRATIONS,
};
use crate::runtime_db::storage_domain::{
    read_storage_domain_connection, upsert_storage_domain, upsert_storage_domain_checkpoint_json,
};
use crate::runtime_db::write_queue::{RuntimeDbWriteContext, RuntimeDbWriter};

pub(crate) const TASK_PAYLOAD_STRING_LIMIT: usize = 2048;
pub(crate) const TASK_PAYLOAD_ARRAY_LIMIT: usize = 64;
pub(crate) const EVIDENCE_PREVIEW_LIMIT: usize = 2048;
pub(crate) const CONTEXT_EPISODE_ANCHORS_DOMAIN: &str = "context_episode_anchors";
pub(crate) const RUNTIME_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);
pub(crate) const RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
pub(crate) const RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY: Duration = Duration::from_millis(1_000);
pub(crate) const RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const RUNTIME_DB_APPEND_RETRY_MAX_DELAY: Duration = Duration::from_millis(5_000);
pub(crate) const RUNTIME_DB_WRITE_QUEUE_CAPACITY: usize = 1024;

#[derive(Debug)]
pub struct RuntimeDbRetryableError {
    operation: &'static str,
    path: PathBuf,
    source: String,
}

impl RuntimeDbRetryableError {
    pub(crate) fn new(operation: &'static str, path: &Path, source: impl fmt::Display) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            source: source.to_string(),
        }
    }
}

impl fmt::Display for RuntimeDbRetryableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "retryable runtime db error while {} for {}: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl StdError for RuntimeDbRetryableError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStateTransitionConflict {
    domain: &'static str,
    record_id: String,
    code: &'static str,
    existing_status: String,
    incoming_status: String,
    expected_revision: Option<u64>,
    actual_revision: Option<u64>,
    retryable: bool,
}

impl RuntimeStateTransitionConflict {
    pub(crate) fn new(
        domain: &'static str,
        record_id: impl Into<String>,
        existing_status: impl Into<String>,
        incoming_status: impl Into<String>,
    ) -> Self {
        Self {
            domain,
            record_id: record_id.into(),
            code: "state_transition_conflict",
            existing_status: existing_status.into(),
            incoming_status: incoming_status.into(),
            expected_revision: None,
            actual_revision: None,
            retryable: false,
        }
    }

    pub(crate) fn revision(
        record_id: impl Into<String>,
        code: &'static str,
        expected_revision: Option<u64>,
        actual_revision: Option<u64>,
        retryable: bool,
    ) -> Self {
        Self {
            domain: "work_item",
            record_id: record_id.into(),
            code,
            existing_status: actual_revision
                .map_or_else(|| "missing".to_string(), |revision| revision.to_string()),
            incoming_status: expected_revision
                .map_or_else(|| "none".to_string(), |revision| revision.to_string()),
            expected_revision,
            actual_revision,
            retryable,
        }
    }

    pub fn domain(&self) -> &'static str {
        self.domain
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn existing_status(&self) -> &str {
        &self.existing_status
    }

    pub fn incoming_status(&self) -> &str {
        &self.incoming_status
    }

    pub fn expected_revision(&self) -> Option<u64> {
        self.expected_revision
    }

    pub fn actual_revision(&self) -> Option<u64> {
        self.actual_revision
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }
}

impl fmt::Display for RuntimeStateTransitionConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.expected_revision.is_some() || self.actual_revision.is_some() {
            write!(
                formatter,
                "{} for {} {}: expected revision {:?}, actual revision {:?}",
                self.code,
                self.domain,
                self.record_id,
                self.expected_revision,
                self.actual_revision
            )
        } else {
            write!(
                formatter,
                "conflicting {} transition for {}: {} -> {}",
                self.domain, self.record_id, self.existing_status, self.incoming_status
            )
        }
    }
}

impl StdError for RuntimeStateTransitionConflict {}

#[derive(Clone)]
pub struct RuntimeDb {
    path: PathBuf,
    lock_path: PathBuf,
    writer: RuntimeDbWriter,
}

impl fmt::Debug for RuntimeDb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeDb")
            .field("path", &self.path)
            .field("lock_path", &self.lock_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct RuntimeDbLock {
    file: File,
    path: PathBuf,
}

impl RuntimeDb {
    pub fn open_and_migrate(
        path: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let path = path.into();
        let writer = RuntimeDbWriter::open(path.clone(), open_connection(&path)?)?;
        let db = Self {
            writer,
            path,
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

    pub fn transaction<T>(&self, f: impl FnMut(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.writer.append_wait(f)
    }

    fn transaction_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.writer.append_wait_with_context(context, f)
    }

    fn transaction_once<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.writer.append_wait_once(f)
    }

    pub fn append(
        &self,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.writer.append(f)
    }

    #[cfg(test)]
    pub(crate) fn flush_background_writes_for_tests(&self) -> Result<()> {
        self.writer.flush_background_writes_for_tests()
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

    pub fn external_triggers(&self) -> ExternalTriggerRepository<'_> {
        ExternalTriggerRepository { db: self }
    }

    pub fn wait_conditions(&self) -> WaitConditionRepository<'_> {
        WaitConditionRepository { db: self }
    }

    pub fn queue_entries(&self) -> QueueEntryRepository<'_> {
        QueueEntryRepository { db: self }
    }

    pub fn timers(&self) -> TimerRepository<'_> {
        TimerRepository { db: self }
    }

    pub fn turn_records(&self) -> TurnRecordRepository<'_> {
        TurnRecordRepository { db: self }
    }

    pub fn messages(&self) -> MessageRepository<'_> {
        MessageRepository { db: self }
    }

    pub fn transcript_entries(&self) -> TranscriptRepository<'_> {
        TranscriptRepository { db: self }
    }

    pub fn evidence(&self) -> EvidenceRepository<'_> {
        EvidenceRepository { db: self }
    }

    pub fn audit_events(&self) -> AuditEventSink<'_> {
        AuditEventSink { db: self }
    }

    pub fn agent_states(&self) -> AgentStateRepository<'_> {
        AgentStateRepository { db: self }
    }

    pub fn workspace_entries(&self) -> WorkspaceEntryRepository<'_> {
        WorkspaceEntryRepository { db: self }
    }

    pub fn workspace_occupancies(&self) -> WorkspaceOccupancyRepository<'_> {
        WorkspaceOccupancyRepository { db: self }
    }

    pub fn execution_root_entries(&self) -> ExecutionRootEntryRepository<'_> {
        ExecutionRootEntryRepository { db: self }
    }

    pub fn agent_identities(&self) -> AgentIdentityRepository<'_> {
        AgentIdentityRepository { db: self }
    }

    pub fn work_item_delegations(&self) -> WorkItemDelegationRepository<'_> {
        WorkItemDelegationRepository { db: self }
    }

    pub fn work_item_continuations(&self) -> WorkItemContinuationRepository<'_> {
        WorkItemContinuationRepository { db: self }
    }

    pub fn context_episodes(&self) -> ContextEpisodeRepository<'_> {
        ContextEpisodeRepository { db: self }
    }

    pub fn operator_notifications(&self) -> OperatorNotificationRepository<'_> {
        OperatorNotificationRepository { db: self }
    }

    pub fn operator_transport_bindings(&self) -> OperatorTransportBindingRepository<'_> {
        OperatorTransportBindingRepository { db: self }
    }

    pub fn operator_delivery_records(&self) -> OperatorDeliveryRepository<'_> {
        OperatorDeliveryRepository { db: self }
    }

    pub fn runtime_index_outbox(&self) -> RuntimeIndexOutboxRepository<'_> {
        RuntimeIndexOutboxRepository { db: self }
    }

    pub const fn expected_storage_domains() -> &'static [ExpectedStorageDomain] {
        &[
            ExpectedStorageDomain {
                domain: "agent_states",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "workspace_entries",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "workspace_occupancies",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "agent_identities",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "work_items",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "work_item_delegations",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "work_item_continuations",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: CONTEXT_EPISODE_ANCHORS_DOMAIN,
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "tasks",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "external_triggers",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "wait_conditions",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "queue_entries",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "timers",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "turn_records",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "messages",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "transcript_entries",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "evidence",
                canonical_source: "db",
            },
            ExpectedStorageDomain {
                domain: "audit_events",
                canonical_source: "db",
            },
        ]
    }

    pub fn storage_domain(&self, domain: &str) -> Result<Option<StorageDomainSnapshot>> {
        let connection = self.connection()?;
        read_storage_domain_connection(&connection, domain)
    }

    pub fn storage_domains(&self) -> Result<Vec<StorageDomainSnapshot>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT domain, schema_version, import_status, canonical_source,
                    source_checkpoint_json, imported_at, updated_at
             FROM storage_domains
             ORDER BY domain ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StorageDomainSnapshot {
                domain: row.get(0)?,
                schema_version: row.get(1)?,
                import_status: row.get(2)?,
                canonical_source: row.get(3)?,
                source_checkpoint_json: row.get(4)?,
                imported_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn diagnose_cutover(&self, expected: &[ExpectedStorageDomain]) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut diagnostics = Vec::new();
        let current_version = current_schema_version(&connection)?;
        let max_known_version = max_known_migration_version();
        if current_version < max_known_version {
            diagnostics.push(format!(
                "missing runtime db migration: schema_migrations is at {current_version}, expected {max_known_version}"
            ));
        }
        for expected_domain in expected {
            match read_storage_domain_connection(&connection, expected_domain.domain)? {
                None => diagnostics.push(format!(
                    "storage domain {} is missing; expected canonical_source={}",
                    expected_domain.domain, expected_domain.canonical_source
                )),
                Some(snapshot) => {
                    if snapshot.import_status == "failed" {
                        diagnostics.push(format!(
                            "storage domain {} import failed: {}",
                            snapshot.domain,
                            snapshot
                                .source_checkpoint_json
                                .as_deref()
                                .unwrap_or("no failure checkpoint recorded")
                        ));
                    } else if snapshot.import_status != "complete" {
                        diagnostics.push(format!(
                            "storage domain {} import is {}; expected complete",
                            snapshot.domain, snapshot.import_status
                        ));
                    }
                    if snapshot.canonical_source != expected_domain.canonical_source {
                        diagnostics.push(format!(
                            "storage domain {} has canonical_source={}; expected {}",
                            snapshot.domain,
                            snapshot.canonical_source,
                            expected_domain.canonical_source
                        ));
                    }
                }
            }
        }
        Ok(diagnostics)
    }

    pub fn validate_expected_storage_domains(
        &self,
        expected: &[ExpectedStorageDomain],
    ) -> Result<()> {
        let diagnostics = self.diagnose_cutover(expected)?;
        if diagnostics.is_empty() {
            return Ok(());
        }
        bail!(
            "runtime db cutover diagnostics failed:\n{}",
            diagnostics.join("\n")
        )
    }

    pub(crate) fn storage_domain_is_complete(
        &self,
        domain: &str,
        canonical_source: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let Some(snapshot) = read_storage_domain_connection(&connection, domain)? else {
            return Ok(false);
        };
        Ok(snapshot.import_status == "complete" && snapshot.canonical_source == canonical_source)
    }

    fn run_storage_domain_import(
        &self,
        domain: &'static str,
        importing_source: &'static str,
        complete_source: &'static str,
        import: impl FnOnce(&Transaction<'_>) -> Result<serde_json::Value>,
    ) -> Result<()> {
        self.transaction(|tx| {
            let existing_checkpoint = tx
                .query_row(
                    "SELECT source_checkpoint_json FROM storage_domains WHERE domain = ?1",
                    [domain],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            upsert_storage_domain_checkpoint_json(
                tx,
                domain,
                "importing",
                importing_source,
                existing_checkpoint,
            )
        })?;
        let result = self.transaction_once(|tx| {
            let checkpoint = import(tx)?;
            upsert_storage_domain(tx, domain, "complete", complete_source, Some(checkpoint))?;
            Ok(())
        });
        if let Err(error) = result {
            let checkpoint = serde_json::json!({
                "error": error.to_string(),
                "retry": "restart runtime to retry legacy import",
            });
            self.transaction_once(|tx| {
                upsert_storage_domain(tx, domain, "failed", importing_source, Some(checkpoint))
            })?;
            return Err(error).with_context(|| format!("importing legacy storage domain {domain}"));
        }
        Ok(())
    }

    pub fn migrate(&self) -> Result<()> {
        let _lock = RuntimeDbLock::lock(&self.lock_path)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
        }
        let _turn = self.writer.state.queue.wait_turn()?;
        let mut connection = self
            .writer
            .state
            .connection
            .lock()
            .map_err(|_| anyhow!("runtime db writer mutex poisoned"))?;
        configure_persistent_database(&connection)?;
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
        backfill_wait_condition_payload_columns(&connection)?;
        backfill_work_item_recheck_columns(&connection)?;
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
