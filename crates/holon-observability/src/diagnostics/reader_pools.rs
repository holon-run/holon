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
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn role_snapshot(
        accumulator: &ReaderPoolAccumulator,
        role: ReaderPoolRole,
    ) -> ReaderPoolDiagnosticsSnapshot {
        accumulator
            .snapshot()
            .into_iter()
            .find(|snapshot| snapshot.role == role)
            .expect("fixed role snapshot")
    }

    #[test]
    fn aggregates_multiple_owners_and_keeps_roles_separate() {
        let accumulator = ReaderPoolAccumulator::new();
        accumulator.record(
            ReaderPoolRole::Ordinary,
            ReaderPoolEvent::Created { capacity: 16 },
        );
        accumulator.record(
            ReaderPoolRole::Ordinary,
            ReaderPoolEvent::Created { capacity: 8 },
        );
        accumulator.record(
            ReaderPoolRole::Preparation,
            ReaderPoolEvent::Created { capacity: 4 },
        );

        let snapshots = accumulator.snapshot();
        assert_eq!(
            snapshots.iter().map(|s| s.role).collect::<Vec<_>>(),
            vec![ReaderPoolRole::Ordinary, ReaderPoolRole::Preparation]
        );
        let ordinary = role_snapshot(&accumulator, ReaderPoolRole::Ordinary);
        assert_eq!(ordinary.instances, 2);
        assert_eq!(ordinary.configured_capacity, 24);
        assert_eq!(ordinary.instances_created, 2);
        let preparation = role_snapshot(&accumulator, ReaderPoolRole::Preparation);
        assert_eq!(preparation.instances, 1);
        assert_eq!(preparation.configured_capacity, 4);
        assert_eq!(preparation.instances_created, 1);
    }

    #[test]
    fn dropped_pools_release_gauges_but_keep_lifetime_totals() {
        let accumulator = ReaderPoolAccumulator::new();
        let role = ReaderPoolRole::Ordinary;
        accumulator.record(role, ReaderPoolEvent::Created { capacity: 16 });
        accumulator.record(role, ReaderPoolEvent::ConnectionOpened);
        accumulator.record(role, ReaderPoolEvent::ConnectionOpened);
        accumulator.record(role, ReaderPoolEvent::CheckoutStarted);
        accumulator.record(
            role,
            ReaderPoolEvent::CheckoutFinished {
                elapsed: Duration::from_millis(3),
                outcome: ReaderPoolOutcome::Success,
            },
        );
        accumulator.record(role, ReaderPoolEvent::LeaseStarted);
        accumulator.record(
            role,
            ReaderPoolEvent::LeaseFinished {
                elapsed: Duration::from_millis(5),
            },
        );
        accumulator.record(role, ReaderPoolEvent::ConnectionClosed);
        accumulator.record(role, ReaderPoolEvent::Dropped { capacity: 16 });

        let snapshot = role_snapshot(&accumulator, role);
        assert_eq!(snapshot.instances, 0);
        assert_eq!(snapshot.configured_capacity, 0);
        assert_eq!(snapshot.connections, 1);
        assert_eq!(snapshot.in_use, 0);
        assert_eq!(snapshot.idle, 1);
        assert_eq!(snapshot.waiting, 0);
        assert_eq!(snapshot.instances_created, 1);
        assert_eq!(snapshot.instances_dropped, 1);
        assert_eq!(snapshot.connections_opened, 2);
        assert_eq!(snapshot.connections_closed, 1);
        assert_eq!(snapshot.checkouts_started, 1);
        assert_eq!(snapshot.checkouts_succeeded, 1);
        assert_eq!(snapshot.leases_started, 1);
        assert_eq!(snapshot.leases_finished, 1);
        assert_eq!(snapshot.checkout_duration.count, 1);
        assert_eq!(snapshot.checkout_duration.total_ns, 3_000_000);
        assert_eq!(snapshot.checkout_duration.max_ns, 3_000_000);
        assert_eq!(snapshot.lease_duration.total_ns, 5_000_000);
    }

    #[test]
    fn waiting_only_counts_unfinished_checkouts() {
        let accumulator = ReaderPoolAccumulator::new();
        let role = ReaderPoolRole::Preparation;
        accumulator.record(role, ReaderPoolEvent::CheckoutStarted);
        accumulator.record(role, ReaderPoolEvent::CheckoutStarted);
        assert_eq!(role_snapshot(&accumulator, role).waiting, 2);
        accumulator.record(
            role,
            ReaderPoolEvent::CheckoutFinished {
                elapsed: Duration::from_secs(30),
                outcome: ReaderPoolOutcome::Timeout,
            },
        );
        accumulator.record(
            role,
            ReaderPoolEvent::CheckoutFinished {
                elapsed: Duration::from_millis(1),
                outcome: ReaderPoolOutcome::Cancelled,
            },
        );
        let snapshot = role_snapshot(&accumulator, role);
        assert_eq!(snapshot.waiting, 0);
        assert_eq!(snapshot.checkouts_timed_out, 1);
        assert_eq!(snapshot.checkouts_cancelled, 1);
        assert_eq!(snapshot.checkout_duration.count, 2);
        assert_eq!(snapshot.checkout_duration.max_ns, 30_000_000_000);
        let highest_bucket = snapshot
            .checkout_duration
            .buckets
            .last()
            .expect("infinite bucket");
        assert_eq!(highest_bucket.upper_bound_ms, None);
        assert_eq!(highest_bucket.count, 2);
    }

    #[test]
    fn duration_count_uses_the_scraped_infinite_bucket() {
        let duration = DurationAccumulator::new();
        // Model a recording paused between the total/max and bucket updates.
        duration.total_ns.store(1, Ordering::Relaxed);
        duration.max_ns.store(1, Ordering::Relaxed);
        let before = duration.snapshot();
        assert_eq!(before.count, before.buckets.last().unwrap().count);
        assert_eq!(before.count, 0);

        duration.buckets[0].store(1, Ordering::Relaxed);
        let after = duration.snapshot();
        assert_eq!(after.count, after.buckets.last().unwrap().count);
        assert_eq!(after.count, 1);
    }

    #[test]
    fn duration_buckets_preserve_sub_millisecond_boundaries() {
        let duration = DurationAccumulator::new();
        duration.record(Duration::from_nanos(1));
        duration.record(Duration::from_nanos(1_000_001));
        let snapshot = duration.snapshot();
        assert_eq!(snapshot.buckets[0].upper_bound_ms, Some(0));
        assert_eq!(snapshot.buckets[0].count, 0);
        assert_eq!(snapshot.buckets[1].upper_bound_ms, Some(1));
        assert_eq!(snapshot.buckets[1].count, 1);
        assert_eq!(snapshot.buckets[2].upper_bound_ms, Some(2));
        assert_eq!(snapshot.buckets[2].count, 2);
        assert_eq!(snapshot.count, 2);
    }

    #[test]
    fn buckets_are_fixed_cumulative_and_carry_no_free_form_labels() {
        let accumulator = ReaderPoolAccumulator::new();
        let role = ReaderPoolRole::Ordinary;
        for millis in [0, 3, 2_500, 5_000] {
            accumulator.record(
                role,
                ReaderPoolEvent::ConnectFinished {
                    elapsed: Duration::from_millis(millis),
                    success: millis != 5_000,
                },
            );
        }
        let snapshot = role_snapshot(&accumulator, role);

        let bounds = snapshot
            .connect_duration
            .buckets
            .iter()
            .map(|bucket| bucket.upper_bound_ms)
            .collect::<Vec<_>>();
        let expected = HISTOGRAM_UPPER_BOUNDS_MS
            .iter()
            .map(|bound| (*bound != u64::MAX).then_some(*bound))
            .collect::<Vec<_>>();
        assert_eq!(bounds, expected);

        let counts = snapshot
            .connect_duration
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .collect::<Vec<_>>();
        assert!(counts.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(counts.last().copied(), Some(4));
        assert_eq!(snapshot.connect_duration.count, 4);
        assert_eq!(snapshot.connects_failed, 1);
        assert_eq!(snapshot.connects_succeeded, 3);

        let value = serde_json::to_value(accumulator.snapshot()).expect("snapshot json");
        let entries = value.as_array().expect("role array");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["role"], "ordinary");
        assert_eq!(entries[1]["role"], "preparation");
        let allowed = [
            "checkout_duration",
            "checkouts_cancelled",
            "checkouts_failed",
            "checkouts_started",
            "checkouts_succeeded",
            "checkouts_timed_out",
            "configured_capacity",
            "connect_duration",
            "connections",
            "connections_closed",
            "connections_opened",
            "connects_failed",
            "connects_started",
            "connects_succeeded",
            "idle",
            "in_use",
            "instances",
            "instances_created",
            "instances_dropped",
            "lease_duration",
            "leases_finished",
            "leases_started",
            "role",
            "waiting",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        for entry in entries {
            let keys = entry
                .as_object()
                .expect("role object")
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(keys, allowed, "reader-pool fields must stay bounded");
        }
        let rendered = serde_json::to_string(&value).expect("snapshot json string");
        assert!(!rendered.contains("owner"));
        assert!(!rendered.contains(".sqlite"));
        assert!(!rendered.contains('/'));
    }
}
