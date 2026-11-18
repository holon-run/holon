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
