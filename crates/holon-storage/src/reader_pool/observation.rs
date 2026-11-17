//! Bounded reader-pool lifecycle observations.
//!
//! Each helper emits one start event and exactly one terminal event through the
//! storage diagnostics hook. Observations carry only the fixed pool role, never
//! an owner, path, SQL text, or business value.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::diagnostics::{ReaderPoolEvent, ReaderPoolOutcome, ReaderPoolRole, StorageDiagnostics};

/// Waiting time for one read entry to acquire a lease.
///
/// Clones share one linearization point, so the async entry can classify an
/// aborted queued read from the caller side while the blocking task still owns
/// a clone. Exactly one `CheckoutFinished` is emitted; an observation dropped
/// without an explicit outcome is reported as `Failed` rather than guessed.
#[derive(Clone)]
pub(super) struct CheckoutObservation {
    state: Arc<CheckoutState>,
}

struct CheckoutState {
    diagnostics: &'static StorageDiagnostics,
    role: ReaderPoolRole,
    started: Instant,
    finished: AtomicBool,
}

impl CheckoutObservation {
    pub(super) fn start(diagnostics: &'static StorageDiagnostics, role: ReaderPoolRole) -> Self {
        (diagnostics.reader_pool)(role, ReaderPoolEvent::CheckoutStarted);
        Self {
            state: Arc::new(CheckoutState {
                diagnostics,
                role,
                started: Instant::now(),
                finished: AtomicBool::new(false),
            }),
        }
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.state.started.elapsed()
    }

    pub(super) fn finish(&self, outcome: ReaderPoolOutcome) {
        if self
            .state
            .finished
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        (self.state.diagnostics.reader_pool)(
            self.state.role,
            ReaderPoolEvent::CheckoutFinished {
                elapsed: self.state.started.elapsed(),
                outcome,
            },
        );
    }
}

impl Drop for CheckoutObservation {
    fn drop(&mut self) {
        self.finish(ReaderPoolOutcome::Failed);
    }
}

/// One `manager.connect` attempt.
pub(super) struct ConnectObservation {
    diagnostics: &'static StorageDiagnostics,
    role: ReaderPoolRole,
    started: Instant,
    finished: bool,
}

impl ConnectObservation {
    pub(super) fn start(diagnostics: &'static StorageDiagnostics, role: ReaderPoolRole) -> Self {
        (diagnostics.reader_pool)(role, ReaderPoolEvent::ConnectStarted);
        Self {
            diagnostics,
            role,
            started: Instant::now(),
            finished: false,
        }
    }

    pub(super) fn finish(&mut self, success: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        (self.diagnostics.reader_pool)(
            self.role,
            ReaderPoolEvent::ConnectFinished {
                elapsed: self.started.elapsed(),
                success,
            },
        );
    }
}

impl Drop for ConnectObservation {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// One application-owned lease, from handoff to release.
pub(super) struct LeaseObservation {
    diagnostics: &'static StorageDiagnostics,
    role: ReaderPoolRole,
    started: Instant,
    finished: bool,
}

impl LeaseObservation {
    pub(super) fn start(diagnostics: &'static StorageDiagnostics, role: ReaderPoolRole) -> Self {
        (diagnostics.reader_pool)(role, ReaderPoolEvent::LeaseStarted);
        Self {
            diagnostics,
            role,
            started: Instant::now(),
            finished: false,
        }
    }
}

impl Drop for LeaseObservation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        (self.diagnostics.reader_pool)(
            self.role,
            ReaderPoolEvent::LeaseFinished {
                elapsed: self.started.elapsed(),
            },
        );
    }
}
