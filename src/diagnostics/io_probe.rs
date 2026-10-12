//! Opt-in, process-wide measurement for isolated runtime workloads.
//!
//! Start before constructing the workload's RuntimeDb so all of its connections
//! have the SQLite statement observer. Counters describe executed statements and
//! instrumented content-reader operations, not OS syscalls or physical disk I/O.
//! SQL text, file paths, parameters, and credentials are never retained.

use std::{
    collections::BTreeMap,
    ffi::{c_int, c_void, CStr},
    fs::File,
    io::{self, Read},
    path::Path,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use rusqlite::{ffi, Connection};
use serde::{Deserialize, Serialize};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static DB_OPENS: AtomicU64 = AtomicU64::new(0);
static SQL_READS: AtomicU64 = AtomicU64::new(0);
static SQL_WRITES: AtomicU64 = AtomicU64::new(0);
static SQL_CONTROL: AtomicU64 = AtomicU64::new(0);
static FILE_OPENS: AtomicU64 = AtomicU64::new(0);
static DIRECTORY_READS: AtomicU64 = AtomicU64::new(0);
static FILE_READS: AtomicU64 = AtomicU64::new(0);
static FILE_BYTES: AtomicU64 = AtomicU64::new(0);
static FILE_ERRORS: AtomicU64 = AtomicU64::new(0);
static TIMINGS_DROPPED: AtomicU64 = AtomicU64::new(0);
static TIMINGS: Mutex<Vec<TimingSample>> = Mutex::new(Vec::new());
static PREPARATIONS: Mutex<BTreeMap<String, (u64, Instant)>> = Mutex::new(BTreeMap::new());
static NEXT_PREPARATION: AtomicU64 = AtomicU64::new(1);
const MAX_TIMING_SAMPLES: usize = 65_536;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IoProbeCounters {
    pub runtime_db_connection_opens: u64,
    pub sql_read_statements: u64,
    pub sql_write_statements: u64,
    pub sql_control_statements: u64,
    pub file_open_attempts: u64,
    pub file_read_attempts: u64,
    pub directory_read_attempts: u64,
    /// Returned body bytes, including partial read_to_end results; not OS reads.
    pub file_read_bytes: u64,
    pub file_errors: u64,
    pub timing_samples_dropped: u64,
}

impl IoProbeCounters {
    fn current() -> Self {
        Self {
            runtime_db_connection_opens: DB_OPENS.load(Ordering::Relaxed),
            sql_read_statements: SQL_READS.load(Ordering::Relaxed),
            sql_write_statements: SQL_WRITES.load(Ordering::Relaxed),
            sql_control_statements: SQL_CONTROL.load(Ordering::Relaxed),
            file_open_attempts: FILE_OPENS.load(Ordering::Relaxed),
            file_read_attempts: FILE_READS.load(Ordering::Relaxed),
            directory_read_attempts: DIRECTORY_READS.load(Ordering::Relaxed),
            file_read_bytes: FILE_BYTES.load(Ordering::Relaxed),
            file_errors: FILE_ERRORS.load(Ordering::Relaxed),
            timing_samples_dropped: TIMINGS_DROPPED.load(Ordering::Relaxed),
        }
    }

    fn since(&self, earlier: &Self) -> Self {
        macro_rules! delta {
            ($($field:ident),+ $(,)?) => {
                Self { $($field: self.$field.saturating_sub(earlier.$field)),+ }
            };
        }
        delta!(
            runtime_db_connection_opens,
            sql_read_statements,
            sql_write_statements,
            sql_control_statements,
            file_open_attempts,
            file_read_attempts,
            directory_read_attempts,
            file_read_bytes,
            file_errors,
            timing_samples_dropped,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingKind {
    ContextPreparationToRequest,
    CommitPublication,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingSample {
    pub kind: TimingKind,
    pub elapsed_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoProbeReport {
    pub counters: IoProbeCounters,
    pub timing_samples: Vec<TimingSample>,
}

/// Concurrent guards observe the same process traffic; they do not isolate tasks.
/// Reset only after quiescing/warming the workload, and snapshot after its work
/// and background writes finish. Other process traffic is included explicitly.
pub struct IoProbe {
    baseline: IoProbeCounters,
    timing_offset: usize,
}

impl IoProbe {
    pub fn start() -> Self {
        ACTIVE.fetch_add(1, Ordering::AcqRel);
        Self {
            baseline: IoProbeCounters::current(),
            timing_offset: TIMINGS.lock().expect("I/O timings poisoned").len(),
        }
    }

    pub fn reset(&mut self) {
        self.baseline = IoProbeCounters::current();
        self.timing_offset = TIMINGS.lock().expect("I/O timings poisoned").len();
    }

    pub fn snapshot(&self) -> IoProbeReport {
        IoProbeReport {
            counters: IoProbeCounters::current().since(&self.baseline),
            timing_samples: TIMINGS.lock().expect("I/O timings poisoned")[self.timing_offset..]
                .to_vec(),
        }
    }
}

impl Drop for IoProbe {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

fn enabled() -> bool {
    ACTIVE.load(Ordering::Acquire) != 0
}

pub(crate) fn record_db_open() {
    if enabled() {
        DB_OPENS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn observe_sql_connection(connection: &Connection) -> rusqlite::Result<()> {
    if !enabled() {
        return Ok(());
    }
    // SAFETY: the callback keeps no connection or statement pointers. SQLite
    // calls it synchronously while both are alive; its context pointer is unused.
    let code = unsafe {
        ffi::sqlite3_trace_v2(
            connection.handle(),
            ffi::SQLITE_TRACE_STMT as u32,
            Some(sql_statement),
            std::ptr::null_mut(),
        )
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
    }
}

unsafe extern "C" fn sql_statement(
    event: u32,
    _context: *mut c_void,
    statement: *mut c_void,
    _text: *mut c_void,
) -> c_int {
    if !enabled() || event != ffi::SQLITE_TRACE_STMT as u32 || statement.is_null() {
        return 0;
    }
    let statement = statement.cast::<ffi::sqlite3_stmt>();
    // SAFETY: SQLITE_TRACE_STMT supplies a live sqlite3_stmt for this callback.
    let sql = unsafe { ffi::sqlite3_sql(statement) };
    let control = if sql.is_null() {
        false
    } else {
        // Only inspect the first keyword to exclude transaction control; never
        // expand parameters or retain the SQL text.
        let bytes = unsafe { CStr::from_ptr(sql) }.to_bytes();
        let first = bytes
            .split(|byte| byte.is_ascii_whitespace() || *byte == b';')
            .find(|word| !word.is_empty())
            .unwrap_or_default();
        [
            b"BEGIN".as_slice(),
            b"COMMIT",
            b"END",
            b"ROLLBACK",
            b"SAVEPOINT",
            b"RELEASE",
        ]
        .iter()
        .any(|keyword| first.eq_ignore_ascii_case(keyword))
    };
    if control {
        SQL_CONTROL.fetch_add(1, Ordering::Relaxed);
    } else if unsafe { ffi::sqlite3_stmt_readonly(statement) } != 0 {
        SQL_READS.fetch_add(1, Ordering::Relaxed);
    } else {
        SQL_WRITES.fetch_add(1, Ordering::Relaxed);
    }
    0
}

pub(crate) fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let measured = enabled();
    if measured {
        FILE_OPENS.fetch_add(1, Ordering::Relaxed);
        FILE_READS.fetch_add(1, Ordering::Relaxed);
    }
    let result = std::fs::read_to_string(path);
    if measured {
        match &result {
            Ok(body) => {
                FILE_BYTES.fetch_add(body.len() as u64, Ordering::Relaxed);
            }
            Err(_) => {
                FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    result
}

pub(crate) fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let measured = enabled();
    if measured {
        FILE_OPENS.fetch_add(1, Ordering::Relaxed);
        FILE_READS.fetch_add(1, Ordering::Relaxed);
    }
    let result = std::fs::read(path);
    if measured {
        match &result {
            Ok(body) => {
                FILE_BYTES.fetch_add(body.len() as u64, Ordering::Relaxed);
            }
            Err(_) => {
                FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    result
}

pub(crate) fn read_dir(path: impl AsRef<Path>) -> io::Result<std::fs::ReadDir> {
    let measured = enabled();
    if measured {
        DIRECTORY_READS.fetch_add(1, Ordering::Relaxed);
    }
    let result = std::fs::read_dir(path);
    if measured && result.is_err() {
        FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    result
}

pub(crate) fn start_timer() -> Option<Instant> {
    enabled().then(Instant::now)
}

pub(crate) fn open_file(path: impl AsRef<Path>) -> io::Result<File> {
    let measured = enabled();
    if measured {
        FILE_OPENS.fetch_add(1, Ordering::Relaxed);
    }
    let result = File::open(path);
    if measured && result.is_err() {
        FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    result
}

pub(crate) fn read_to_end(reader: &mut impl Read, buffer: &mut Vec<u8>) -> io::Result<usize> {
    let measured = enabled();
    if measured {
        FILE_READS.fetch_add(1, Ordering::Relaxed);
    }
    let before = buffer.len();
    let result = reader.read_to_end(buffer);
    if measured {
        FILE_BYTES.fetch_add(
            buffer.len().saturating_sub(before) as u64,
            Ordering::Relaxed,
        );
        if result.is_err() {
            FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
    }
    result
}

pub(crate) fn record_timing(kind: TimingKind, elapsed: Duration) {
    if !enabled() {
        return;
    }
    let mut timings = TIMINGS.lock().expect("I/O timings poisoned");
    if timings.len() == MAX_TIMING_SAMPLES {
        TIMINGS_DROPPED.fetch_add(1, Ordering::Relaxed);
    } else {
        timings.push(TimingSample {
            kind,
            elapsed_micros: elapsed.as_micros().min(u64::MAX as u128) as u64,
        });
    }
}

pub(crate) struct ContextPreparationGuard(Option<(String, u64)>);

pub(crate) fn context_preparation(agent_id: &str) -> ContextPreparationGuard {
    if !enabled() {
        return ContextPreparationGuard(None);
    }
    let nonce = NEXT_PREPARATION.fetch_add(1, Ordering::Relaxed);
    PREPARATIONS
        .lock()
        .expect("context preparation poisoned")
        .insert(agent_id.to_owned(), (nonce, Instant::now()));
    ContextPreparationGuard(Some((agent_id.to_owned(), nonce)))
}

pub(crate) fn context_request_prepared(agent_id: &str) {
    if !enabled() {
        return;
    }
    let preparation = PREPARATIONS
        .lock()
        .expect("context preparation poisoned")
        .remove(agent_id);
    if let Some((_, started)) = preparation {
        record_timing(TimingKind::ContextPreparationToRequest, started.elapsed());
    }
}

impl Drop for ContextPreparationGuard {
    fn drop(&mut self) {
        if let Some((agent_id, nonce)) = &self.0 {
            let mut preparations = PREPARATIONS.lock().expect("context preparation poisoned");
            if preparations
                .get(agent_id)
                .is_some_and(|(current, _)| current == nonce)
            {
                preparations.remove(agent_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_runtime_connection_configuration() {
        let mut probe = IoProbe::start();
        let directory = tempfile::tempdir().unwrap();
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )
        .unwrap();
        probe.reset();
        let _connection = db.connection().unwrap();
        let report = probe.snapshot();
        assert!(report.counters.runtime_db_connection_opens >= 1);
        // No domain query was requested: the connection's configuration itself
        // executes SQL and must be inside the statement observer's coverage.
        assert!(report.counters.sql_read_statements >= 1);
    }

    #[test]
    fn measures_real_statements_content_and_completed_context_without_payloads() {
        let mut probe = IoProbe::start();
        let connection = Connection::open_in_memory().unwrap();
        observe_sql_connection(&connection).unwrap();
        connection.execute_batch("CREATE TABLE measured (value TEXT); BEGIN; INSERT INTO measured VALUES ('private'); COMMIT;").unwrap();
        let value: String = connection
            .query_row("SELECT value FROM measured", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "private");
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "secret body").unwrap();
        assert_eq!(read_to_string(file.path()).unwrap(), "secret body");
        let _guard = context_preparation("probe-fixture");
        context_request_prepared("probe-fixture");
        let report = probe.snapshot();
        assert!(report.counters.sql_read_statements >= 1);
        assert!(report.counters.sql_write_statements >= 2);
        assert!(report.counters.sql_control_statements >= 2);
        assert!(report.counters.file_read_bytes >= 11);
        assert!(report
            .timing_samples
            .iter()
            .any(|s| s.kind == TimingKind::ContextPreparationToRequest));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("secret body"));
        assert!(!serialized.contains("private"));
        probe.reset();
        let _: String = connection
            .query_row("SELECT value FROM measured", [], |r| r.get(0))
            .unwrap();
        assert!(probe.snapshot().counters.sql_read_statements >= 1);
    }
}
