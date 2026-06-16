use std::{
    collections::BTreeMap,
    error::Error as StdError,
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{
    ffi::ErrorCode, params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, CallbackDeliveryMode,
    ContextEpisodeRecord, DeliverySummaryRecord, ExternalTriggerRecord, ExternalTriggerScope,
    ExternalTriggerStatus, MessageEnvelope, QueueEntryRecord, QueueEntryStatus, TaskRecord,
    TaskStatus, TimerRecord, TimerStatus, ToolExecutionRecord, TranscriptEntry, TurnRecord,
    WaitConditionRecord, WorkItemContinuationFrame, WorkItemDelegationRecord, WorkItemRecord,
    WorkItemState, WorkspaceEntry, WorkspaceOccupancyRecord,
};

const TASK_PAYLOAD_STRING_LIMIT: usize = 2048;
const TASK_PAYLOAD_ARRAY_LIMIT: usize = 64;
const EVIDENCE_PREVIEW_LIMIT: usize = 2048;
const CONTEXT_EPISODE_ANCHORS_DOMAIN: &str = "context_episode_anchors";
const RUNTIME_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);
const RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY: Duration = Duration::from_millis(1_000);
const RUNTIME_DB_APPEND_RETRY_MAX_DELAY: Duration = Duration::from_millis(5_000);
const RUNTIME_DB_WRITE_QUEUE_CAPACITY: usize = 1024;

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

pub fn is_retryable_db_error(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source.downcast_ref::<RuntimeDbRetryableError>().is_some()
            || source
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(is_sqlite_locked)
    })
}

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

#[derive(Clone)]
struct RuntimeDbWriter {
    state: Arc<RuntimeDbWriterState>,
    append_tx: mpsc::SyncSender<RuntimeDbWriteRequest>,
}

struct RuntimeDbWriterState {
    path: PathBuf,
    connection: Mutex<Connection>,
    queue: Arc<RuntimeDbWriteQueue>,
}

struct RuntimeDbWriteQueue {
    state: Mutex<RuntimeDbWriteQueueState>,
    available: Condvar,
}

#[derive(Default)]
struct RuntimeDbWriteQueueState {
    next_ticket: u64,
    serving_ticket: u64,
}

struct RuntimeDbWriteTurn {
    queue: Arc<RuntimeDbWriteQueue>,
}

type RuntimeDbWriteJob =
    Box<dyn for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static>;

struct RuntimeDbWriteRequest {
    context: RuntimeDbWriteContext,
    job: RuntimeDbWriteJob,
}

#[derive(Clone, Copy)]
struct RuntimeDbWriteContext {
    operation: &'static str,
    table: &'static str,
    mode: RuntimeDbWriteMode,
}

impl RuntimeDbWriteContext {
    const fn new(operation: &'static str, table: &'static str, mode: RuntimeDbWriteMode) -> Self {
        Self {
            operation,
            table,
            mode,
        }
    }

    const fn sync(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Sync)
    }

    const fn background(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Background)
    }
}

#[derive(Clone, Copy)]
enum RuntimeDbWriteMode {
    Sync,
    Background,
}

impl RuntimeDbWriteMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Background => "background",
        }
    }
}

pub struct WorkItemRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TaskRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct ExternalTriggerRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct WaitConditionRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct QueueEntryRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TimerRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TurnRecordRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct MessageRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TranscriptRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct EvidenceRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct AuditEventSink<'a> {
    db: &'a RuntimeDb,
}

pub struct AgentStateRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct WorkspaceEntryRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct WorkspaceOccupancyRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct AgentIdentityRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct WorkItemDelegationRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct WorkItemContinuationRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct ContextEpisodeRepository<'a> {
    db: &'a RuntimeDb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDomainSnapshot {
    pub domain: String,
    pub schema_version: i64,
    pub import_status: String,
    pub canonical_source: String,
    pub source_checkpoint_json: Option<String>,
    pub imported_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyJsonlPosture {
    Disabled,
    DebugExportOnly,
    AuditMirror,
    LegacyImportOnly,
}

impl LegacyJsonlPosture {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::DebugExportOnly => "debug_export_only",
            Self::AuditMirror => "audit_mirror",
            Self::LegacyImportOnly => "legacy_import_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedStorageDomain {
    pub domain: &'static str,
    pub canonical_source: &'static str,
    pub legacy_jsonl_posture: LegacyJsonlPosture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Message,
    TranscriptEntry,
    ToolExecution,
    ModelRequest,
    ModelResponse,
    Brief,
    DeliverySummary,
    ArtifactMetadata,
}

impl EvidenceKind {
    fn table_name(self) -> &'static str {
        match self {
            Self::Message => "messages",
            Self::TranscriptEntry => "transcript_entries",
            Self::ToolExecution => "tool_executions",
            Self::ModelRequest => "model_requests",
            Self::ModelResponse => "model_responses",
            Self::Brief => "briefs",
            Self::DeliverySummary => "delivery_summaries",
            Self::ArtifactMetadata => "artifact_metadata",
        }
    }
}

static RUNTIME_DB_WRITE_QUEUES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<RuntimeDbWriteQueue>>>> =
    OnceLock::new();

impl RuntimeDbWriter {
    fn open(path: PathBuf, connection: Connection) -> Result<Self> {
        let queue = runtime_db_write_queue(&path)?;
        let state = Arc::new(RuntimeDbWriterState {
            path,
            connection: Mutex::new(connection),
            queue,
        });
        let (append_tx, append_rx) =
            mpsc::sync_channel::<RuntimeDbWriteRequest>(RUNTIME_DB_WRITE_QUEUE_CAPACITY);
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("holon-runtime-db-writer".to_string())
            .spawn(move || {
                while let Ok(request) = append_rx.recv() {
                    let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
                    loop {
                        match thread_state
                            .append_wait_with_context(request.context, |tx| (request.job)(tx))
                        {
                            Ok(()) => break,
                            Err(error) if is_retryable_db_error(&error) => {
                                tracing::warn!(
                                    error = %error,
                                    path = %thread_state.path.display(),
                                    operation = request.context.operation,
                                    table = request.context.table,
                                    mode = request.context.mode.as_str(),
                                    retry_delay_ms = retry_delay.as_millis(),
                                    "runtime db queued write retrying"
                                );
                                thread::sleep(retry_delay);
                                retry_delay = next_runtime_db_retry_delay(
                                    retry_delay,
                                    RUNTIME_DB_APPEND_RETRY_MAX_DELAY,
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    path = %thread_state.path.display(),
                                    operation = request.context.operation,
                                    table = request.context.table,
                                    mode = request.context.mode.as_str(),
                                    "runtime db queued write failed"
                                );
                                break;
                            }
                        }
                    }
                }
            })
            .context("spawning runtime db writer thread")?;
        Ok(Self { state, append_tx })
    }

    fn append(
        &self,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.append_with_context(
            RuntimeDbWriteContext::background("runtime_db.append", "unknown"),
            f,
        )
    }

    fn append_with_context(
        &self,
        context: RuntimeDbWriteContext,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let request = RuntimeDbWriteRequest {
            context,
            job: Box::new(f),
        };
        match self.append_tx.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => bail!("runtime db write queue is full"),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bail!("runtime db write queue is disconnected")
            }
        }
    }

    fn append_wait<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.state.append_wait(f)
    }

    fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.state.append_wait_with_context(context, f)
    }
}

impl RuntimeDbWriterState {
    fn append_wait<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.append_wait_with_context(
            RuntimeDbWriteContext::sync("runtime_db.transaction", "unknown"),
            f,
        )
    }

    fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let queue_wait_started_at = Instant::now();
        let _turn = self.queue.wait_turn()?;
        let queue_wait = queue_wait_started_at.elapsed();
        let mutex_wait_started_at = Instant::now();
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("runtime db writer mutex poisoned"))?;
        let mutex_wait = mutex_wait_started_at.elapsed();
        run_transaction_on_connection(&connection, &self.path, context, queue_wait, mutex_wait, f)
    }
}

impl RuntimeDbWriteQueue {
    fn wait_turn(self: &Arc<Self>) -> Result<RuntimeDbWriteTurn> {
        let ticket = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .context("runtime db write queue ticket overflow")?;
            ticket
        };

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
        while state.serving_ticket != ticket {
            state = self
                .available
                .wait(state)
                .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
        }
        Ok(RuntimeDbWriteTurn {
            queue: Arc::clone(self),
        })
    }
}

impl Drop for RuntimeDbWriteTurn {
    fn drop(&mut self) {
        if let Ok(mut state) = self.queue.state.lock() {
            state.serving_ticket = state.serving_ticket.saturating_add(1);
            self.queue.available.notify_all();
        }
    }
}

fn runtime_db_write_queue(path: &Path) -> Result<Arc<RuntimeDbWriteQueue>> {
    let key = runtime_db_write_queue_key(path);
    let queues = RUNTIME_DB_WRITE_QUEUES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut queues = queues
        .lock()
        .map_err(|_| anyhow!("runtime db write queues mutex poisoned"))?;
    Ok(Arc::clone(queues.entry(key).or_insert_with(|| {
        Arc::new(RuntimeDbWriteQueue {
            state: Mutex::new(RuntimeDbWriteQueueState::default()),
            available: Condvar::new(),
        })
    })))
}

fn runtime_db_write_queue_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => parent
            .canonicalize()
            .map(|parent| parent.join(file_name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct EvidenceQuery<'a> {
    pub agent_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub work_item_id: Option<&'a str>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub message_id: Option<String>,
    pub task_id: Option<String>,
    pub work_item_id: Option<String>,
    pub created_at: String,
    pub preview: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvidencePayloadRow {
    pub payload_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct MessageSearchQuery {
    pub query: String,
    pub agent_ids: Vec<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSearchRow {
    pub evidence_id: String,
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub message_id: String,
    pub task_id: Option<String>,
    pub work_item_id: Option<String>,
    pub created_at: String,
    pub kind: String,
    pub preview: Option<String>,
}

impl WorkItemRepository<'_> {
    pub fn import_legacy(
        &self,
        records: Vec<WorkItemRecord>,
        current_work_item_id: Option<&str>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("work_items", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_items", "jsonl", "db", |tx| {
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
                    upsert_work_item_tx(
                        tx,
                        record,
                        current_work_item_id == Some(record.id.as_str()),
                    )?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
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

impl AgentStateRepository<'_> {
    pub fn import_legacy(&self, record: Option<AgentState>) -> Result<()> {
        if self.db.storage_domain_is_complete("agent_states", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_states", "json", "db", |tx| {
                if let Some(record) = record.as_ref() {
                    upsert_agent_state_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": usize::from(record.is_some()) }))
            })
    }

    pub fn upsert(&self, record: &AgentState) -> Result<()> {
        self.db.transaction(|tx| upsert_agent_state_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentState>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_state_payload(&payload))
            .transpose()
    }
}

impl WorkspaceEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_entries", "jsonl", "db", |tx| {
                let latest = reduce_workspace_entry_records(records);
                for record in latest.values() {
                    upsert_workspace_entry_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_entry_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceEntry>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM workspace_entries
             ORDER BY updated_at DESC, created_at DESC, workspace_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_workspace_entry_payload(&row?))
            .collect()
    }
}

impl WorkspaceOccupancyRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceOccupancyRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_occupancies", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_occupancies", "jsonl", "db", |tx| {
                let latest = reduce_workspace_occupancy_records(records);
                for record in latest.values() {
                    upsert_workspace_occupancy_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceOccupancyRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_occupancy_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM workspace_occupancies
             ORDER BY acquired_at DESC, occupancy_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_workspace_occupancy_payload(&row?))
            .collect()
    }
}

impl AgentIdentityRepository<'_> {
    pub fn import_legacy(&self, records: Vec<AgentIdentityRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("agent_identities", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_identities", "jsonl", "db", |tx| {
                let latest = reduce_agent_identity_records(records);
                for record in latest.values() {
                    upsert_agent_identity_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_agent_identity_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM agent_identities
             ORDER BY updated_at DESC, created_at DESC, agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_agent_identity_payload(&row?))
            .collect()
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_identity_payload(&payload))
            .transpose()
    }
}

impl WorkItemDelegationRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkItemDelegationRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_delegations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_delegations", "jsonl", "db", |tx| {
                let latest = reduce_work_item_delegation_records(records);
                for record in latest.values() {
                    upsert_work_item_delegation_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_delegation_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_delegation_payload(&row?))
            .collect()
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             WHERE parent_agent_id = ?1 OR child_agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest_for_child(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM work_item_delegations
                 WHERE child_agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
                 LIMIT 1",
                [child_agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_delegation_payload(&payload))
            .transpose()
    }
}

impl WorkItemContinuationRepository<'_> {
    pub fn import_empty(&self) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_continuations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_continuations", "new-domain", "db", |_tx| {
                Ok(serde_json::json!({ "imported_records": 0 }))
            })
    }

    pub fn upsert(&self, record: &WorkItemContinuationFrame) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_continuation_tx(tx, record))
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WorkItemContinuationFrame>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1 AND state = 'active'
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_continuation_payload(&row?))
            .collect()
    }
}

impl ContextEpisodeRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ContextEpisodeRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete(CONTEXT_EPISODE_ANCHORS_DOMAIN, "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import(CONTEXT_EPISODE_ANCHORS_DOMAIN, "jsonl", "db", |tx| {
                let latest = reduce_context_episode_records(records);
                for record in latest.values() {
                    upsert_context_episode_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ContextEpisodeRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_context_episode_tx(tx, record))
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             WHERE agent_id = ?1
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ExternalTriggerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ExternalTriggerRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("external_triggers", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("external_triggers", "jsonl", "db", |tx| {
                let latest = reduce_external_trigger_records(records);
                for record in latest.values() {
                    upsert_external_trigger_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ExternalTriggerRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_external_trigger_tx(tx, record))
    }

    pub fn latest(&self, external_trigger_id: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM external_triggers WHERE external_trigger_id = ?1",
                [external_trigger_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn latest_for_agent_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn active_default_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE target_agent_id = ?1 AND status = 'active'
                 ORDER BY created_at DESC, external_trigger_id ASC
                 LIMIT 1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn active_by_token_hash(&self, token_hash: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE token_hash = ?1 AND status = 'active'
                 LIMIT 1",
                [token_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }
}

impl TaskRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TaskRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("tasks", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("tasks", "jsonl", "db", |tx| {
                let latest = reduce_task_records(records);
                for record in latest.values() {
                    upsert_task_tx(tx, record)?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| is_active_task_status(&record.status))
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
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

impl WaitConditionRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WaitConditionRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("wait_conditions", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("wait_conditions", "jsonl", "db", |tx| {
                let latest = reduce_wait_condition_records(records);
                for record in latest.values() {
                    upsert_wait_condition_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WaitConditionRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_wait_condition_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM wait_conditions
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_wait_condition_payload(&row?))
            .collect()
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM wait_conditions
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_wait_condition_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM wait_conditions
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_wait_condition_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM wait_conditions
             WHERE agent_id = ?1 AND status = 'active'
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_wait_condition_payload(&row?))
            .collect()
    }
}

impl QueueEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<QueueEntryRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("queue_entries", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("queue_entries", "jsonl", "db", |tx| {
                let imported_records = records.len();
                for record in records {
                    upsert_queue_entry_tx(tx, &record)?;
                }
                Ok(serde_json::json!({ "imported_records": imported_records }))
            })
    }

    pub fn upsert(&self, record: &QueueEntryRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_queue_entry_tx(tx, record))
    }

    pub fn try_claim_queued_message(&self, record: &QueueEntryRecord) -> Result<bool> {
        self.db
            .transaction(|tx| try_claim_queued_message_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM queue_entries
             ORDER BY updated_at DESC, created_at DESC, message_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_queue_entry_payload(&row?)).collect()
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 WHERE agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }
}

impl TimerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TimerRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("timers", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("timers", "jsonl", "db", |tx| {
                let latest = reduce_timer_records(records);
                for record in latest.values() {
                    upsert_timer_tx(tx, record)?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| record.status == TimerStatus::Active)
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TimerRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_timer_tx(tx, record))
    }

    pub fn latest(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM timers WHERE timer_id = ?1",
                [timer_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_timer_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TimerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_timer_payload(&row?)).collect()
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TurnRecordRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
        wait_conditions: Vec<WaitConditionRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("turn_records", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("turn_records", "jsonl-derived", "db", |tx| {
                let records = derive_turn_records_from_legacy_evidence(
                    messages,
                    tool_executions,
                    briefs,
                    delivery_summaries,
                    wait_conditions,
                )?;
                for record in &records {
                    upsert_turn_record_tx(tx, record)?;
                }
                Ok(serde_json::json!({
                    "imported_records": records.len(),
                    "source": "legacy evidence jsonl",
                    "ignored": "turns.jsonl"
                }))
            })
    }

    pub fn upsert(&self, record: &TurnRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_turn_record_tx(tx, record))
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM turn_records
             WHERE agent_id = ?1
             ORDER BY turn_index DESC, created_at DESC, turn_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records = rows
            .map(|row| decode_turn_record_payload(&row?))
            .collect::<Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM turn_records
             ORDER BY turn_index DESC, created_at DESC, turn_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records = rows
            .map(|row| decode_turn_record_payload(&row?))
            .collect::<Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }
}

impl MessageRepository<'_> {
    pub fn import_legacy(&self, messages: Vec<serde_json::Value>) -> Result<()> {
        if self.db.storage_domain_is_complete("messages", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("messages", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            upsert_message_tx(tx, &message)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                }))
            })
    }

    pub fn upsert(&self, message: &MessageEnvelope) -> Result<()> {
        self.db.transaction(|tx| upsert_message_tx(tx, message))
    }

    pub fn upsert_many(&self, messages: &[MessageEnvelope]) -> Result<()> {
        self.db.transaction(|tx| {
            for message in messages {
                upsert_message_tx(tx, message)?;
            }
            Ok(())
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(message_seq, 9223372036854775807) DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY COALESCE(message_seq, 9223372036854775807) DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    pub fn from(
        &self,
        agent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(message_seq, 9223372036854775807) ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY COALESCE(message_seq, 9223372036854775807) ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?1",
            )?;
            let rows = statement.query_map([offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        if records.len() > limit {
            records.drain(0..(records.len() - limit));
        }
        Ok(records)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<MessageEnvelope>> {
        let connection = self.db.connection()?;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(message_seq, 9223372036854775807) ASC, created_at ASC, message_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?)).collect()
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY COALESCE(message_seq, 9223372036854775807) ASC, created_at ASC, message_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?)).collect()
        }
    }

    pub fn all_values(&self, agent_id: Option<&str>) -> Result<Vec<serde_json::Value>> {
        self.all(agent_id)?
            .into_iter()
            .map(|message| serde_json::to_value(message).map_err(Into::into))
            .collect()
    }

    pub fn search(&self, query: MessageSearchQuery) -> Result<Vec<MessageSearchRow>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let Some(search_terms) = message_search_match_query(&query.query) else {
            return Ok(Vec::new());
        };

        let mut clauses = vec!["message_search_index MATCH ?".to_string()];
        let mut values = Vec::<String>::new();
        values.push(search_terms);
        if !query.agent_ids.is_empty() {
            let placeholders = std::iter::repeat("?")
                .take(query.agent_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("messages.agent_id IN ({placeholders})"));
            values.extend(query.agent_ids);
        }

        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT messages.evidence_id,
                    messages.agent_id,
                    messages.turn_id,
                    messages.message_id,
                    messages.task_id,
                    messages.work_item_id,
                    messages.created_at,
                    messages.kind,
                    messages.preview
             FROM message_search_index
             JOIN messages ON messages.evidence_id = message_search_index.evidence_id
             WHERE {}
             ORDER BY bm25(message_search_index), messages.created_at DESC, messages.evidence_id ASC
             LIMIT {}",
            clauses.join(" AND "),
            limit
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let params = values
            .iter()
            .map(|value| value as &dyn ToSql)
            .collect::<Vec<_>>();
        let rows = statement.query_map(params.as_slice(), |row| {
            Ok(MessageSearchRow {
                evidence_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                message_id: row.get(3)?,
                task_id: row.get(4)?,
                work_item_id: row.get(5)?,
                created_at: row.get(6)?,
                kind: row.get(7)?,
                preview: row.get(8)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn by_id(
        &self,
        agent_id: Option<&str>,
        message_id: &str,
    ) -> Result<Option<MessageEnvelope>> {
        let connection = self.db.connection()?;
        let payload = if let Some(agent_id) = agent_id {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM messages
                     WHERE agent_id = ?1 AND message_id = ?2
                     LIMIT 1",
                    params![agent_id, message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM messages
                     WHERE message_id = ?1
                     LIMIT 1",
                    [message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        };
        payload
            .map(|payload| decode_message_payload(&payload))
            .transpose()
    }

    pub fn count(&self, agent_id: Option<&str>) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT COUNT(*) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?
        };
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn max_message_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(message_seq) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT MAX(message_seq) FROM messages", [], |row| {
                row.get(0)
            })?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl TranscriptRepository<'_> {
    pub fn import_legacy(&self, entries: Vec<TranscriptEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("transcript_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("transcript_entries", "jsonl", "db", |tx| {
                for entry in &entries {
                    upsert_transcript_entry_tx(tx, entry)?;
                }
                Ok(serde_json::json!({
                    "imported_transcript_entries": entries.len(),
                }))
            })
    }

    pub fn upsert(&self, entry: &TranscriptEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_transcript_entry_tx(tx, entry))
    }

    pub fn upsert_many(&self, entries: &[TranscriptEntry]) -> Result<()> {
        self.db.transaction(|tx| {
            for entry in entries {
                upsert_transcript_entry_tx(tx, entry)?;
            }
            Ok(())
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<TranscriptEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<TranscriptEntry>> {
        let connection = self.db.connection()?;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        }
    }

    pub fn by_id(&self, agent_id: Option<&str>, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        let connection = self.db.connection()?;
        let payload: Option<String> = if let Some(agent_id) = agent_id {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM transcript_entries
                     WHERE agent_id = ?1 AND evidence_id = ?2
                     LIMIT 1",
                    params![agent_id, entry_id],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM transcript_entries
                     WHERE evidence_id = ?1
                     LIMIT 1",
                    [entry_id],
                    |row| row.get(0),
                )
                .optional()?
        };
        payload
            .map(|payload| decode_transcript_entry_payload(&payload))
            .transpose()
    }

    pub fn max_transcript_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries",
                [],
                |row| row.get(0),
            )?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl EvidenceRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        transcript_entries: Vec<TranscriptEntry>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("evidence", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("evidence", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            insert_message_evidence_tx(tx, &message)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                for entry in &transcript_entries {
                    insert_transcript_evidence_tx(tx, entry)?;
                }
                for record in &tool_executions {
                    insert_tool_evidence_tx(tx, record)?;
                }
                for brief in &briefs {
                    insert_brief_evidence_tx(tx, brief)?;
                }
                for summary in &delivery_summaries {
                    insert_delivery_summary_evidence_tx(tx, summary)?;
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                    "imported_transcript_entries": transcript_entries.len(),
                    "imported_tool_executions": tool_executions.len(),
                    "imported_briefs": briefs.len(),
                    "imported_delivery_summaries": delivery_summaries.len(),
                }))
            })
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.db
            .transaction(|tx| insert_message_evidence_tx(tx, message))
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        self.db
            .transaction(|tx| insert_transcript_evidence_tx(tx, entry))
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_tool_evidence_tx(tx, record))
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_brief_evidence_tx(tx, brief))
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_delivery_summary_evidence_tx(tx, record))
    }

    pub fn query(&self, kind: EvidenceKind, query: EvidenceQuery<'_>) -> Result<Vec<EvidenceRow>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let mut clauses = Vec::new();
        let mut params = Vec::<String>::new();
        push_optional_clause(&mut clauses, &mut params, "agent_id", query.agent_id);
        push_optional_clause(&mut clauses, &mut params, "turn_id", query.turn_id);
        push_optional_clause(&mut clauses, &mut params, "message_id", query.message_id);
        push_optional_clause(&mut clauses, &mut params, "task_id", query.task_id);
        push_optional_clause(
            &mut clauses,
            &mut params,
            "work_item_id",
            query.work_item_id,
        );
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT evidence_id, agent_id, turn_id, message_id, task_id, work_item_id, created_at, preview
             FROM {}{}
             ORDER BY created_at DESC, evidence_id ASC
             LIMIT {}",
            kind.table_name(),
            where_clause,
            limit
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(EvidenceRow {
                evidence_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                message_id: row.get(3)?,
                task_id: row.get(4)?,
                work_item_id: row.get(5)?,
                created_at: row.get(6)?,
                preview: row.get(7)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn recent_payloads(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidencePayloadRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1
             ORDER BY created_at DESC, evidence_id DESC
             LIMIT ?2",
            kind.table_name()
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![agent_id, limit], |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })?;
        let mut records: Vec<_> = rows
            .map(|row| row.map_err(Into::into))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn payload_by_id(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        evidence_id: &str,
    ) -> Result<Option<EvidencePayloadRow>> {
        let sql = format!(
            "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1 AND evidence_id = ?2
             LIMIT 1",
            kind.table_name()
        );
        let connection = self.db.connection()?;
        connection
            .query_row(&sql, params![agent_id, evidence_id], |row| {
                Ok(EvidencePayloadRow {
                    payload_json: row.get(0)?,
                })
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn recent_briefs(&self, agent_id: &str, limit: usize) -> Result<Vec<BriefRecord>> {
        self.recent_payloads(EvidenceKind::Brief, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn brief_by_id(&self, agent_id: &str, brief_id: &str) -> Result<Option<BriefRecord>> {
        self.payload_by_id(EvidenceKind::Brief, agent_id, brief_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn recent_tool_executions(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ToolExecutionRecord>> {
        self.recent_payloads(EvidenceKind::ToolExecution, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn tool_execution_by_id(
        &self,
        agent_id: &str,
        tool_id: &str,
    ) -> Result<Option<ToolExecutionRecord>> {
        self.payload_by_id(EvidenceKind::ToolExecution, agent_id, tool_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn recent_delivery_summaries(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        self.recent_payloads(EvidenceKind::DeliverySummary, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn latest_delivery_summary(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Option<DeliverySummaryRecord>> {
        let connection = self.db.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload_json
                 FROM delivery_summaries
                 WHERE agent_id = ?1 AND work_item_id = ?2
                 ORDER BY created_at DESC, evidence_id DESC
                 LIMIT 1",
                params![agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .transpose()
    }

    pub fn count_briefs(&self, agent_id: &str) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM briefs WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }
}

impl AuditEventSink<'_> {
    pub fn append(&self, agent_id: Option<&str>, event: &AuditEvent) -> Result<()> {
        let agent_id = agent_id.map(str::to_string);
        let event = event.clone();
        self.db.append_with_context(
            RuntimeDbWriteContext::background("audit_events.append", "audit_events"),
            move |tx| insert_audit_event_tx(tx, agent_id.as_deref(), &event),
        )
    }

    pub fn append_many(&self, agent_id: Option<&str>, events: &[AuditEvent]) -> Result<()> {
        self.db.transaction_with_context(
            RuntimeDbWriteContext::sync("audit_events.append_many", "audit_events"),
            |tx| {
                for event in events {
                    insert_audit_event_tx(tx, agent_id, event)?;
                }
                Ok(())
            },
        )
    }

    pub fn import_legacy(&self, agent_id: Option<&str>, events: Vec<AuditEvent>) -> Result<()> {
        if self.db.storage_domain_is_complete("audit_events", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("audit_events", "jsonl", "db", |tx| {
                for event in &events {
                    insert_audit_event_tx(tx, agent_id, event)?;
                }
                Ok(serde_json::json!({ "imported_records": events.len() }))
            })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events");
        if agent_id.is_some() {
            sql.push_str(" WHERE agent_id = ?1");
        }
        sql.push_str(" ORDER BY event_seq DESC, created_at DESC LIMIT ?");
        let mut statement = connection.prepare(&sql)?;
        let mut events = if let Some(agent_id) = agent_id {
            statement
                .query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            statement
                .query_map(params![limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        };
        events.reverse();
        Ok(events)
    }

    pub fn latest_event_seq(&self, agent_id: Option<&str>) -> Result<Option<u64>> {
        let connection = self.db.connection()?;
        let value = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(event_seq) FROM audit_events WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, Option<i64>>(0),
            )?
        } else {
            connection.query_row("SELECT MAX(event_seq) FROM audit_events", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
        };
        value
            .map(|seq| u64::try_from(seq).context("stored audit event sequence is negative"))
            .transpose()
    }

    pub fn max_event_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        Ok(self.latest_event_seq(agent_id)?.unwrap_or(0))
    }

    pub fn range(
        &self,
        agent_id: Option<&str>,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let lower = i64::try_from(after_seq.unwrap_or(0))
            .context("audit event lower cursor exceeds SQLite integer range")?;
        let upper = before_seq
            .map(|seq| {
                i64::try_from(seq).context("audit event upper cursor exceeds SQLite integer range")
            })
            .transpose()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if upper.is_some() {
            sql.push_str(" AND event_seq < ?2");
        }
        if agent_id.is_some() {
            let param_index = if upper.is_some() { 3 } else { 2 };
            sql.push_str(&format!(" AND agent_id = ?{param_index}"));
        }
        if descending {
            sql.push_str(" ORDER BY event_seq DESC, created_at DESC");
        } else {
            sql.push_str(" ORDER BY event_seq ASC, created_at ASC");
        }
        let limit_param_index = 2 + usize::from(upper.is_some()) + usize::from(agent_id.is_some());
        sql.push_str(&format!(" LIMIT ?{limit_param_index}"));
        let mut statement = connection.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(lower)];
        if let Some(upper) = upper {
            params.push(Box::new(upper));
        }
        if let Some(agent_id) = agent_id {
            params.push(Box::new(agent_id.to_owned()));
        }
        params.push(Box::new(limit));
        let events = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect();
        events
    }

    pub fn page_after(
        &self,
        agent_id: Option<&str>,
        after_event_seq: u64,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if agent_id.is_some() {
            sql.push_str(" AND agent_id = ?2");
        }
        let limit_param_index = if agent_id.is_some() { 3 } else { 2 };
        sql.push_str(&format!(
            " ORDER BY event_seq ASC, created_at ASC LIMIT ?{limit_param_index}"
        ));
        let mut statement = connection.prepare(&sql)?;
        if let Some(agent_id) = agent_id {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, agent_id, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        } else {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        }
    }
}

fn read_storage_domain_connection(
    connection: &Connection,
    domain: &str,
) -> Result<Option<StorageDomainSnapshot>> {
    connection
        .query_row(
            "SELECT domain, schema_version, import_status, canonical_source,
                    source_checkpoint_json, imported_at, updated_at
             FROM storage_domains WHERE domain = ?1",
            [domain],
            |row| {
                Ok(StorageDomainSnapshot {
                    domain: row.get(0)?,
                    schema_version: row.get(1)?,
                    import_status: row.get(2)?,
                    canonical_source: row.get(3)?,
                    source_checkpoint_json: row.get(4)?,
                    imported_at: row.get(5)?,
                    updated_at: row.get(6)?,
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
    upsert_storage_domain_checkpoint_json(
        tx,
        domain,
        import_status,
        canonical_source,
        checkpoint.map(|value| value.to_string()),
    )
}

fn upsert_storage_domain_checkpoint_json(
    tx: &Transaction<'_>,
    domain: &str,
    import_status: &str,
    canonical_source: &str,
    checkpoint_json: Option<String>,
) -> Result<()> {
    let now = timestamp(Utc::now());
    let imported_at = (import_status == "complete").then(|| now.clone());
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
            checkpoint_json,
            imported_at,
            now
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
struct EvidenceInsert<'a> {
    table: &'static str,
    evidence_id: &'a str,
    agent_id: &'a str,
    turn_id: Option<&'a str>,
    message_id: Option<&'a str>,
    task_id: Option<&'a str>,
    work_item_id: Option<&'a str>,
    created_at: DateTime<Utc>,
    kind: String,
    preview: Option<String>,
    payload_json: String,
}

fn insert_evidence_tx(tx: &Transaction<'_>, evidence: EvidenceInsert<'_>) -> Result<()> {
    let content_hash = content_hash(&evidence.payload_json);
    let sql = format!(
        "INSERT INTO {} (
            evidence_id, agent_id, turn_id, message_id, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        evidence.table
    );
    tx.execute(
        &sql,
        params![
            evidence.evidence_id,
            evidence.agent_id,
            evidence.turn_id,
            evidence.message_id,
            evidence.task_id,
            evidence.work_item_id,
            timestamp(evidence.created_at),
            evidence.kind,
            Option::<String>::None,
            content_hash,
            evidence.preview,
            evidence.payload_json,
        ],
    )?;
    Ok(())
}

fn insert_message_evidence_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    upsert_message_tx(tx, message)
}

fn insert_transcript_evidence_tx(tx: &Transaction<'_>, entry: &TranscriptEntry) -> Result<()> {
    upsert_transcript_entry_tx(tx, entry)
}

fn upsert_agent_state_tx(tx: &Transaction<'_>, record: &AgentState) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let now = timestamp(Utc::now());
    tx.execute(
        "INSERT INTO agent_states (
            agent_id, status, turn_index, current_run_id, current_work_item_id,
            active_workspace_id, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(agent_id) DO UPDATE SET
            status = excluded.status,
            turn_index = excluded.turn_index,
            current_run_id = excluded.current_run_id,
            current_work_item_id = excluded.current_work_item_id,
            active_workspace_id = excluded.active_workspace_id,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.turn_index >= agent_states.turn_index",
        params![
            record.id,
            status,
            record.turn_index as i64,
            record.current_run_id,
            record.current_work_item_id,
            record
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.as_str()),
            now,
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_workspace_entry_tx(tx: &Transaction<'_>, record: &WorkspaceEntry) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO workspace_entries (
            workspace_id, workspace_alias, workspace_kind, owner_agent_id,
            workspace_anchor, repo_name, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(workspace_id) DO UPDATE SET
            workspace_alias = excluded.workspace_alias,
            workspace_kind = excluded.workspace_kind,
            owner_agent_id = excluded.owner_agent_id,
            workspace_anchor = excluded.workspace_anchor,
            repo_name = excluded.repo_name,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= workspace_entries.updated_at",
        params![
            record.workspace_id,
            record.workspace_alias,
            record.workspace_kind,
            record.owner_agent_id,
            record.workspace_anchor.display().to_string(),
            record.repo_name,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_workspace_occupancy_tx(
    tx: &Transaction<'_>,
    record: &WorkspaceOccupancyRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let access_mode = enum_string(&record.access_mode)?;
    tx.execute(
        "INSERT INTO workspace_occupancies (
            occupancy_id, execution_root_id, workspace_id, holder_agent_id,
            access_mode, acquired_at, released_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(occupancy_id) DO UPDATE SET
            execution_root_id = excluded.execution_root_id,
            workspace_id = excluded.workspace_id,
            holder_agent_id = excluded.holder_agent_id,
            access_mode = excluded.access_mode,
            acquired_at = excluded.acquired_at,
            released_at = excluded.released_at,
            payload_json = excluded.payload_json
         WHERE COALESCE(excluded.released_at, '') >= COALESCE(workspace_occupancies.released_at, '')",
        params![
            record.occupancy_id,
            record.execution_root_id,
            record.workspace_id,
            record.holder_agent_id,
            access_mode,
            timestamp(record.acquired_at),
            record.released_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_agent_identity_tx(tx: &Transaction<'_>, record: &AgentIdentityRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let kind = enum_string(&record.kind)?;
    let visibility = enum_string(&record.visibility)?;
    let ownership = record.ownership.as_ref().map(enum_string).transpose()?;
    let profile_preset = record
        .profile_preset
        .as_ref()
        .map(enum_string)
        .transpose()?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO agent_identities (
            agent_id, kind, visibility, ownership, profile_preset, status,
            parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
            created_at, updated_at, archived_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(agent_id) DO UPDATE SET
            kind = excluded.kind,
            visibility = excluded.visibility,
            ownership = excluded.ownership,
            profile_preset = excluded.profile_preset,
            status = excluded.status,
            parent_agent_id = excluded.parent_agent_id,
            lineage_parent_agent_id = excluded.lineage_parent_agent_id,
            delegated_from_task_id = excluded.delegated_from_task_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= agent_identities.updated_at",
        params![
            record.agent_id,
            kind,
            visibility,
            ownership,
            profile_preset,
            status,
            record.parent_agent_id,
            record.lineage_parent_agent_id,
            record.delegated_from_task_id,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.archived_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_message_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    let payload_json = serde_json::to_string(message)?;
    let content_hash = content_hash(&payload_json);
    let kind = enum_string(&message.kind)?;
    let preview = evidence_preview(&message.body);
    tx.execute(
        "INSERT INTO messages (
            evidence_id, agent_id, turn_id, message_id, message_seq, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            message_seq = excluded.message_seq,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        params![
            message.id,
            message.agent_id,
            message.turn_id,
            message.id,
            message.message_seq.map(|seq| seq as i64),
            message.task_id,
            message.work_item_id,
            timestamp(message.created_at),
            kind,
            Option::<String>::None,
            content_hash,
            preview,
            payload_json,
        ],
    )?;
    upsert_message_search_index_tx(tx, message, &kind, &payload_json)?;
    Ok(())
}

fn upsert_message_search_index_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
    kind: &str,
    payload_json: &str,
) -> Result<()> {
    tx.execute(
        "DELETE FROM message_search_index WHERE evidence_id = ?1",
        params![message.id],
    )?;
    tx.execute(
        "INSERT INTO message_search_index (
            evidence_id, agent_id, turn_id, message_id, task_id, work_item_id,
            kind, body_text, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            message.id,
            message.agent_id,
            message.turn_id,
            message.id,
            message.task_id,
            message.work_item_id,
            kind,
            message_body_search_text(&message.body),
            payload_json,
        ],
    )?;
    Ok(())
}

fn message_body_search_text(body: &crate::types::MessageBody) -> String {
    match body {
        crate::types::MessageBody::Text { text } => text.clone(),
        crate::types::MessageBody::Json { value } => value.to_string(),
        crate::types::MessageBody::Brief {
            title,
            text,
            attachments,
        } => {
            let mut fields = Vec::new();
            if let Some(title) = title {
                fields.push(title.clone());
            }
            fields.push(text.clone());
            if let Some(attachments) = attachments {
                fields.push(serde_json::to_string(attachments).unwrap_or_default());
            }
            fields.join("\n")
        }
    }
}

fn message_search_match_query(query: &str) -> Option<String> {
    let terms = query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

fn upsert_transcript_entry_tx(tx: &Transaction<'_>, entry: &TranscriptEntry) -> Result<()> {
    let turn_id = entry
        .data
        .get("turn_id")
        .and_then(serde_json::Value::as_str);
    let task_id = entry
        .data
        .get("task_id")
        .and_then(serde_json::Value::as_str);
    let work_item_id = entry
        .data
        .get("work_item_id")
        .and_then(serde_json::Value::as_str);
    let payload_json = serde_json::to_string(entry)?;
    let content_hash = content_hash(&payload_json);
    let kind = enum_string(&entry.kind)?;
    tx.execute(
        "INSERT INTO transcript_entries (
            evidence_id, agent_id, turn_id, message_id, transcript_seq, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            transcript_seq = excluded.transcript_seq,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        params![
            entry.id,
            entry.agent_id,
            turn_id,
            entry.related_message_id,
            entry.transcript_seq.map(|seq| seq as i64),
            task_id,
            work_item_id,
            timestamp(entry.created_at),
            kind,
            Option::<String>::None,
            content_hash,
            evidence_preview(&entry.data),
            payload_json,
        ],
    )?;
    Ok(())
}

fn insert_tool_evidence_tx(tx: &Transaction<'_>, record: &ToolExecutionRecord) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::ToolExecution.table_name(),
            evidence_id: &record.id,
            agent_id: &record.agent_id,
            turn_id: record.turn_id.as_deref(),
            message_id: None,
            task_id: None,
            work_item_id: record.work_item_id.as_deref(),
            created_at: record.created_at,
            kind: record.tool_name.clone(),
            preview: Some(truncate_evidence_string(&record.summary)),
            payload_json: full_payload_json(record)?,
        },
    )
}

fn insert_brief_evidence_tx(tx: &Transaction<'_>, brief: &BriefRecord) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::Brief.table_name(),
            evidence_id: &brief.id,
            agent_id: &brief.agent_id,
            turn_id: brief.turn_id.as_deref(),
            message_id: brief.related_message_id.as_deref(),
            task_id: brief.related_task_id.as_deref(),
            work_item_id: brief.work_item_id.as_deref(),
            created_at: brief.created_at,
            kind: enum_string(&brief.kind)?,
            preview: Some(truncate_evidence_string(&brief.text)),
            payload_json: full_payload_json(brief)?,
        },
    )
}

fn insert_delivery_summary_evidence_tx(
    tx: &Transaction<'_>,
    record: &DeliverySummaryRecord,
) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::DeliverySummary.table_name(),
            evidence_id: &record.id,
            agent_id: &record.agent_id,
            turn_id: record.turn_id.as_deref(),
            message_id: None,
            task_id: None,
            work_item_id: Some(&record.work_item_id),
            created_at: record.created_at,
            kind: "delivery_summary".to_string(),
            preview: Some(truncate_evidence_string(&record.text)),
            payload_json: full_payload_json(record)?,
        },
    )
}

fn insert_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    tx.execute(
        "INSERT INTO audit_events (
            audit_event_id, event_seq, agent_id, kind, created_at, data_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(audit_event_id) DO NOTHING",
        params![
            event.id,
            event_seq,
            agent_id,
            event.kind,
            timestamp(event.created_at),
            serde_json::to_string(event)?,
        ],
    )?;
    Ok(())
}

fn normalize_legacy_message_value(
    mut raw_message: serde_json::Value,
) -> Result<Option<MessageEnvelope>> {
    let Some(object) = raw_message.as_object_mut() else {
        return Ok(None);
    };
    let has_turn_id = object
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_turn_id {
        let Some(turn_index) = object
            .get("turn_index")
            .or_else(|| object.get("message_seq"))
            .and_then(serde_json::Value::as_u64)
        else {
            return Ok(None);
        };
        object.insert(
            "turn_id".to_string(),
            serde_json::Value::String(format!("legacy-turn-{turn_index}")),
        );
    }
    serde_json::from_value(raw_message)
        .map(Some)
        .map_err(Into::into)
}

fn push_optional_clause(
    clauses: &mut Vec<String>,
    params: &mut Vec<String>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        params.push(value.to_string());
        clauses.push(format!("{column} = ?{}", params.len()));
    }
}

fn full_payload_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}

fn content_hash(payload_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn evidence_preview(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|value| truncate_evidence_string(&value))
}

fn truncate_evidence_string(value: &str) -> String {
    let mut truncated = value.to_string();
    if truncated.len() > EVIDENCE_PREVIEW_LIMIT {
        truncate_string_in_place(&mut truncated, EVIDENCE_PREVIEW_LIMIT);
    }
    truncated
}

fn truncate_string_in_place(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    value.truncate(boundary);
}

fn upsert_work_item_delegation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemDelegationRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    tx.execute(
        "INSERT INTO work_item_delegations (
            delegation_id, parent_agent_id, parent_work_item_id, child_agent_id,
            child_work_item_id, state, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(delegation_id) DO UPDATE SET
            parent_agent_id = excluded.parent_agent_id,
            parent_work_item_id = excluded.parent_work_item_id,
            child_agent_id = excluded.child_agent_id,
            child_work_item_id = excluded.child_work_item_id,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_delegations.updated_at",
        params![
            record.delegation_id,
            record.parent_agent_id,
            record.parent_work_item_id,
            record.child_agent_id,
            record.child_work_item_id,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_work_item_continuation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemContinuationFrame,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let return_policy = enum_string(&record.return_policy)?;
    let state = enum_string(&record.state)?;
    tx.execute(
        "INSERT INTO work_item_continuations (
            continuation_id, agent_id, suspended_work_item_id, active_work_item_id,
            return_policy, state, created_at, updated_at, resolved_at, cancelled_at,
            resolution_reason, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(continuation_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            suspended_work_item_id = excluded.suspended_work_item_id,
            active_work_item_id = excluded.active_work_item_id,
            return_policy = excluded.return_policy,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            resolution_reason = excluded.resolution_reason,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_continuations.updated_at",
        params![
            record.id,
            record.agent_id,
            record.suspended_work_item_id,
            record.active_work_item_id,
            return_policy,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.resolution_reason.as_deref(),
            record.turn_id.as_deref(),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_context_episode_tx(tx: &Transaction<'_>, record: &ContextEpisodeRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let boundary_reason = enum_string(&record.boundary_reason)?;
    tx.execute(
        "INSERT INTO context_episode_anchors (
            episode_id, agent_id, workspace_id, work_item_id, boundary_reason,
            start_turn_index, end_turn_index, started_at, ended_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(episode_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            workspace_id = excluded.workspace_id,
            work_item_id = excluded.work_item_id,
            boundary_reason = excluded.boundary_reason,
            start_turn_index = excluded.start_turn_index,
            end_turn_index = excluded.end_turn_index,
            started_at = excluded.started_at,
            ended_at = excluded.ended_at,
            payload_json = excluded.payload_json
         WHERE excluded.ended_at >= context_episode_anchors.ended_at",
        params![
            record.id,
            record.agent_id,
            record.workspace_id,
            record.current_work_item_id,
            boundary_reason,
            record.start_turn_index as i64,
            record.end_turn_index as i64,
            timestamp(record.created_at),
            timestamp(record.finalized_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_external_trigger_tx(tx: &Transaction<'_>, record: &ExternalTriggerRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let revoked_at = record.revoked_at.map(timestamp);
    let last_delivered_at = record.last_delivered_at.map(timestamp);
    tx.execute(
        "INSERT INTO external_triggers (
            external_trigger_id, target_agent_id, waiting_intent_id, trigger_url,
            token_hash, status, created_at, revoked_at, last_delivered_at,
            delivery_count, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(external_trigger_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            waiting_intent_id = excluded.waiting_intent_id,
            trigger_url = excluded.trigger_url,
            token_hash = excluded.token_hash,
            status = excluded.status,
            created_at = excluded.created_at,
            revoked_at = excluded.revoked_at,
            last_delivered_at = excluded.last_delivered_at,
            delivery_count = excluded.delivery_count,
            payload_json = excluded.payload_json
         WHERE excluded.delivery_count > external_triggers.delivery_count
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') > COALESCE(external_triggers.last_delivered_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') > COALESCE(external_triggers.revoked_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') = COALESCE(external_triggers.revoked_at, '')
                AND excluded.created_at >= external_triggers.created_at
            )",
        params![
            record.external_trigger_id,
            record.target_agent_id,
            record.waiting_intent_id,
            record.trigger_url,
            record.token_hash,
            status,
            timestamp(record.created_at),
            revoked_at,
            last_delivered_at,
            record.delivery_count as i64,
            payload_json,
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
    let status_phase = i64::from(task_status_phase(&record.status));
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
         WHERE excluded.revision > tasks.revision
            OR (excluded.revision = tasks.revision AND ?20 >= CASE tasks.status
                WHEN 'queued' THEN 0
                WHEN 'running' THEN 1
                WHEN 'cancelling' THEN 2
                ELSE 3
            END)",
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
            status_phase,
        ],
    )?;
    Ok(())
}

fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

fn upsert_wait_condition_tx(tx: &Transaction<'_>, record: &WaitConditionRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let kind = enum_string(&record.kind)?;
    tx.execute(
        "INSERT INTO wait_conditions (
            wait_condition_id, agent_id, work_item_id, status, kind, source,
            subject_ref, waiting_for, created_at, updated_at, expires_at,
            resolved_at, cancelled_at, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(wait_condition_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            work_item_id = excluded.work_item_id,
            status = excluded.status,
            kind = excluded.kind,
            source = excluded.source,
            subject_ref = excluded.subject_ref,
            waiting_for = excluded.waiting_for,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            expires_at = excluded.expires_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= wait_conditions.updated_at",
        params![
            record.id,
            record.agent_id,
            record.work_item_id,
            status,
            kind,
            record.source,
            record.subject_ref,
            record.waiting_for,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.expires_at.map(timestamp),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.turn_id,
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_queue_entry_tx(tx: &Transaction<'_>, record: &QueueEntryRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let priority = enum_string(&record.priority)?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO queue_entries (
            message_id, agent_id, priority, status, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(message_id, status) DO UPDATE SET
            agent_id = excluded.agent_id,
            priority = excluded.priority,
            status = excluded.status,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= queue_entries.updated_at",
        params![
            record.message_id,
            record.agent_id,
            priority,
            status,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn try_claim_queued_message_tx(tx: &Transaction<'_>, record: &QueueEntryRecord) -> Result<bool> {
    let latest_status = tx
        .query_row(
            "SELECT status
             FROM queue_entries
             WHERE message_id = ?1 AND agent_id = ?2
             ORDER BY updated_at DESC, created_at DESC
             LIMIT 1",
            params![record.message_id, record.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let queued_status = enum_string(&QueueEntryStatus::Queued)?;
    if latest_status.as_deref() != Some(queued_status.as_str()) {
        return Ok(false);
    }

    let mut claimed = record.clone();
    claimed.status = QueueEntryStatus::Dequeued;
    let payload_json = serde_json::to_string(&claimed)?;
    let priority = enum_string(&claimed.priority)?;
    let status = enum_string(&claimed.status)?;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO queue_entries (
            message_id, agent_id, priority, status, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            claimed.message_id,
            claimed.agent_id,
            priority,
            status,
            timestamp(claimed.created_at),
            timestamp(claimed.updated_at),
            payload_json,
        ],
    )?;
    Ok(changed == 1)
}

fn upsert_timer_tx(tx: &Transaction<'_>, record: &TimerRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let updated_at = timer_updated_at(record);
    tx.execute(
        "INSERT INTO timers (
            timer_id, agent_id, status, summary, created_at, duration_ms,
            interval_ms, repeat, next_fire_at, last_fired_at, fire_count,
            updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(timer_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            status = excluded.status,
            summary = excluded.summary,
            created_at = excluded.created_at,
            duration_ms = excluded.duration_ms,
            interval_ms = excluded.interval_ms,
            repeat = excluded.repeat,
            next_fire_at = excluded.next_fire_at,
            last_fired_at = excluded.last_fired_at,
            fire_count = excluded.fire_count,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.fire_count > timers.fire_count
            OR (
                excluded.fire_count = timers.fire_count
                AND (
                    CASE excluded.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    > CASE timers.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    OR (
                        excluded.status = timers.status
                        AND excluded.updated_at >= timers.updated_at
                    )
                )
            )",
        params![
            record.id,
            record.agent_id,
            status,
            record.summary,
            timestamp(record.created_at),
            record.duration_ms as i64,
            record.interval_ms.map(|value| value as i64),
            i64::from(record.repeat),
            record.next_fire_at.map(timestamp),
            record.last_fired_at.map(timestamp),
            record.fire_count as i64,
            timestamp(updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_turn_record_tx(tx: &Transaction<'_>, record: &TurnRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let terminal_kind = record
        .terminal
        .as_ref()
        .map(|terminal| enum_string(&terminal.kind))
        .transpose()?;
    let completed_at = record
        .terminal
        .as_ref()
        .map(|terminal| timestamp(terminal.completed_at));
    tx.execute(
        "INSERT INTO turn_records (
            turn_id, turn_index, agent_id, run_id, current_work_item_id,
            trigger_message_id, terminal_kind, created_at, completed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(turn_id) DO UPDATE SET
            turn_index = excluded.turn_index,
            agent_id = excluded.agent_id,
            run_id = excluded.run_id,
            current_work_item_id = excluded.current_work_item_id,
            trigger_message_id = excluded.trigger_message_id,
            terminal_kind = excluded.terminal_kind,
            created_at = excluded.created_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json
         WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
        params![
            record.turn_id,
            record.turn_index as i64,
            record.agent_id,
            record.run_id,
            record.current_work_item_id,
            record
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref()),
            terminal_kind,
            timestamp(record.created_at),
            completed_at,
            payload_json,
        ],
    )?;
    Ok(())
}

fn derive_turn_records_from_legacy_evidence(
    messages: Vec<serde_json::Value>,
    tool_executions: Vec<ToolExecutionRecord>,
    briefs: Vec<BriefRecord>,
    delivery_summaries: Vec<DeliverySummaryRecord>,
    wait_conditions: Vec<WaitConditionRecord>,
) -> Result<Vec<TurnRecord>> {
    let mut records = BTreeMap::<String, TurnRecord>::new();
    for raw_message in messages {
        if let Some(message) = normalize_legacy_message_value(raw_message)? {
            let turn_key = turn_key_from_message(&message);
            let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
                TurnRecord::new(&message.agent_id, &turn_key.turn_id, turn_key.turn_index)
            });
            reinforce_turn_index(record, &turn_key);
            record.created_at = record.created_at.min(message.created_at);
            record.input_message_ids.push(message.id.clone());
            if record.trigger.is_none() {
                record.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
            }
            if record.current_work_item_id.is_none() {
                record.current_work_item_id = message.work_item_id.clone();
            }
        }
    }
    for tool in tool_executions {
        let Some(turn_key) = turn_key_from_optional(tool.turn_id.as_deref(), tool.turn_index)
        else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&tool.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(tool.created_at);
        record.tool_execution_ids.push(tool.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = tool.work_item_id.clone();
        }
    }
    for brief in briefs {
        let Some(turn_key) = turn_key_from_optional(
            brief.turn_id.as_deref(),
            brief.turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&brief.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(brief.created_at);
        record.produced_brief_ids.push(brief.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = brief.work_item_id.clone();
        }
    }
    for summary in delivery_summaries {
        let Some(turn_key) = turn_key_from_optional(
            summary.turn_id.as_deref(),
            summary.source_turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&summary.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(summary.created_at);
        record.delivery_summary_ids.push(summary.id.clone());
        record
            .completed_work_item_ids
            .push(summary.work_item_id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = Some(summary.work_item_id.clone());
        }
    }
    for condition in wait_conditions {
        let Some(turn_id) = condition
            .turn_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let record = records
            .entry(turn_id.trim().to_string())
            .or_insert_with(|| TurnRecord::new(&condition.agent_id, turn_id.trim(), 0));
        record.created_at = record.created_at.min(condition.created_at);
        record.waiting_condition_ids.push(condition.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = condition.work_item_id.clone();
        }
    }
    for record in records.values_mut() {
        record.input_message_ids.sort();
        record.input_message_ids.dedup();
        record.tool_execution_ids.sort();
        record.tool_execution_ids.dedup();
        record.produced_brief_ids.sort();
        record.produced_brief_ids.dedup();
        record.delivery_summary_ids.sort();
        record.delivery_summary_ids.dedup();
        record.completed_work_item_ids.sort();
        record.completed_work_item_ids.dedup();
        record.waiting_condition_ids.sort();
        record.waiting_condition_ids.dedup();
    }
    Ok(records.into_values().collect())
}

struct DerivedTurnKey {
    turn_id: String,
    turn_index: u64,
}

fn reinforce_turn_index(record: &mut TurnRecord, turn_key: &DerivedTurnKey) {
    if record.turn_index == 0 && turn_key.turn_index != 0 {
        record.turn_index = turn_key.turn_index;
    }
}

fn turn_key_from_message(message: &MessageEnvelope) -> DerivedTurnKey {
    if let Some(turn_id) = message
        .turn_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index: 0,
        };
    }
    let turn_index = message.message_seq.unwrap_or_default();
    DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    }
}

fn turn_key_from_optional(turn_id: Option<&str>, turn_index: u64) -> Option<DerivedTurnKey> {
    if let Some(turn_id) = turn_id.filter(|value| !value.trim().is_empty()) {
        return Some(DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index,
        });
    }
    (turn_index != 0).then(|| DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    })
}

fn newer_work_item_record(candidate: &WorkItemRecord, existing: &WorkItemRecord) -> bool {
    candidate
        .revision
        .cmp(&existing.revision)
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

fn reduce_wait_condition_records(
    records: Vec<WaitConditionRecord>,
) -> BTreeMap<String, WaitConditionRecord> {
    let mut latest = BTreeMap::<String, WaitConditionRecord>::new();
    for record in records {
        if latest
            .get(&record.id)
            .is_none_or(|existing| newer_wait_condition_record(&record, existing))
        {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

fn newer_wait_condition_record(
    candidate: &WaitConditionRecord,
    existing: &WaitConditionRecord,
) -> bool {
    candidate
        .updated_at
        .cmp(&existing.updated_at)
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.id.cmp(&existing.id))
        .is_gt()
}

fn reduce_timer_records(records: Vec<TimerRecord>) -> BTreeMap<String, TimerRecord> {
    let mut latest = BTreeMap::<String, TimerRecord>::new();
    for record in records {
        if latest
            .get(&record.id)
            .is_none_or(|existing| newer_timer_record(&record, existing))
        {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

fn newer_timer_record(candidate: &TimerRecord, existing: &TimerRecord) -> bool {
    candidate
        .fire_count
        .cmp(&existing.fire_count)
        .then_with(|| {
            timer_status_rank(&candidate.status).cmp(&timer_status_rank(&existing.status))
        })
        .then_with(|| timer_updated_at(candidate).cmp(&timer_updated_at(existing)))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.id.cmp(&existing.id))
        .is_gt()
}

fn timer_status_rank(status: &TimerStatus) -> u8 {
    match status {
        TimerStatus::Active => 0,
        TimerStatus::Cancelled => 1,
        TimerStatus::Completed => 2,
    }
}

fn timer_updated_at(record: &TimerRecord) -> DateTime<Utc> {
    record
        .last_fired_at
        .or(record.next_fire_at)
        .unwrap_or(record.created_at)
}

fn reduce_external_trigger_records(
    records: Vec<ExternalTriggerRecord>,
) -> BTreeMap<String, ExternalTriggerRecord> {
    let mut latest_by_id = BTreeMap::<String, ExternalTriggerRecord>::new();
    for record in records {
        if latest_by_id
            .get(&record.external_trigger_id)
            .is_none_or(|existing| newer_external_trigger_record(&record, existing))
        {
            latest_by_id.insert(
                record.external_trigger_id.clone(),
                normalize_external_trigger_record(record),
            );
        }
    }

    let mut active_by_agent = BTreeMap::<String, String>::new();
    for record in latest_by_id.values() {
        if record.status != ExternalTriggerStatus::Active {
            continue;
        }
        let replace = active_by_agent
            .get(&record.target_agent_id)
            .and_then(|id| latest_by_id.get(id))
            .is_none_or(|existing| newer_external_trigger_record(record, existing));
        if replace {
            active_by_agent.insert(
                record.target_agent_id.clone(),
                record.external_trigger_id.clone(),
            );
        }
    }

    for record in latest_by_id.values_mut() {
        if record.status == ExternalTriggerStatus::Active
            && active_by_agent.get(&record.target_agent_id) != Some(&record.external_trigger_id)
        {
            record.status = ExternalTriggerStatus::Revoked;
            record.revoked_at.get_or_insert(record.created_at);
        }
    }
    latest_by_id
}

fn normalize_external_trigger_record(mut record: ExternalTriggerRecord) -> ExternalTriggerRecord {
    record.scope = ExternalTriggerScope::Agent;
    record.delivery_mode = CallbackDeliveryMode::WakeHint;
    record.waiting_intent_id = None;
    record
}

fn newer_external_trigger_record(
    candidate: &ExternalTriggerRecord,
    existing: &ExternalTriggerRecord,
) -> bool {
    candidate
        .delivery_count
        .cmp(&existing.delivery_count)
        .then_with(|| candidate.last_delivered_at.cmp(&existing.last_delivered_at))
        .then_with(|| candidate.revoked_at.cmp(&existing.revoked_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| {
            candidate
                .external_trigger_id
                .cmp(&existing.external_trigger_id)
        })
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

fn reduce_work_item_delegation_records(
    records: Vec<WorkItemDelegationRecord>,
) -> BTreeMap<String, WorkItemDelegationRecord> {
    let mut latest = BTreeMap::<String, WorkItemDelegationRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.delegation_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.delegation_id.clone(), record);
        }
    }
    latest
}

fn reduce_context_episode_records(
    records: Vec<ContextEpisodeRecord>,
) -> BTreeMap<String, ContextEpisodeRecord> {
    let mut latest = BTreeMap::<String, ContextEpisodeRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.id)
            .is_none_or(|existing| record.finalized_at >= existing.finalized_at);
        if should_replace {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

fn reduce_workspace_entry_records(
    records: Vec<WorkspaceEntry>,
) -> BTreeMap<String, WorkspaceEntry> {
    let mut latest = BTreeMap::<String, WorkspaceEntry>::new();
    for record in records {
        let should_replace = latest
            .get(&record.workspace_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.workspace_id.clone(), record);
        }
    }
    latest
}

fn reduce_workspace_occupancy_records(
    records: Vec<WorkspaceOccupancyRecord>,
) -> BTreeMap<String, WorkspaceOccupancyRecord> {
    let mut latest = BTreeMap::<String, WorkspaceOccupancyRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.occupancy_id)
            .is_none_or(|existing| record.released_at >= existing.released_at);
        if should_replace {
            latest.insert(record.occupancy_id.clone(), record);
        }
    }
    latest
}

fn reduce_agent_identity_records(
    records: Vec<AgentIdentityRecord>,
) -> BTreeMap<String, AgentIdentityRecord> {
    let mut latest = BTreeMap::<String, AgentIdentityRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.agent_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.agent_id.clone(), record);
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

fn decode_agent_state_payload(payload: &str) -> Result<AgentState> {
    serde_json::from_str(payload).context("decoding agent state payload from runtime db")
}

fn decode_workspace_entry_payload(payload: &str) -> Result<WorkspaceEntry> {
    serde_json::from_str(payload).context("decoding workspace entry payload from runtime db")
}

fn decode_workspace_occupancy_payload(payload: &str) -> Result<WorkspaceOccupancyRecord> {
    serde_json::from_str(payload).context("decoding workspace occupancy payload from runtime db")
}

fn decode_agent_identity_payload(payload: &str) -> Result<AgentIdentityRecord> {
    serde_json::from_str(payload).context("decoding agent identity payload from runtime db")
}

fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}

fn decode_work_item_delegation_payload(payload: &str) -> Result<WorkItemDelegationRecord> {
    serde_json::from_str(payload).context("decoding work item delegation payload from runtime db")
}

fn decode_work_item_continuation_payload(payload: &str) -> Result<WorkItemContinuationFrame> {
    serde_json::from_str(payload).context("decoding work item continuation payload from runtime db")
}

fn decode_context_episode_payload(payload: &str) -> Result<ContextEpisodeRecord> {
    serde_json::from_str(payload).context("decoding context episode payload from runtime db")
}

fn decode_external_trigger_payload(payload: &str) -> Result<ExternalTriggerRecord> {
    serde_json::from_str(payload).context("decoding external trigger payload from runtime db")
}

fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

fn decode_wait_condition_payload(payload: &str) -> Result<WaitConditionRecord> {
    serde_json::from_str(payload).context("decoding wait condition payload from runtime db")
}

fn decode_queue_entry_payload(payload: &str) -> Result<QueueEntryRecord> {
    serde_json::from_str(payload).context("decoding queue entry payload from runtime db")
}

fn decode_timer_payload(payload: &str) -> Result<TimerRecord> {
    serde_json::from_str(payload).context("decoding timer payload from runtime db")
}

fn decode_turn_record_payload(payload: &str) -> Result<TurnRecord> {
    serde_json::from_str(payload).context("decoding turn record payload from runtime db")
}

fn decode_message_payload(payload: &str) -> Result<MessageEnvelope> {
    serde_json::from_str(payload).context("decoding message payload from runtime db")
}

fn decode_transcript_entry_payload(payload: &str) -> Result<TranscriptEntry> {
    serde_json::from_str(payload).context("decoding transcript entry payload from runtime db")
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
    Migration {
        version: 4,
        name: "external_triggers_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS external_triggers (
  external_trigger_id TEXT PRIMARY KEY,
  target_agent_id TEXT NOT NULL,
  waiting_intent_id TEXT,
  trigger_url TEXT,
  token_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  revoked_at TEXT,
  last_delivered_at TEXT,
  delivery_count INTEGER NOT NULL DEFAULT 0,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_external_triggers_agent_status
  ON external_triggers(target_agent_id, status);

CREATE INDEX IF NOT EXISTS idx_external_triggers_token_hash
  ON external_triggers(token_hash);

CREATE UNIQUE INDEX IF NOT EXISTS idx_external_triggers_active_default_agent
  ON external_triggers(target_agent_id)
  WHERE status = 'active';
"#,
    },
    Migration {
        version: 5,
        name: "evidence_indexing_and_audit_sink",
        sql: r#"
CREATE TABLE IF NOT EXISTS messages (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  message_seq INTEGER,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS transcript_entries (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  transcript_seq INTEGER,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_executions (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_requests (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_responses (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS briefs (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS delivery_summaries (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_metadata (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_agent_turn
  ON messages(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_messages_message
  ON messages(message_id);
CREATE INDEX IF NOT EXISTS idx_messages_seq
  ON messages(message_seq);
CREATE INDEX IF NOT EXISTS idx_messages_task
  ON messages(task_id);
CREATE INDEX IF NOT EXISTS idx_messages_work_item
  ON messages(work_item_id);

CREATE INDEX IF NOT EXISTS idx_transcript_entries_agent_turn
  ON transcript_entries(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_message
  ON transcript_entries(message_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_seq
  ON transcript_entries(transcript_seq);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_task
  ON transcript_entries(task_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_work_item
  ON transcript_entries(work_item_id);

CREATE INDEX IF NOT EXISTS idx_tool_executions_agent_turn
  ON tool_executions(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_message
  ON tool_executions(message_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_task
  ON tool_executions(task_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_work_item
  ON tool_executions(work_item_id);

CREATE INDEX IF NOT EXISTS idx_model_requests_agent_turn
  ON model_requests(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_message
  ON model_requests(message_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_task
  ON model_requests(task_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_work_item
  ON model_requests(work_item_id);

CREATE INDEX IF NOT EXISTS idx_model_responses_agent_turn
  ON model_responses(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_message
  ON model_responses(message_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_task
  ON model_responses(task_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_work_item
  ON model_responses(work_item_id);

CREATE INDEX IF NOT EXISTS idx_briefs_agent_turn
  ON briefs(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_briefs_message
  ON briefs(message_id);
CREATE INDEX IF NOT EXISTS idx_briefs_task
  ON briefs(task_id);
CREATE INDEX IF NOT EXISTS idx_briefs_work_item
  ON briefs(work_item_id);

CREATE INDEX IF NOT EXISTS idx_delivery_summaries_agent_turn
  ON delivery_summaries(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_message
  ON delivery_summaries(message_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_task
  ON delivery_summaries(task_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_work_item
  ON delivery_summaries(work_item_id);

CREATE INDEX IF NOT EXISTS idx_artifact_metadata_agent_turn
  ON artifact_metadata(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_message
  ON artifact_metadata(message_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_task
  ON artifact_metadata(task_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_work_item
  ON artifact_metadata(work_item_id);
"#,
    },
    Migration {
        version: 6,
        name: "scheduler_control_plane_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS wait_conditions (
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
);

CREATE TABLE IF NOT EXISTS queue_entries (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);

CREATE TABLE IF NOT EXISTS timers (
  timer_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  status TEXT NOT NULL,
  summary TEXT,
  created_at TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  interval_ms INTEGER,
  repeat INTEGER NOT NULL DEFAULT 0,
  next_fire_at TEXT,
  last_fired_at TEXT,
  fire_count INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_agent_status
  ON wait_conditions(agent_id, status);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_work_item_status
  ON wait_conditions(work_item_id, status);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_subject
  ON wait_conditions(kind, subject_ref);

CREATE INDEX IF NOT EXISTS idx_queue_entries_agent_status
  ON queue_entries(agent_id, status, updated_at);

CREATE INDEX IF NOT EXISTS idx_timers_agent_status
  ON timers(agent_id, status, next_fire_at);
"#,
    },
    Migration {
        version: 7,
        name: "queue_entries_preserve_lifecycle_history",
        sql: r#"
CREATE TABLE IF NOT EXISTS queue_entries_v2 (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);

INSERT OR REPLACE INTO queue_entries_v2 (
  message_id, agent_id, priority, status, created_at, updated_at, payload_json
)
SELECT message_id, agent_id, priority, status, created_at, updated_at, payload_json
FROM queue_entries;

DROP TABLE queue_entries;

ALTER TABLE queue_entries_v2 RENAME TO queue_entries;

CREATE INDEX IF NOT EXISTS idx_queue_entries_agent_status
  ON queue_entries(agent_id, status, updated_at);
"#,
    },
    Migration {
        version: 8,
        name: "turn_records_spine",
        sql: r#"
CREATE TABLE IF NOT EXISTS turn_records (
  turn_id TEXT PRIMARY KEY,
  turn_index INTEGER NOT NULL,
  agent_id TEXT NOT NULL,
  run_id TEXT,
  current_work_item_id TEXT,
  trigger_message_id TEXT,
  terminal_kind TEXT,
  created_at TEXT NOT NULL,
  completed_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_records_agent_recent
  ON turn_records(agent_id, turn_index, created_at);

CREATE INDEX IF NOT EXISTS idx_turn_records_work_item
  ON turn_records(current_work_item_id);
"#,
    },
    Migration {
        version: 9,
        name: "agent_workspace_registry_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS agent_states (
  agent_id TEXT PRIMARY KEY,
  status TEXT NOT NULL,
  turn_index INTEGER NOT NULL DEFAULT 0,
  current_run_id TEXT,
  current_work_item_id TEXT,
  active_workspace_id TEXT,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_entries (
  workspace_id TEXT PRIMARY KEY,
  workspace_alias TEXT,
  workspace_kind TEXT,
  owner_agent_id TEXT,
  workspace_anchor TEXT NOT NULL,
  repo_name TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_occupancies (
  occupancy_id TEXT PRIMARY KEY,
  execution_root_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  holder_agent_id TEXT NOT NULL,
  access_mode TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  released_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_identities (
  agent_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  visibility TEXT NOT NULL,
  ownership TEXT,
  profile_preset TEXT,
  status TEXT NOT NULL,
  parent_agent_id TEXT,
  lineage_parent_agent_id TEXT,
  delegated_from_task_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_states_status
  ON agent_states(status);

CREATE INDEX IF NOT EXISTS idx_workspace_entries_anchor
  ON workspace_entries(workspace_anchor);

CREATE INDEX IF NOT EXISTS idx_workspace_occupancies_root_active
  ON workspace_occupancies(execution_root_id, released_at);

CREATE INDEX IF NOT EXISTS idx_workspace_occupancies_holder
  ON workspace_occupancies(holder_agent_id);

CREATE INDEX IF NOT EXISTS idx_agent_identities_status
  ON agent_identities(status);
"#,
    },
    Migration {
        version: 10,
        name: "audit_events_agent_seq_index",
        sql: r#"
CREATE INDEX IF NOT EXISTS idx_audit_events_agent_seq_created
  ON audit_events(agent_id, event_seq, created_at);
"#,
    },
    Migration {
        version: 11,
        name: "memory_episode_delegation_domains",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_item_delegations (
  delegation_id TEXT PRIMARY KEY,
  parent_agent_id TEXT NOT NULL,
  parent_work_item_id TEXT NOT NULL,
  child_agent_id TEXT NOT NULL,
  child_work_item_id TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS context_episode_anchors (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_item_delegations_parent
  ON work_item_delegations(parent_agent_id, parent_work_item_id);
CREATE INDEX IF NOT EXISTS idx_work_item_delegations_child
  ON work_item_delegations(child_agent_id, child_work_item_id);
CREATE INDEX IF NOT EXISTS idx_work_item_delegations_state
  ON work_item_delegations(state);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_agent_turn
  ON context_episode_anchors(agent_id, end_turn_index);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_work_item
  ON context_episode_anchors(work_item_id);
"#,
    },
    Migration {
        version: 12,
        name: "work_item_continuation_stack",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_item_continuations (
  continuation_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  suspended_work_item_id TEXT NOT NULL,
  active_work_item_id TEXT NOT NULL,
  return_policy TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  resolved_at TEXT,
  cancelled_at TEXT,
  resolution_reason TEXT,
  last_turn_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_item_continuations_agent_state
  ON work_item_continuations(agent_id, state);
CREATE INDEX IF NOT EXISTS idx_work_item_continuations_suspended
  ON work_item_continuations(agent_id, suspended_work_item_id, state);
CREATE INDEX IF NOT EXISTS idx_work_item_continuations_active
  ON work_item_continuations(agent_id, active_work_item_id, state);
"#,
    },
    Migration {
        version: 13,
        name: "context_episode_anchors_table",
        sql: r#"
DROP INDEX IF EXISTS idx_context_episodes_agent_turn;
DROP INDEX IF EXISTS idx_context_episodes_work_item;
DROP TABLE IF EXISTS context_episodes;
DELETE FROM storage_domains WHERE domain = 'context_episodes';

CREATE TABLE IF NOT EXISTS context_episode_anchors (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_agent_turn
  ON context_episode_anchors(agent_id, end_turn_index);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_work_item
  ON context_episode_anchors(work_item_id);
"#,
    },
    Migration {
        version: 14,
        name: "drop_working_memory_deltas",
        sql: r#"
DROP INDEX IF EXISTS idx_working_memory_deltas_revision;
DROP TABLE IF EXISTS working_memory_deltas;
DELETE FROM storage_domains WHERE domain = 'working_memory_deltas';
"#,
    },
    Migration {
        version: 15,
        name: "message_search_index",
        sql: r#"
CREATE TABLE IF NOT EXISTS messages (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  message_seq INTEGER,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS message_search_index USING fts5(
  evidence_id UNINDEXED,
  agent_id UNINDEXED,
  turn_id UNINDEXED,
  message_id UNINDEXED,
  task_id UNINDEXED,
  work_item_id UNINDEXED,
  kind,
  body_text,
  payload_json
);

INSERT INTO message_search_index (
  evidence_id, agent_id, turn_id, message_id, task_id, work_item_id,
  kind, body_text, payload_json
)
SELECT
  messages.evidence_id,
  messages.agent_id,
  messages.turn_id,
  messages.message_id,
  messages.task_id,
  messages.work_item_id,
  messages.kind,
  COALESCE(messages.preview, ''),
  messages.payload_json
FROM messages
WHERE NOT EXISTS (
  SELECT 1
  FROM message_search_index
  WHERE message_search_index.evidence_id = messages.evidence_id
);
"#,
    },
];

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

    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.writer.append_wait(f)
    }

    fn transaction_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.writer.append_wait_with_context(context, f)
    }

    pub fn append(
        &self,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.writer.append(f)
    }

    fn append_with_context(
        &self,
        context: RuntimeDbWriteContext,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.writer.append_with_context(context, f)
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

    pub const fn expected_storage_domains() -> &'static [ExpectedStorageDomain] {
        &[
            ExpectedStorageDomain {
                domain: "agent_states",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "workspace_entries",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "workspace_occupancies",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "agent_identities",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "work_items",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "work_item_delegations",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "work_item_continuations",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::Disabled,
            },
            ExpectedStorageDomain {
                domain: CONTEXT_EPISODE_ANCHORS_DOMAIN,
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "tasks",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "external_triggers",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "wait_conditions",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "queue_entries",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "timers",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "turn_records",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::Disabled,
            },
            ExpectedStorageDomain {
                domain: "messages",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "transcript_entries",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "evidence",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
            },
            ExpectedStorageDomain {
                domain: "audit_events",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::LegacyImportOnly,
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
                    "storage domain {} is missing; expected canonical_source={} legacy_jsonl_posture={}",
                    expected_domain.domain,
                    expected_domain.canonical_source,
                    expected_domain.legacy_jsonl_posture.as_str()
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

    pub(crate) fn mark_storage_domain_complete(
        &self,
        domain: &'static str,
        canonical_source: &'static str,
        checkpoint: serde_json::Value,
    ) -> Result<()> {
        self.transaction(|tx| {
            upsert_storage_domain(tx, domain, "complete", canonical_source, Some(checkpoint))
        })
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
        let result = self.transaction(|tx| {
            let checkpoint = import(tx)?;
            upsert_storage_domain(tx, domain, "complete", complete_source, Some(checkpoint))?;
            Ok(())
        });
        if let Err(error) = result {
            let checkpoint = serde_json::json!({
                "error": error.to_string(),
                "retry": "restart runtime to retry legacy import",
            });
            self.transaction(|tx| {
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
    let started_at = Instant::now();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    configure_connection(&connection)?;
    crate::diagnostics::record_runtime_db_connection_open(started_at.elapsed());
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(RUNTIME_DB_BUSY_TIMEOUT)?;
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
"#,
    )?;
    Ok(())
}

fn configure_persistent_database(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
PRAGMA journal_mode = WAL;
"#,
    )?;
    Ok(())
}

fn run_transaction_on_connection<T>(
    connection: &Connection,
    path: &Path,
    context: RuntimeDbWriteContext,
    queue_wait: Duration,
    mutex_wait: Duration,
    f: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let started_at = Instant::now();
    tracing::trace!(
        path = %path.display(),
        operation = context.operation,
        table = context.table,
        mode = context.mode.as_str(),
        queue_wait_ms = queue_wait.as_millis(),
        mutex_wait_ms = mutex_wait.as_millis(),
        "runtime db write starting"
    );
    let (transaction, begin_retry_count, begin_wait) =
        begin_immediate_transaction_with_retry(connection, path)?;
    match f(&transaction) {
        Ok(value) => {
            transaction.commit().map_err(|error| {
                map_runtime_db_sqlite_error("committing transaction", path, error)
            })?;
            let elapsed = started_at.elapsed();
            tracing::trace!(
                path = %path.display(),
                operation = context.operation,
                table = context.table,
                mode = context.mode.as_str(),
                queue_wait_ms = queue_wait.as_millis(),
                mutex_wait_ms = mutex_wait.as_millis(),
                begin_wait_ms = begin_wait.as_millis(),
                begin_retry_count,
                elapsed_ms = elapsed.as_millis(),
                "runtime db write committed"
            );
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback();
            let elapsed = started_at.elapsed();
            tracing::warn!(
                error = %error,
                retryable = is_retryable_db_error(&error),
                path = %path.display(),
                operation = context.operation,
                table = context.table,
                mode = context.mode.as_str(),
                queue_wait_ms = queue_wait.as_millis(),
                mutex_wait_ms = mutex_wait.as_millis(),
                begin_wait_ms = begin_wait.as_millis(),
                begin_retry_count,
                elapsed_ms = elapsed.as_millis(),
                "runtime db write rolled back"
            );
            Err(error)
        }
    }
}

fn begin_immediate_transaction_with_retry<'connection>(
    connection: &'connection Connection,
    path: &Path,
) -> Result<(Transaction<'connection>, u32, Duration)> {
    let started_at = Instant::now();
    let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
    let mut retry_count = 0;
    loop {
        // The writer mutex prevents concurrent transactions on this connection;
        // the retry loop absorbs transient locks from external processes or connections.
        match Transaction::new_unchecked(connection, TransactionBehavior::Immediate) {
            Ok(transaction) => return Ok((transaction, retry_count, started_at.elapsed())),
            Err(error)
                if is_sqlite_locked(&error) && started_at.elapsed() < RUNTIME_DB_BUSY_TIMEOUT =>
            {
                retry_count += 1;
                tracing::trace!(
                    error = %error,
                    path = %path.display(),
                    retry_count,
                    retry_delay_ms = retry_delay.as_millis(),
                    "runtime db begin immediate transaction retrying"
                );
                thread::sleep(retry_delay);
                retry_delay = next_runtime_db_retry_delay(
                    retry_delay,
                    RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY,
                );
            }
            Err(error) if is_sqlite_locked(&error) => {
                return Err(RuntimeDbRetryableError::new(
                    "starting immediate transaction",
                    path,
                    error,
                )
                .into());
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "starting immediate runtime db transaction for {}",
                        path.display()
                    )
                });
            }
        }
    }
}

fn next_runtime_db_retry_delay(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

fn map_runtime_db_sqlite_error(
    operation: &'static str,
    path: &Path,
    error: rusqlite::Error,
) -> anyhow::Error {
    if is_sqlite_locked(&error) {
        RuntimeDbRetryableError::new(operation, path, error).into()
    } else {
        anyhow!(error).context(format!("{} for {}", operation, path.display()))
    }
}

fn is_sqlite_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
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

#[cfg(test)]
fn table_exists(connection: &Connection, table_name: &str) -> Result<bool> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table_name],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
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
            waiting_intent_id: Some(format!("wait-{id}")),
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
    fn message_search_indexes_messages_across_agents() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut first = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text:
                    "find the kestrel runtime note for PR #1786 on feature/issue-1783 with foo-bar"
                        .into(),
            },
        );
        first.id = "msg-search-a".into();
        let mut second = MessageEnvelope::new(
            "agent-b",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "another kestrel message".into(),
            },
        );
        second.id = "msg-search-b".into();
        db.messages().upsert(&first)?;
        db.messages().upsert(&second)?;

        let all = db.messages().search(MessageSearchQuery {
            query: "kestrel".into(),
            agent_ids: Vec::new(),
            limit: 10,
        })?;
        assert_eq!(all.len(), 2);

        let filtered = db.messages().search(MessageSearchQuery {
            query: "kestrel".into(),
            agent_ids: vec!["agent-b".into()],
            limit: 10,
        })?;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].agent_id, "agent-b");
        assert_eq!(filtered[0].message_id, "msg-search-b");

        for query in ["PR #1786", "feature/issue-1783", "foo-bar"] {
            let results = db.messages().search(MessageSearchQuery {
                query: query.into(),
                agent_ids: Vec::new(),
                limit: 10,
            })?;
            assert_eq!(results.len(), 1, "query {query:?}");
            assert_eq!(results[0].message_id, "msg-search-a");
        }
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
        assert_eq!(latest.len(), 2);
        assert!(latest.iter().any(|record| {
            record.message_id == "message-1" && record.status == QueueEntryStatus::Queued
        }));
        assert!(latest.iter().any(|record| {
            record.message_id == "message-1" && record.status == QueueEntryStatus::Dequeued
        }));
        Ok(())
    }

    #[test]
    fn queue_claim_rejects_message_whose_latest_lifecycle_is_terminal() -> Result<()> {
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
        assert_eq!(active.waiting_intent_id, None);

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
        trigger.waiting_intent_id = None;
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
