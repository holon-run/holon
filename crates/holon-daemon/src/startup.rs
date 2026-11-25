//! Startup timing only: these observations never gate or schedule recovery.
use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

#[cfg(any(test, feature = "test-support"))]
use std::pin::Pin;

use anyhow::Result;

#[cfg(any(test, feature = "test-support"))]
pub type StartupTestHook = dyn Fn(StartupObservation, &'static str) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct StartupObservation {
    pub startup_id: String,
    started_at: chrono::DateTime<chrono::Utc>,
    pub agent_id: Option<String>,
    pub generation: Option<u64>,
    #[cfg(any(test, feature = "test-support"))]
    test_hook: Option<Arc<StartupTestHook>>,
}

impl std::fmt::Debug for StartupObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupObservation")
            .field("startup_id", &self.startup_id)
            .field("agent_id", &self.agent_id)
            .field("generation", &self.generation)
            .finish()
    }
}

impl Default for StartupObservation {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupObservation {
    pub fn new() -> Self {
        Self::with_id(holon_domain::ids::runtime_id("startup"))
    }

    pub fn with_id(startup_id: String) -> Self {
        Self {
            startup_id,
            started_at: chrono::Utc::now(),
            agent_id: None,
            generation: None,
            #[cfg(any(test, feature = "test-support"))]
            test_hook: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_hook(mut self, hook: Arc<StartupTestHook>) -> Self {
        self.test_hook = Some(hook);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_hook(&self) -> Option<&StartupTestHook> {
        self.test_hook.as_deref()
    }

    pub fn started_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_at
    }

    pub fn for_agent(&self, agent_id: &str, generation: Option<u64>) -> Self {
        Self {
            startup_id: self.startup_id.clone(),
            started_at: self.started_at,
            agent_id: Some(agent_id.to_string()),
            generation,
            #[cfg(any(test, feature = "test-support"))]
            test_hook: self.test_hook.clone(),
        }
    }

    pub fn phase(&self, phase: &'static str) -> StartupPhase {
        let observation = StartupPhase {
            observation: self.clone(),
            phase,
            started: Instant::now(),
            finished: false,
            counters: None,
            children: None,
            parent: None,
        };
        observation.emit("started", "pending");
        observation
    }

    /// A parent phase that aggregates named child phases and reports the
    /// residual (unattributed) time when it finishes.
    pub fn scope(&self, phase: &'static str) -> StartupPhase {
        let observation = StartupPhase {
            observation: self.clone(),
            phase,
            started: Instant::now(),
            finished: false,
            counters: None,
            children: Some(Arc::new(PhaseAggregate::default())),
            parent: None,
        };
        observation.emit("started", "pending");
        observation
    }

    pub fn observe_sync<T>(&self, phase: &'static str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let observation = self.phase(phase);
        let result = f();
        observation.finish(&result);
        result
    }

    pub async fn observe<T>(
        &self,
        phase: &'static str,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let observation = self.phase(phase);
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = self.test_hook() {
            if let Err(error) = hook(self.clone(), phase).await {
                let result = Err(error);
                observation.finish(&result);
                return result;
            }
        }
        let result = future.await;
        observation.finish(&result);
        result
    }
}

/// Bounded, observation-only counters for a startup phase. They never gate or
/// schedule recovery; they only make a phase's cost attributable.
#[derive(Default)]
pub struct StartupPhaseCounters {
    rows_scanned: AtomicU64,
    rows_decoded: AtomicU64,
    bytes_decoded: AtomicU64,
    transactions: AtomicU64,
    write_tx_count: AtomicU64,
    max_write_tx_ms: AtomicU64,
    candidates: AtomicU64,
    batches: AtomicU64,
    skipped: AtomicU64,
    failed: AtomicU64,
}

impl StartupPhaseCounters {
    pub fn snapshot(&self) -> StartupPhaseCounterSnapshot {
        StartupPhaseCounterSnapshot {
            rows_scanned: self.rows_scanned.load(Ordering::Relaxed),
            rows_decoded: self.rows_decoded.load(Ordering::Relaxed),
            bytes_decoded: self.bytes_decoded.load(Ordering::Relaxed),
            transactions: self.transactions.load(Ordering::Relaxed),
            write_tx_count: self.write_tx_count.load(Ordering::Relaxed),
            max_write_tx_ms: self.max_write_tx_ms.load(Ordering::Relaxed),
            candidates: self.candidates.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    pub fn add_rows_scanned(&self, rows: u64) {
        self.rows_scanned.fetch_add(rows, Ordering::Relaxed);
    }

    pub fn add_rows_decoded(&self, rows: u64) {
        self.rows_decoded.fetch_add(rows, Ordering::Relaxed);
    }

    pub fn add_bytes_decoded(&self, bytes: u64) {
        self.bytes_decoded.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_transactions(&self, count: u64) {
        self.transactions.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_candidates(&self, count: u64) {
        self.candidates.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_batches(&self, count: u64) {
        self.batches.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_skipped(&self, count: u64) {
        self.skipped.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_failed(&self, count: u64) {
        self.failed.fetch_add(count, Ordering::Relaxed);
    }

    /// Count one write transaction and fold its held duration into the max.
    pub fn record_write_transaction(&self, held_ms: u64) {
        self.transactions.fetch_add(1, Ordering::Relaxed);
        self.write_tx_count.fetch_add(1, Ordering::Relaxed);
        self.max_write_tx_ms.fetch_max(held_ms, Ordering::Relaxed);
    }

    fn merge_from(&self, other: &Self) {
        macro_rules! merge_add {
            ($($field:ident),+ $(,)?) => {
                $(
                    self.$field
                        .fetch_add(other.$field.load(Ordering::Relaxed), Ordering::Relaxed);
                )+
            };
        }
        merge_add!(
            rows_scanned,
            rows_decoded,
            bytes_decoded,
            transactions,
            write_tx_count,
            candidates,
            batches,
            skipped,
            failed,
        );
        self.max_write_tx_ms.fetch_max(
            other.max_write_tx_ms.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }
}

/// Read-only copy of a phase's bounded counters for tests and future sinks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartupPhaseCounterSnapshot {
    pub rows_scanned: u64,
    pub rows_decoded: u64,
    pub bytes_decoded: u64,
    pub transactions: u64,
    pub write_tx_count: u64,
    pub max_write_tx_ms: u64,
    pub candidates: u64,
    pub batches: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Elapsed attribution and bounded counters for a finished phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartupPhaseSummary {
    pub elapsed_ms: u64,
    /// Sum of named child elapsed times for a scope phase; zero otherwise.
    pub attributed_ms: u64,
    pub counters: StartupPhaseCounterSnapshot,
}

#[derive(Default)]
struct PhaseAggregate {
    elapsed_us: AtomicU64,
    counters: StartupPhaseCounters,
}

fn emit_phase(
    observation: &StartupObservation,
    phase: &'static str,
    started: Instant,
    state: &str,
    outcome: &str,
) {
    tracing::info!(
        target: "holon::startup",
        startup_id = %observation.startup_id,
        agent_id = observation.agent_id.as_deref(),
        generation = observation.generation,
        phase,
        state,
        outcome,
        elapsed_ms = started.elapsed().as_millis() as u64,
        error_class = if outcome == "error" { phase } else { "" },
        "startup phase"
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_counters(
    observation: &StartupObservation,
    phase: &'static str,
    elapsed_ms: u64,
    attributed_ms: u64,
    unattributed_ms: Option<u64>,
    counters: &StartupPhaseCounters,
) {
    tracing::info!(
        target: "holon::startup",
        startup_id = %observation.startup_id,
        agent_id = observation.agent_id.as_deref(),
        generation = observation.generation,
        phase,
        state = "counters",
        elapsed_ms,
        attributed_ms,
        unattributed_ms,
        rows_scanned = counters.rows_scanned.load(Ordering::Relaxed),
        rows_decoded = counters.rows_decoded.load(Ordering::Relaxed),
        bytes_decoded = counters.bytes_decoded.load(Ordering::Relaxed),
        transactions = counters.transactions.load(Ordering::Relaxed),
        write_tx_count = counters.write_tx_count.load(Ordering::Relaxed),
        max_write_tx_ms = counters.max_write_tx_ms.load(Ordering::Relaxed),
        candidates = counters.candidates.load(Ordering::Relaxed),
        batches = counters.batches.load(Ordering::Relaxed),
        skipped = counters.skipped.load(Ordering::Relaxed),
        failed = counters.failed.load(Ordering::Relaxed),
        "startup phase counters"
    );
}

/// A named child of a scope phase. Elapsed time and counters are folded into the
/// parent scope on finish, or on drop if an early return skips the explicit end.
pub struct StartupChildPhase {
    observation: StartupObservation,
    phase: &'static str,
    started: Instant,
    finished: bool,
    counters: StartupPhaseCounters,
    parent: Arc<PhaseAggregate>,
}

impl StartupChildPhase {
    pub fn counters(&self) -> &StartupPhaseCounters {
        &self.counters
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn finish<T>(mut self, result: &Result<T>) {
        self.emit("finished", if result.is_ok() { "ok" } else { "error" });
        self.finished = true;
        self.fold_into_parent();
    }

    fn emit(&self, state: &str, outcome: &str) {
        emit_phase(&self.observation, self.phase, self.started, state, outcome);
    }

    fn fold_into_parent(&self) {
        let elapsed_us = self.started.elapsed().as_micros() as u64;
        self.parent
            .elapsed_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        self.parent.counters.merge_from(&self.counters);
    }
}

impl Drop for StartupChildPhase {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("finished", "incomplete");
            self.fold_into_parent();
        }
    }
}

pub struct StartupPhase {
    observation: StartupObservation,
    phase: &'static str,
    started: Instant,
    finished: bool,
    counters: Option<StartupPhaseCounters>,
    children: Option<Arc<PhaseAggregate>>,
    parent: Option<Arc<PhaseAggregate>>,
}

impl StartupPhase {
    pub fn finish<T>(mut self, result: &Result<T>) {
        self.emit("finished", if result.is_ok() { "ok" } else { "error" });
        self.finished = true;
        self.fold();
    }

    /// Finish the phase and return its elapsed, attributed, and counter
    /// summary. Callers that only report use [`StartupPhase::finish`].
    pub fn finish_with_summary<T>(mut self, result: &Result<T>) -> StartupPhaseSummary {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let summary = self.summary(elapsed_ms);
        self.emit("finished", if result.is_ok() { "ok" } else { "error" });
        self.finished = true;
        self.fold();
        summary
    }

    fn summary(&self, elapsed_ms: u64) -> StartupPhaseSummary {
        let (attributed_ms, counters) = match &self.children {
            Some(children) => {
                let total = StartupPhaseCounters::default();
                total.merge_from(&children.counters);
                if let Some(own) = &self.counters {
                    total.merge_from(own);
                }
                (
                    children.elapsed_us.load(Ordering::Relaxed) / 1000,
                    total.snapshot(),
                )
            }
            None => (
                0,
                self.counters
                    .as_ref()
                    .map(StartupPhaseCounters::snapshot)
                    .unwrap_or_default(),
            ),
        };
        StartupPhaseSummary {
            elapsed_ms,
            attributed_ms,
            counters,
        }
    }

    /// Create a named child phase; the parent must have been created with
    /// [`StartupObservation::scope`].
    pub fn child(&self, phase: &'static str) -> StartupChildPhase {
        let parent = self
            .children
            .as_ref()
            .expect("startup child phase requires a scope phase")
            .clone();
        let child = StartupChildPhase {
            observation: self.observation.clone(),
            phase,
            started: Instant::now(),
            finished: false,
            counters: StartupPhaseCounters::default(),
            parent,
        };
        child.emit("started", "pending");
        child
    }

    fn fold(&self) {
        let elapsed_us = self.started.elapsed().as_micros() as u64;
        if let Some(parent) = &self.parent {
            parent.elapsed_us.fetch_add(elapsed_us, Ordering::Relaxed);
        }
        match (&self.children, &self.counters) {
            (Some(children), own) => {
                let attributed_ms = children.elapsed_us.load(Ordering::Relaxed) / 1000;
                let elapsed_ms = elapsed_us / 1000;
                let total = StartupPhaseCounters::default();
                total.merge_from(&children.counters);
                if let Some(own) = own {
                    total.merge_from(own);
                }
                emit_counters(
                    &self.observation,
                    self.phase,
                    elapsed_ms,
                    attributed_ms,
                    Some(elapsed_ms.saturating_sub(attributed_ms)),
                    &total,
                );
                if let Some(parent) = &self.parent {
                    parent.counters.merge_from(&total);
                }
            }
            (None, Some(counters)) => {
                let elapsed_ms = elapsed_us / 1000;
                emit_counters(
                    &self.observation,
                    self.phase,
                    elapsed_ms,
                    elapsed_ms,
                    None,
                    counters,
                );
                if let Some(parent) = &self.parent {
                    parent.counters.merge_from(counters);
                }
            }
            (None, None) => {}
        }
    }

    fn emit(&self, state: &str, outcome: &str) {
        emit_phase(&self.observation, self.phase, self.started, state, outcome);
    }
}

impl Drop for StartupPhase {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("finished", "incomplete");
        }
    }
}

#[cfg(test)]
mod tests;
