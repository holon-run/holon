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
pub struct ReadOrigin {
    file: &'static str,
    line: u32,
}

impl ReadOrigin {
    pub const UNKNOWN: Self = Self {
        file: "unknown",
        line: 0,
    };

    pub fn from_location(location: &'static Location<'static>) -> Self {
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
    fn enter_label(label: &'static str) -> Self {
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
pub fn current_scope() -> Option<RuntimeDbReadScope> {
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
pub fn with_scope<T>(scope: RuntimeDbReadScope, work: impl FnOnce() -> T) -> T {
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
pub async fn run_blocking<T, F>(label: &'static str, budget: Duration, work: F) -> Result<T>
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
mod tests;
