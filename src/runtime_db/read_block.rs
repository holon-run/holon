//! Bounded, observable runtime DB read blocks.
//!
//! Polling HTTP handlers must not run synchronous SQLite reads on a tokio
//! worker thread: one slow read would otherwise occupy a worker until the
//! storage layer answers. [`run_blocking`] moves the whole read block onto the
//! blocking pool and gives it an absolute read deadline, so a slow statement can
//! cost one request instead of the process.
//!
//! The same block is the reporting unit for slow reads:
//!
//! - a read block that outlives `HOLON_SLOW_DB_READ_MS` reports its call site,
//!   checkout wait and execution time;
//! - a SQLite statement that outlives the same threshold reports the SQL text.
//!
//! `rusqlite` profile callbacks receive no user context, so the call site and
//! HTTP read label travel through a thread-local read context that the blocking
//! read sets for the duration of the block.

use std::cell::RefCell;
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::Connection;

use super::RuntimeDbReadScope;

/// Slow read reporting is off for `0` and defaults to one second.
const DEFAULT_SLOW_READ_MS: u64 = 1_000;
const ENV_SLOW_READ_MS: &str = "HOLON_SLOW_DB_READ_MS";
const UNINITIALIZED_MS: u64 = u64::MAX;
const MAX_LOGGED_SQL_CHARS: usize = 512;

static SLOW_READ_MS: AtomicU64 = AtomicU64::new(UNINITIALIZED_MS);

/// Static call site of a pooled read. Kept cheap: every reader checkout carries
/// one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadOrigin {
    file: &'static str,
    line: u32,
}

impl ReadOrigin {
    pub(crate) const UNKNOWN: Self = Self {
        file: "unknown",
        line: 0,
    };

    pub(crate) fn from_location(location: &'static Location<'static>) -> Self {
        Self {
            file: location.file(),
            line: location.line(),
        }
    }
}

#[derive(Clone, Copy)]
struct ReadContext {
    label: Option<&'static str>,
    origin: Option<ReadOrigin>,
}

thread_local! {
    static READ_CONTEXT: RefCell<Option<ReadContext>> = const { RefCell::new(None) };
    static READ_SCOPE: RefCell<Option<RuntimeDbReadScope>> = const { RefCell::new(None) };
}

/// Sets the label and call site reported by slow reads on this thread.
pub(crate) struct ReadContextGuard {
    previous: Option<ReadContext>,
}

impl ReadContextGuard {
    /// Label the enclosing blocking read block, for example `http.events.page`.
    pub(crate) fn enter_label(label: &'static str) -> Self {
        Self::enter(ReadContext {
            label: Some(label),
            origin: None,
        })
    }

    /// Attribute nested reads to their own call site while keeping the label of
    /// the enclosing block.
    pub(crate) fn enter_origin(origin: ReadOrigin) -> Self {
        Self::enter(ReadContext {
            label: None,
            origin: Some(origin),
        })
    }

    fn enter(context: ReadContext) -> Self {
        READ_CONTEXT.with(|slot| {
            let previous = *slot.borrow();
            let merged = ReadContext {
                label: context
                    .label
                    .or_else(|| previous.and_then(|value| value.label)),
                origin: context
                    .origin
                    .or_else(|| previous.and_then(|value| value.origin)),
            };
            *slot.borrow_mut() = Some(merged);
            Self { previous }
        })
    }
}

impl Drop for ReadContextGuard {
    fn drop(&mut self) {
        READ_CONTEXT.with(|slot| *slot.borrow_mut() = self.previous);
    }
}

/// Read deadline of the enclosing blocking read block, if any. Explicitly
/// scoped database handles take precedence over this ambient scope.
pub(crate) fn current_scope() -> Option<RuntimeDbReadScope> {
    READ_SCOPE.with(|slot| slot.borrow().clone())
}

struct ReadScopeGuard {
    previous: Option<RuntimeDbReadScope>,
}

impl ReadScopeGuard {
    fn enter(scope: RuntimeDbReadScope) -> Self {
        READ_SCOPE.with(|slot| {
            let previous = slot.borrow().clone();
            *slot.borrow_mut() = Some(scope);
            Self { previous }
        })
    }
}

impl Drop for ReadScopeGuard {
    fn drop(&mut self) {
        READ_SCOPE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}

/// Scope nested synchronous reads inside an already registered blocking worker.
pub(crate) fn with_scope<T>(scope: RuntimeDbReadScope, work: impl FnOnce() -> T) -> T {
    let _scope = ReadScopeGuard::enter(scope);
    work()
}

fn current_context() -> Option<ReadContext> {
    READ_CONTEXT.with(|slot| *slot.borrow())
}

/// Runs a synchronous read block off the async worker threads under one read
/// deadline. SQL execution is cancelled at the deadline through the existing
/// scoped-read progress hook; the block itself still finishes so its reader
/// connection is released before the caller sees the result.
pub(crate) async fn run_blocking<T, F>(label: &'static str, budget: Duration, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + budget;
        let _context = ReadContextGuard::enter_label(label);
        let _scope = ReadScopeGuard::enter(RuntimeDbReadScope::new(deadline));
        work()
    })
    .await?
}

/// Reports a read block above the slow threshold with its call site and split
/// between pool checkout and execution.
pub(crate) fn record_read_block(origin: ReadOrigin, checkout: Duration, operation: Duration) {
    let Some(threshold) = slow_read_threshold() else {
        return;
    };
    let elapsed = checkout.saturating_add(operation);
    if elapsed < threshold {
        return;
    }
    let label = current_label();
    tracing::warn!(
        target: "holon::performance",
        read = label.unwrap_or("unlabelled"),
        origin_file = origin.file,
        origin_line = origin.line,
        checkout_ms = milliseconds(checkout),
        operation_ms = milliseconds(operation),
        "slow runtime db read"
    );
}

fn current_label() -> Option<&'static str> {
    current_context().and_then(|context| context.label)
}

/// Installs the statement profiler on one pooled reader connection. Slow
/// statements are attributed to the enclosing read block.
pub(crate) fn install_statement_profile(connection: &mut Connection) -> Result<()> {
    connection.profile(Some(profile_statement));
    Ok(())
}

fn profile_statement(sql: &str, elapsed: Duration) {
    let Some(threshold) = slow_read_threshold() else {
        return;
    };
    if elapsed < threshold {
        return;
    }
    let context = current_context();
    let origin = context
        .and_then(|value| value.origin)
        .unwrap_or(ReadOrigin::UNKNOWN);
    tracing::warn!(
        target: "holon::performance",
        sql = %summarize_sql(sql),
        read = context.and_then(|value| value.label).unwrap_or("unlabelled"),
        origin_file = origin.file,
        origin_line = origin.line,
        elapsed_ms = milliseconds(elapsed),
        "slow sqlite statement"
    );
}

fn collapse_sql_whitespace(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summarize_sql(sql: &str) -> String {
    let collapsed = collapse_sql_whitespace(sql);
    if collapsed.chars().count() <= MAX_LOGGED_SQL_CHARS {
        return collapsed;
    }
    let mut truncated = collapsed
        .chars()
        .take(MAX_LOGGED_SQL_CHARS)
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn slow_read_threshold() -> Option<Duration> {
    let mut configured = SLOW_READ_MS.load(Ordering::Relaxed);
    if configured == UNINITIALIZED_MS {
        configured = std::env::var(ENV_SLOW_READ_MS)
            .ok()
            .as_deref()
            .map(|raw| parse_slow_read_ms(Some(raw)))
            .unwrap_or(parse_slow_read_ms(None));
        SLOW_READ_MS.store(configured, Ordering::Relaxed);
    }
    match configured {
        0 => None,
        value => Some(Duration::from_millis(value)),
    }
}

/// `None` (unset) and invalid values fall back to the default; `0` disables
/// slow read reporting.
fn parse_slow_read_ms(raw: Option<&str>) -> u64 {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SLOW_READ_MS)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, MutexGuard};

    use super::*;

    static THRESHOLD_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct ThresholdGuard {
        previous: u64,
    }

    impl Drop for ThresholdGuard {
        fn drop(&mut self) {
            SLOW_READ_MS.store(self.previous, Ordering::Relaxed);
        }
    }

    fn override_threshold(ms: u64) -> (MutexGuard<'static, ()>, ThresholdGuard) {
        let lock = THRESHOLD_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = SLOW_READ_MS.load(Ordering::Relaxed);
        SLOW_READ_MS.store(ms, Ordering::Relaxed);
        (lock, ThresholdGuard { previous })
    }

    fn capture_logs(f: impl FnOnce()) -> String {
        #[derive(Clone)]
        struct LogWriter(Arc<Mutex<Vec<u8>>>);

        impl io::Write for LogWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let writer_output = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || LogWriter(writer_output.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = output.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn slow_read_threshold_parsing_defaults_and_disables() {
        assert_eq!(parse_slow_read_ms(None), DEFAULT_SLOW_READ_MS);
        assert_eq!(parse_slow_read_ms(Some("")), DEFAULT_SLOW_READ_MS);
        assert_eq!(
            parse_slow_read_ms(Some("not-a-number")),
            DEFAULT_SLOW_READ_MS
        );
        assert_eq!(parse_slow_read_ms(Some(" 250 ")), 250);
        assert_eq!(parse_slow_read_ms(Some("0")), 0);
    }

    #[test]
    fn sql_summary_collapses_whitespace_and_truncates() {
        assert_eq!(
            summarize_sql("SELECT   data_json\n  FROM audit_events\tLIMIT ?1"),
            "SELECT data_json FROM audit_events LIMIT ?1"
        );
        let summary = summarize_sql(&"x".repeat(MAX_LOGGED_SQL_CHARS + 32));
        assert_eq!(summary.chars().count(), MAX_LOGGED_SQL_CHARS + 1);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn slow_read_block_reports_call_site_and_split() {
        let (_lock, _threshold) = override_threshold(100);
        let logs = capture_logs(|| {
            record_read_block(
                ReadOrigin {
                    file: "src/runtime_db/repositories.rs",
                    line: 5085,
                },
                Duration::from_millis(20),
                Duration::from_millis(1_500),
            );
        });
        assert!(logs.contains("slow runtime db read"), "{logs}");
        assert!(
            logs.contains(r#"origin_file="src/runtime_db/repositories.rs""#),
            "{logs}"
        );
        assert!(logs.contains("origin_line=5085"), "{logs}");
        assert!(logs.contains("checkout_ms=20"), "{logs}");
        assert!(logs.contains("operation_ms=1500"), "{logs}");
    }

    #[test]
    fn slow_sql_statement_reports_read_label_sql_and_call_site() {
        let (_lock, _threshold) = override_threshold(100);
        let logs = capture_logs(|| {
            let _label = ReadContextGuard::enter_label("http.events.page");
            let _origin = ReadContextGuard::enter_origin(ReadOrigin {
                file: "src/runtime_db/repositories.rs",
                line: 5085,
            });
            profile_statement(
                "SELECT   data_json\nFROM audit_events",
                Duration::from_millis(1_500),
            );
        });
        assert!(logs.contains("slow sqlite statement"), "{logs}");
        assert!(
            logs.contains("sql=SELECT data_json FROM audit_events"),
            "{logs}"
        );
        assert!(logs.contains(r#"read="http.events.page""#), "{logs}");
        assert!(logs.contains("origin_line=5085"), "{logs}");
        assert!(logs.contains("elapsed_ms=1500"), "{logs}");
    }

    #[test]
    fn reads_below_the_threshold_stay_silent() {
        let (_lock, _threshold) = override_threshold(1_000);
        let logs = capture_logs(|| {
            profile_statement("SELECT 1", Duration::from_millis(10));
            record_read_block(
                ReadOrigin::UNKNOWN,
                Duration::from_millis(1),
                Duration::from_millis(10),
            );
        });
        assert!(!logs.contains("slow sqlite statement"), "{logs}");
        assert!(!logs.contains("slow runtime db read"), "{logs}");
    }

    #[test]
    fn read_context_nests_label_and_origin() {
        let _label = ReadContextGuard::enter_label("http.tasks.list");
        assert_eq!(current_label(), Some("http.tasks.list"));
        {
            let _origin = ReadContextGuard::enter_origin(ReadOrigin {
                file: "src/a.rs",
                line: 7,
            });
            let context = current_context().expect("context");
            assert_eq!(context.label, Some("http.tasks.list"));
            assert_eq!(context.origin.map(|origin| origin.line), Some(7));
        }
        let context = current_context().expect("context");
        assert_eq!(context.label, Some("http.tasks.list"));
        assert!(context.origin.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_read_leaves_the_async_worker_free() {
        let started = Instant::now();
        let read = run_blocking("test.off_worker", Duration::from_secs(30), || {
            std::thread::sleep(Duration::from_millis(150));
            Ok(())
        });
        let mut read = std::pin::pin!(read);
        let timer = tokio::time::sleep(Duration::from_millis(20));
        tokio::pin!(timer);
        // On a single-threaded runtime this timer can only fire while the read
        // sleeps on the blocking pool.
        tokio::select! {
            () = &mut timer => {}
            result = &mut read => panic!("blocking read completed inline: {result:?}"),
        }
        read.await.expect("blocking read");
        assert!(started.elapsed() >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn blocking_read_scopes_only_its_own_thread() {
        let observed = run_blocking("test.scope", Duration::from_secs(30), || {
            Ok(current_scope().map(|scope| scope.deadline()))
        })
        .await
        .expect("blocking read")
        .expect("read scope");
        assert!(observed > Instant::now());
        assert!(current_scope().is_none(), "scope leaked to the caller");
    }
}
