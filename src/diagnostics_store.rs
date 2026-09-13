use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::observability::{RecentTrace, RecentTraceSummary};

const DEFAULT_QUEUE_CAPACITY: usize = 256;
const DEFAULT_SLOW_TRACE_THRESHOLD: Duration = Duration::from_secs(1);
const DEFAULT_RETENTION_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const DEFAULT_RETENTION_MAX_TRACES: usize = 10_000;
const DEFAULT_RETENTION_MIN_TRACES: usize = 1_000;
const DEFAULT_RETENTION_DELETE_BATCH: usize = 256;

static DIAGNOSTICS_HANDLE: OnceLock<Mutex<Option<DiagnosticsHandle>>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub struct DiagnosticsWriterConfig {
    pub queue_capacity: usize,
    pub slow_trace_threshold: Duration,
    pub sample_rate_permyriad: u16,
    pub retention_age: Duration,
    pub retention_max_traces: usize,
    pub retention_min_traces: usize,
    pub retention_delete_batch: usize,
}

impl Default for DiagnosticsWriterConfig {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            slow_trace_threshold: DEFAULT_SLOW_TRACE_THRESHOLD,
            sample_rate_permyriad: 0,
            retention_age: DEFAULT_RETENTION_AGE,
            retention_max_traces: DEFAULT_RETENTION_MAX_TRACES,
            retention_min_traces: DEFAULT_RETENTION_MIN_TRACES,
            retention_delete_batch: DEFAULT_RETENTION_DELETE_BATCH,
        }
    }
}

impl DiagnosticsWriterConfig {
    pub fn validate(self) -> Result<Self> {
        if self.queue_capacity == 0 {
            return Err(anyhow!(
                "diagnostics queue capacity must be greater than zero"
            ));
        }
        if self.sample_rate_permyriad > 10_000 {
            return Err(anyhow!(
                "diagnostics sample rate must not exceed 10000 permyriad"
            ));
        }
        if self.retention_max_traces == 0 {
            return Err(anyhow!(
                "diagnostics retention max traces must be greater than zero"
            ));
        }
        if self.retention_min_traces > self.retention_max_traces {
            return Err(anyhow!(
                "diagnostics retention min traces must not exceed max traces"
            ));
        }
        if self.retention_delete_batch == 0 {
            return Err(anyhow!(
                "diagnostics retention delete batch must be greater than zero"
            ));
        }
        ChronoDuration::from_std(self.retention_age)
            .context("diagnostics retention age is out of range")?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsWriterStats {
    pub queue_depth: u64,
    pub queued_traces: u64,
    pub dropped_traces: u64,
    pub filtered_traces: u64,
    pub persisted_traces: u64,
    pub retention_deleted_traces: u64,
    pub writer_failures: u64,
}

#[derive(Default)]
struct DiagnosticsWriterCounters {
    queue_depth: AtomicU64,
    queued_traces: AtomicU64,
    dropped_traces: AtomicU64,
    filtered_traces: AtomicU64,
    persisted_traces: AtomicU64,
    retention_deleted_traces: AtomicU64,
    writer_failures: AtomicU64,
}

#[derive(Clone)]
pub struct DiagnosticsHandle {
    sender: mpsc::SyncSender<DiagnosticsCommand>,
    counters: Arc<DiagnosticsWriterCounters>,
}

enum DiagnosticsCommand {
    Record(RecentTrace),
    Shutdown,
}

impl DiagnosticsHandle {
    pub fn try_record_trace(&self, trace: RecentTrace) -> bool {
        self.counters.queue_depth.fetch_add(1, Ordering::Relaxed);
        match self.sender.try_send(DiagnosticsCommand::Record(trace)) {
            Ok(()) => {
                self.counters.queued_traces.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) => {
                self.counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.counters.dropped_traces.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn stats(&self) -> DiagnosticsWriterStats {
        DiagnosticsWriterStats {
            queue_depth: self.counters.queue_depth.load(Ordering::Relaxed),
            queued_traces: self.counters.queued_traces.load(Ordering::Relaxed),
            dropped_traces: self.counters.dropped_traces.load(Ordering::Relaxed),
            filtered_traces: self.counters.filtered_traces.load(Ordering::Relaxed),
            persisted_traces: self.counters.persisted_traces.load(Ordering::Relaxed),
            retention_deleted_traces: self
                .counters
                .retention_deleted_traces
                .load(Ordering::Relaxed),
            writer_failures: self.counters.writer_failures.load(Ordering::Relaxed),
        }
    }
}

pub struct DiagnosticsWriter {
    path: PathBuf,
    handle: Option<DiagnosticsHandle>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}

impl DiagnosticsWriter {
    pub fn start(path: impl Into<PathBuf>, config: DiagnosticsWriterConfig) -> Result<Self> {
        let config = config.validate()?;
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let counters = Arc::new(DiagnosticsWriterCounters::default());
        let worker_counters = Arc::clone(&counters);
        let worker_path = path.clone();
        let worker = thread::Builder::new()
            .name("holon-diagnostics-writer".to_string())
            .spawn(move || {
                let mut connection = match open_connection(&worker_path) {
                    Ok(connection) => {
                        let _ = ready_sender.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        let _ = ready_sender.send(Err(message.clone()));
                        return Err(anyhow!(message));
                    }
                };
                let result = run_writer(&mut connection, receiver, config, &worker_counters);
                if result.is_err() {
                    worker_counters
                        .writer_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
                result
            })
            .context("failed to spawn diagnostics writer")?;
        ready_receiver
            .recv()
            .context("diagnostics writer stopped during startup")?
            .map_err(|message| anyhow!(message))?;
        let handle = DiagnosticsHandle { sender, counters };
        Ok(Self {
            path,
            handle: Some(handle),
            worker: Some(worker),
        })
    }

    pub fn handle(&self) -> DiagnosticsHandle {
        self.handle
            .as_ref()
            .expect("diagnostics writer already shut down")
            .clone()
    }

    pub fn install(&self) {
        let mut installed = diagnostics_handle()
            .lock()
            .expect("diagnostics handle lock poisoned");
        *installed = Some(self.handle());
    }

    pub fn stats(&self) -> DiagnosticsWriterStats {
        self.handle().stats()
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        if let Some(handle) = self.handle.as_ref() {
            clear_installed_handle(handle);
        }
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let shutdown = self.handle.take().map(|handle| {
            handle
                .sender
                .send(DiagnosticsCommand::Shutdown)
                .map_err(|_| anyhow!("diagnostics writer stopped before shutdown"))
        });
        let worker_result = worker
            .join()
            .map_err(|_| anyhow!("diagnostics writer thread panicked"))?;
        if let Some(shutdown) = shutdown {
            shutdown?;
        }
        worker_result
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DiagnosticsWriter {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

pub fn try_record_trace(trace: RecentTrace) {
    let handle = diagnostics_handle()
        .lock()
        .expect("diagnostics handle lock poisoned")
        .clone();
    if let Some(handle) = handle {
        handle.try_record_trace(trace);
    }
}

pub fn writer_stats() -> DiagnosticsWriterStats {
    diagnostics_handle()
        .lock()
        .expect("diagnostics handle lock poisoned")
        .as_ref()
        .map(DiagnosticsHandle::stats)
        .unwrap_or_default()
}

pub fn persistent_trace(path: &Path, trace_id: &str) -> Result<Option<RecentTrace>> {
    let connection = open_connection(path)?;
    connection
        .query_row(
            "SELECT trace_json FROM traces WHERE trace_id = ?1",
            [trace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).context("invalid persisted trace JSON"))
        .transpose()
}

pub fn search_persistent_traces(
    path: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<RecentTraceSummary>> {
    let connection = open_connection(path)?;
    let pattern = format!("%{}%", escape_like_pattern(query));
    let mut statement = connection.prepare(
        "SELECT trace_json FROM traces
         WHERE trace_id = ?1
            OR trace_id LIKE ?2 ESCAPE '~'
            OR search_text LIKE ?2 ESCAPE '~'
         ORDER BY completed_at DESC
         LIMIT ?3",
    )?;
    let rows = statement.query_map(params![query, pattern, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| {
        let trace: RecentTrace =
            serde_json::from_str(&row?).context("invalid persisted trace JSON")?;
        Ok(RecentTraceSummary::from(&trace))
    })
    .collect()
}

fn escape_like_pattern(value: &str) -> String {
    value
        .replace('~', "~~")
        .replace('%', "~%")
        .replace('_', "~_")
}

fn diagnostics_handle() -> &'static Mutex<Option<DiagnosticsHandle>> {
    DIAGNOSTICS_HANDLE.get_or_init(|| Mutex::new(None))
}

fn clear_installed_handle(handle: &DiagnosticsHandle) {
    let mut installed = diagnostics_handle()
        .lock()
        .expect("diagnostics handle lock poisoned");
    if installed
        .as_ref()
        .is_some_and(|candidate| Arc::ptr_eq(&candidate.counters, &handle.counters))
    {
        installed.take();
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open diagnostics database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS traces (
             trace_id TEXT PRIMARY KEY NOT NULL,
             started_at TEXT NOT NULL,
             completed_at TEXT NOT NULL,
             duration_us INTEGER NOT NULL,
             span_count INTEGER NOT NULL,
             dropped_spans INTEGER NOT NULL,
             error_count INTEGER NOT NULL,
             search_text TEXT NOT NULL,
             trace_json TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS traces_completed_at_idx
             ON traces(completed_at DESC);",
    )?;
    Ok(connection)
}

fn run_writer(
    connection: &mut Connection,
    receiver: mpsc::Receiver<DiagnosticsCommand>,
    config: DiagnosticsWriterConfig,
    counters: &DiagnosticsWriterCounters,
) -> Result<()> {
    while let Ok(command) = receiver.recv() {
        let DiagnosticsCommand::Record(first) = command else {
            break;
        };
        let mut batch = vec![first];
        let mut shutdown = false;
        for command in receiver.try_iter().take(63) {
            match command {
                DiagnosticsCommand::Record(trace) => batch.push(trace),
                DiagnosticsCommand::Shutdown => {
                    shutdown = true;
                    break;
                }
            }
        }
        counters
            .queue_depth
            .fetch_sub(batch.len() as u64, Ordering::Relaxed);
        let transaction = connection.transaction()?;
        for trace in batch {
            if trace_is_persisted(&transaction, &trace.trace_id)? || should_persist(&trace, config)
            {
                persist_trace(&transaction, &trace)?;
                counters.persisted_traces.fetch_add(1, Ordering::Relaxed);
            } else {
                counters.filtered_traces.fetch_add(1, Ordering::Relaxed);
            }
        }
        let deleted = enforce_retention(&transaction, config)?;
        counters
            .retention_deleted_traces
            .fetch_add(deleted, Ordering::Relaxed);
        transaction.commit()?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

fn trace_is_persisted(transaction: &Transaction<'_>, trace_id: &str) -> Result<bool> {
    transaction
        .query_row(
            "SELECT 1 FROM traces WHERE trace_id = ?1",
            [trace_id],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(Into::into)
}

fn should_persist(trace: &RecentTrace, config: DiagnosticsWriterConfig) -> bool {
    trace.error_count > 0
        || trace.duration_us >= config.slow_trace_threshold.as_micros() as u64
        || sampled(&trace.trace_id, config.sample_rate_permyriad)
}

fn sampled(trace_id: &str, rate_permyriad: u16) -> bool {
    if rate_permyriad == 0 {
        return false;
    }
    if rate_permyriad >= 10_000 {
        return true;
    }
    let hash = trace_id
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    hash % 10_000 < u64::from(rate_permyriad)
}

fn enforce_retention(
    transaction: &Transaction<'_>,
    config: DiagnosticsWriterConfig,
) -> Result<u64> {
    if config.retention_delete_batch == 0 {
        return Ok(0);
    }
    let count = trace_count(transaction)?;
    let floor = config.retention_min_traces.min(count);
    let available = count.saturating_sub(floor);
    if available == 0 {
        return Ok(0);
    }

    let delete_limit = available.min(config.retention_delete_batch);
    let cutoff = Utc::now()
        - ChronoDuration::from_std(config.retention_age)
            .context("diagnostics retention age is out of range")?;
    let age_deleted = transaction.execute(
        "DELETE FROM traces WHERE trace_id IN (
             SELECT trace_id FROM traces
             WHERE completed_at < ?1
             ORDER BY completed_at ASC
             LIMIT ?2
         )",
        params![cutoff.to_rfc3339(), delete_limit as i64],
    )?;

    let remaining_count = count.saturating_sub(age_deleted);
    let size_excess = remaining_count.saturating_sub(config.retention_max_traces.max(floor));
    let remaining_batch = config.retention_delete_batch.saturating_sub(age_deleted);
    let size_limit = size_excess.min(remaining_batch);
    let size_deleted = if size_limit == 0 {
        0
    } else {
        transaction.execute(
            "DELETE FROM traces WHERE trace_id IN (
                 SELECT trace_id FROM traces
                 ORDER BY completed_at ASC
                 LIMIT ?1
             )",
            [size_limit as i64],
        )?
    };
    Ok((age_deleted + size_deleted) as u64)
}

fn trace_count(transaction: &Transaction<'_>) -> Result<usize> {
    let count = transaction.query_row("SELECT COUNT(*) FROM traces", [], |row| {
        row.get::<_, i64>(0)
    })?;
    usize::try_from(count).context("diagnostics trace count is out of range")
}

fn persist_trace(transaction: &Transaction<'_>, trace: &RecentTrace) -> Result<()> {
    let trace_json = serde_json::to_string(trace)?;
    transaction.execute(
        "INSERT INTO traces (
             trace_id, started_at, completed_at, duration_us, span_count,
             dropped_spans, error_count, search_text, trace_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(trace_id) DO UPDATE SET
             started_at = excluded.started_at,
             completed_at = excluded.completed_at,
             duration_us = excluded.duration_us,
             span_count = excluded.span_count,
             dropped_spans = excluded.dropped_spans,
             error_count = excluded.error_count,
             search_text = excluded.search_text,
             trace_json = excluded.trace_json",
        params![
            trace.trace_id,
            trace.started_at.to_rfc3339(),
            trace.completed_at.to_rfc3339(),
            trace.duration_us as i64,
            trace.span_count as i64,
            trace.dropped_spans as i64,
            trace.error_count as i64,
            trace_search_text(trace),
            trace_json,
        ],
    )?;
    Ok(())
}

fn trace_search_text(trace: &RecentTrace) -> String {
    let mut values = Vec::new();
    for span in &trace.spans {
        for value in [
            span.attributes.turn_id.as_deref(),
            span.attributes.message_id.as_deref(),
            span.attributes.run_id.as_deref(),
            span.attributes.work_item_id.as_deref(),
            span.attributes.task_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            values.push(value);
        }
    }
    values.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::{completed_span_at, TraceAttributes, TraceContext, TraceSpanStatus};
    use chrono::{TimeZone, Utc};

    fn trace(trace_id: &str, duration_ms: i64, status: TraceSpanStatus) -> RecentTrace {
        let started_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let completed_at = started_at + chrono::Duration::milliseconds(duration_ms);
        let context = TraceContext {
            trace_id: trace_id.to_string(),
            span_id: "0011223344556677".to_string(),
            trace_flags: 1,
            trace_state: None,
        };
        let span = completed_span_at(
            "holon.turn",
            &context,
            None,
            started_at,
            completed_at,
            status,
            TraceAttributes {
                turn_id: Some("turn-1".to_string()),
                message_id: Some("message-1".to_string()),
                ..Default::default()
            },
        );
        RecentTrace {
            trace_id: trace_id.to_string(),
            started_at,
            completed_at,
            duration_us: duration_ms as u64 * 1_000,
            span_count: 1,
            dropped_spans: 0,
            error_count: usize::from(status == TraceSpanStatus::Error),
            spans: vec![span],
        }
    }

    #[test]
    fn writer_persists_error_and_slow_traces_but_not_fast_successes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let writer = DiagnosticsWriter::start(
            &path,
            DiagnosticsWriterConfig {
                slow_trace_threshold: Duration::from_millis(500),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = writer.handle();
        assert!(handle.try_record_trace(trace("fast", 10, TraceSpanStatus::Ok)));
        assert!(handle.try_record_trace(trace("slow", 600, TraceSpanStatus::Ok)));
        assert!(handle.try_record_trace(trace("error", 10, TraceSpanStatus::Error)));
        writer.shutdown().unwrap();

        assert!(persistent_trace(&path, "fast").unwrap().is_none());
        assert!(persistent_trace(&path, "slow").unwrap().is_some());
        assert!(persistent_trace(&path, "error").unwrap().is_some());
        assert_eq!(
            search_persistent_traces(&path, "turn-1", 10).unwrap().len(),
            2
        );
        assert_eq!(
            search_persistent_traces(&path, "message-1", 10)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn shutdown_drains_queued_updates_and_reopen_reads_latest_trace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let writer = DiagnosticsWriter::start(
            &path,
            DiagnosticsWriterConfig {
                sample_rate_permyriad: 10_000,
                ..Default::default()
            },
        )
        .unwrap();
        let handle = writer.handle();
        assert!(handle.try_record_trace(trace("sampled", 10, TraceSpanStatus::Ok)));
        assert!(handle.try_record_trace(trace("sampled", 20, TraceSpanStatus::Ok)));
        writer.shutdown().unwrap();

        let persisted = persistent_trace(&path, "sampled").unwrap().unwrap();
        assert_eq!(persisted.duration_us, 20_000);
    }

    #[test]
    fn full_queue_drops_without_blocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let handle = DiagnosticsHandle {
            sender,
            counters: Arc::new(DiagnosticsWriterCounters::default()),
        };
        assert!(handle.try_record_trace(trace("first", 10, TraceSpanStatus::Ok)));
        assert!(!handle.try_record_trace(trace("second", 10, TraceSpanStatus::Ok)));
        assert_eq!(handle.stats().queued_traces, 1);
        assert_eq!(handle.stats().dropped_traces, 1);
        assert_eq!(handle.stats().queue_depth, 1);
        drop(receiver);
    }

    #[test]
    fn persistent_search_treats_like_wildcards_as_literals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let mut connection = open_connection(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        persist_trace(
            &transaction,
            &trace("trace-percent%", 10, TraceSpanStatus::Error),
        )
        .unwrap();
        persist_trace(
            &transaction,
            &trace("trace-plain", 10, TraceSpanStatus::Error),
        )
        .unwrap();
        transaction.commit().unwrap();

        let matches = search_persistent_traces(&path, "%", 10).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].trace_id, "trace-percent%");
    }

    #[test]
    fn shutting_down_old_writer_preserves_new_installed_handle() {
        let first_directory = tempfile::tempdir().unwrap();
        let second_directory = tempfile::tempdir().unwrap();
        let first = DiagnosticsWriter::start(
            first_directory.path().join("diagnostics.sqlite"),
            DiagnosticsWriterConfig {
                sample_rate_permyriad: 10_000,
                ..Default::default()
            },
        )
        .unwrap();
        let second_path = second_directory.path().join("diagnostics.sqlite");
        let second = DiagnosticsWriter::start(
            &second_path,
            DiagnosticsWriterConfig {
                sample_rate_permyriad: 10_000,
                ..Default::default()
            },
        )
        .unwrap();
        first.install();
        second.install();

        first.shutdown().unwrap();
        try_record_trace(trace("new-writer", 10, TraceSpanStatus::Ok));
        second.shutdown().unwrap();

        assert!(persistent_trace(&second_path, "new-writer")
            .unwrap()
            .is_some());
    }

    #[test]
    fn dropping_writer_drains_queued_updates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let writer = DiagnosticsWriter::start(
            &path,
            DiagnosticsWriterConfig {
                sample_rate_permyriad: 10_000,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(writer
            .handle()
            .try_record_trace(trace("drop-drain", 10, TraceSpanStatus::Ok)));
        drop(writer);

        assert!(persistent_trace(&path, "drop-drain").unwrap().is_some());
    }

    #[test]
    fn retention_respects_delete_batch_and_recent_floor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let mut connection = open_connection(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        for trace_id in ["first", "second", "third", "fourth"] {
            persist_trace(&transaction, &trace(trace_id, 10, TraceSpanStatus::Error)).unwrap();
        }
        let config = DiagnosticsWriterConfig {
            retention_age: Duration::from_secs(100 * 365 * 24 * 60 * 60),
            retention_max_traces: 2,
            retention_min_traces: 1,
            retention_delete_batch: 1,
            ..Default::default()
        };

        assert_eq!(enforce_retention(&transaction, config).unwrap(), 1);
        assert_eq!(trace_count(&transaction).unwrap(), 3);
        assert_eq!(enforce_retention(&transaction, config).unwrap(), 1);
        assert_eq!(trace_count(&transaction).unwrap(), 2);
        assert_eq!(enforce_retention(&transaction, config).unwrap(), 0);
        transaction.commit().unwrap();
    }

    #[test]
    fn writer_stats_report_persisted_filtered_and_retention_counts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.sqlite");
        let writer = DiagnosticsWriter::start(
            &path,
            DiagnosticsWriterConfig {
                slow_trace_threshold: Duration::from_millis(500),
                retention_age: Duration::from_secs(100 * 365 * 24 * 60 * 60),
                retention_max_traces: 1,
                retention_min_traces: 1,
                retention_delete_batch: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let handle = writer.handle();
        assert!(handle.try_record_trace(trace("fast", 10, TraceSpanStatus::Ok)));
        assert!(handle.try_record_trace(trace("error", 10, TraceSpanStatus::Error)));
        assert!(handle.try_record_trace(trace("slow", 600, TraceSpanStatus::Ok)));
        writer.shutdown().unwrap();

        let stats = handle.stats();
        assert_eq!(stats.queued_traces, 3);
        assert_eq!(stats.filtered_traces, 1);
        assert_eq!(stats.persisted_traces, 2);
        assert_eq!(stats.retention_deleted_traces, 1);
        assert_eq!(stats.dropped_traces, 0);
        assert_eq!(stats.writer_failures, 0);
    }
}
