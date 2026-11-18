//! Process-lifetime RuntimeDb reader-pool aggregation.
//!
//! Observations arrive as bounded role/event pairs from the storage adapter.
//! Nothing here keys on an owner, path, or query text: the only dimensions are
//! the fixed role and the shared fixed latency buckets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::HISTOGRAM_UPPER_BOUNDS_MS;

pub use holon_domain::diagnostics::{
    ReaderPoolBucketSnapshot, ReaderPoolDiagnosticsSnapshot, ReaderPoolDurationSnapshot,
    ReaderPoolEvent, ReaderPoolOutcome, ReaderPoolRole,
};

const ROLE_COUNT: usize = 2;

fn role_index(role: ReaderPoolRole) -> usize {
    match role {
        ReaderPoolRole::Ordinary => 0,
        ReaderPoolRole::Preparation => 1,
    }
}

fn saturating_decrement(value: &AtomicU64, amount: u64) {
    let mut current = value.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match value.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

struct DurationAccumulator {
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    buckets: [AtomicU64; HISTOGRAM_UPPER_BOUNDS_MS.len()],
}

impl DurationAccumulator {
    const fn new() -> Self {
        Self {
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            buckets: [const { AtomicU64::new(0) }; HISTOGRAM_UPPER_BOUNDS_MS.len()],
        }
    }

    fn record(&self, elapsed: Duration) {
        let elapsed_ns = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.max_ns.fetch_max(elapsed_ns, Ordering::Relaxed);
        let bucket = HISTOGRAM_UPPER_BOUNDS_MS.partition_point(|upper_bound| {
            *upper_bound != u64::MAX && u128::from(*upper_bound) * 1_000_000 < elapsed.as_nanos()
        });
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> ReaderPoolDurationSnapshot {
        let mut cumulative = 0;
        let buckets = HISTOGRAM_UPPER_BOUNDS_MS
            .iter()
            .enumerate()
            .map(|(index, upper_bound)| {
                cumulative += self.buckets[index].load(Ordering::Relaxed);
                ReaderPoolBucketSnapshot {
                    upper_bound_ms: (*upper_bound != u64::MAX).then_some(*upper_bound),
                    count: cumulative,
                }
            })
            .collect();
        ReaderPoolDurationSnapshot {
            // Use the same bucket sample even when recording races a scrape.
            count: cumulative,
            total_ns: self.total_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            buckets,
        }
    }
}

struct RoleAccumulator {
    role: ReaderPoolRole,
    instances: AtomicU64,
    configured_capacity: AtomicU64,
    connections: AtomicU64,
    in_use: AtomicU64,
    waiting: AtomicU64,
    instances_created: AtomicU64,
    instances_dropped: AtomicU64,
    connections_opened: AtomicU64,
    connections_closed: AtomicU64,
    checkouts_started: AtomicU64,
    checkouts_succeeded: AtomicU64,
    checkouts_timed_out: AtomicU64,
    checkouts_cancelled: AtomicU64,
    checkouts_failed: AtomicU64,
    checkout_duration: DurationAccumulator,
    leases_started: AtomicU64,
    leases_finished: AtomicU64,
    lease_duration: DurationAccumulator,
    connects_started: AtomicU64,
    connects_succeeded: AtomicU64,
    connects_failed: AtomicU64,
    connect_duration: DurationAccumulator,
}

impl RoleAccumulator {
    const fn new(role: ReaderPoolRole) -> Self {
        Self {
            role,
            instances: AtomicU64::new(0),
            configured_capacity: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            in_use: AtomicU64::new(0),
            waiting: AtomicU64::new(0),
            instances_created: AtomicU64::new(0),
            instances_dropped: AtomicU64::new(0),
            connections_opened: AtomicU64::new(0),
            connections_closed: AtomicU64::new(0),
            checkouts_started: AtomicU64::new(0),
            checkouts_succeeded: AtomicU64::new(0),
            checkouts_timed_out: AtomicU64::new(0),
            checkouts_cancelled: AtomicU64::new(0),
            checkouts_failed: AtomicU64::new(0),
            checkout_duration: DurationAccumulator::new(),
            leases_started: AtomicU64::new(0),
            leases_finished: AtomicU64::new(0),
            lease_duration: DurationAccumulator::new(),
            connects_started: AtomicU64::new(0),
            connects_succeeded: AtomicU64::new(0),
            connects_failed: AtomicU64::new(0),
            connect_duration: DurationAccumulator::new(),
        }
    }

    fn record(&self, event: ReaderPoolEvent) {
        match event {
            ReaderPoolEvent::Created { capacity } => {
                self.instances.fetch_add(1, Ordering::Relaxed);
                self.configured_capacity
                    .fetch_add(u64::from(capacity), Ordering::Relaxed);
                self.instances_created.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::Dropped { capacity } => {
                saturating_decrement(&self.instances, 1);
                saturating_decrement(&self.configured_capacity, u64::from(capacity));
                self.instances_dropped.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::ConnectionOpened => {
                self.connections.fetch_add(1, Ordering::Relaxed);
                self.connections_opened.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::ConnectionClosed => {
                saturating_decrement(&self.connections, 1);
                self.connections_closed.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::CheckoutStarted => {
                self.waiting.fetch_add(1, Ordering::Relaxed);
                self.checkouts_started.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::CheckoutFinished { elapsed, outcome } => {
                saturating_decrement(&self.waiting, 1);
                let counter = match outcome {
                    ReaderPoolOutcome::Success => &self.checkouts_succeeded,
                    ReaderPoolOutcome::Timeout => &self.checkouts_timed_out,
                    ReaderPoolOutcome::Cancelled => &self.checkouts_cancelled,
                    ReaderPoolOutcome::Failed => &self.checkouts_failed,
                };
                counter.fetch_add(1, Ordering::Relaxed);
                self.checkout_duration.record(elapsed);
            }
            ReaderPoolEvent::LeaseStarted => {
                self.in_use.fetch_add(1, Ordering::Relaxed);
                self.leases_started.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::LeaseFinished { elapsed } => {
                saturating_decrement(&self.in_use, 1);
                self.leases_finished.fetch_add(1, Ordering::Relaxed);
                self.lease_duration.record(elapsed);
            }
            ReaderPoolEvent::ConnectStarted => {
                self.connects_started.fetch_add(1, Ordering::Relaxed);
            }
            ReaderPoolEvent::ConnectFinished { elapsed, success } => {
                if success {
                    self.connects_succeeded.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.connects_failed.fetch_add(1, Ordering::Relaxed);
                }
                self.connect_duration.record(elapsed);
            }
        }
    }

    fn snapshot(&self) -> ReaderPoolDiagnosticsSnapshot {
        let connections = self.connections.load(Ordering::Relaxed);
        let in_use = self.in_use.load(Ordering::Relaxed);
        ReaderPoolDiagnosticsSnapshot {
            role: self.role,
            instances: self.instances.load(Ordering::Relaxed),
            configured_capacity: self.configured_capacity.load(Ordering::Relaxed),
            connections,
            in_use,
            idle: connections.saturating_sub(in_use),
            waiting: self.waiting.load(Ordering::Relaxed),
            instances_created: self.instances_created.load(Ordering::Relaxed),
            instances_dropped: self.instances_dropped.load(Ordering::Relaxed),
            connections_opened: self.connections_opened.load(Ordering::Relaxed),
            connections_closed: self.connections_closed.load(Ordering::Relaxed),
            checkouts_started: self.checkouts_started.load(Ordering::Relaxed),
            checkouts_succeeded: self.checkouts_succeeded.load(Ordering::Relaxed),
            checkouts_timed_out: self.checkouts_timed_out.load(Ordering::Relaxed),
            checkouts_cancelled: self.checkouts_cancelled.load(Ordering::Relaxed),
            checkouts_failed: self.checkouts_failed.load(Ordering::Relaxed),
            checkout_duration: self.checkout_duration.snapshot(),
            leases_started: self.leases_started.load(Ordering::Relaxed),
            leases_finished: self.leases_finished.load(Ordering::Relaxed),
            lease_duration: self.lease_duration.snapshot(),
            connects_started: self.connects_started.load(Ordering::Relaxed),
            connects_succeeded: self.connects_succeeded.load(Ordering::Relaxed),
            connects_failed: self.connects_failed.load(Ordering::Relaxed),
            connect_duration: self.connect_duration.snapshot(),
        }
    }
}

/// Isolated, disposable reader-pool accumulator.
///
/// The process-wide instance collects production observations and is the only
/// source of the exported snapshot. Tests build their own instance so they
/// never observe unrelated global state; recording into a local instance is
/// not exported anywhere.
pub struct ReaderPoolAccumulator {
    roles: [RoleAccumulator; ROLE_COUNT],
}

impl ReaderPoolAccumulator {
    pub const fn new() -> Self {
        Self {
            roles: [
                RoleAccumulator::new(ReaderPoolRole::Ordinary),
                RoleAccumulator::new(ReaderPoolRole::Preparation),
            ],
        }
    }

    pub fn record(&self, role: ReaderPoolRole, event: ReaderPoolEvent) {
        self.roles[role_index(role)].record(event);
    }

    /// Snapshot both fixed roles in stable order, even when no pool exists.
    pub fn snapshot(&self) -> Vec<ReaderPoolDiagnosticsSnapshot> {
        self.roles.iter().map(RoleAccumulator::snapshot).collect()
    }
}

impl Default for ReaderPoolAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

static PROCESS_ACCUMULATOR: ReaderPoolAccumulator = ReaderPoolAccumulator::new();

/// Observe one reader-pool lifecycle event for a fixed public role.
pub fn record_reader_pool_event(role: ReaderPoolRole, event: ReaderPoolEvent) {
    PROCESS_ACCUMULATOR.record(role, event);
}

/// Aggregated reader-pool diagnostics for every RuntimeDb owner recorded in
/// this process.
pub fn snapshot() -> Vec<ReaderPoolDiagnosticsSnapshot> {
    PROCESS_ACCUMULATOR.snapshot()
}

#[cfg(test)]
mod tests;
