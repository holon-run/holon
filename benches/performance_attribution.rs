use std::{hint::black_box, sync::Barrier, thread, time::Instant};

use anyhow::{ensure, Context, Result};
use chrono::Utc;
use holon::{
    diagnostics::performance_snapshot,
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{
        AgentState, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
        WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::TempDir;

const AGENT: &str = "attribution";
const OTHER: &str = "other";
const WORK_QUEUE_TERMINAL_WINDOW: usize = 128;

fn projected_item_count(own_completed: usize) -> usize {
    own_completed.min(WORK_QUEUE_TERMINAL_WINDOW) + 1
}

struct Fixture {
    storage: AppStorage,
    db: RuntimeDb,
    // Keep the directory alive until all database handles have been dropped.
    _root: TempDir,
}

impl Fixture {
    fn new(own_completed: usize, other_completed: usize, other_waits: usize) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            root.path().join("runtime.sqlite"),
            root.path().join("runtime.lock"),
        )?;
        let storage = AppStorage::new_for_agent(root.path(), AGENT, db.clone())?;
        storage.write_agent(&AgentState::new(AGENT))?;
        let other = AppStorage::new_for_agent(root.path(), OTHER, db.clone())?;
        other.write_agent(&AgentState::new(OTHER))?;
        for (store, agent, count) in [
            (&storage, AGENT, own_completed),
            (&other, OTHER, other_completed),
        ] {
            for index in 0..count {
                let mut item = WorkItemRecord::new(agent, "completed", WorkItemState::Completed);
                item.id = format!("{agent}-completed-{index}");
                store.append_work_item(&item)?;
            }
        }
        let mut runnable = WorkItemRecord::new(AGENT, "runnable", WorkItemState::Open);
        runnable.id = "own-runnable".into();
        runnable.plan_status = WorkItemPlanStatus::Ready;
        storage.append_work_item(&runnable)?;
        for index in 0..other_waits {
            let mut item = WorkItemRecord::new(OTHER, "waiting", WorkItemState::Open);
            item.id = format!("other-waiting-{index}");
            item.plan_status = WorkItemPlanStatus::Ready;
            item.blocked_by = Some("operator_input".into());
            other.append_work_item(&item)?;
            let now = Utc::now();
            other.append_wait_condition(&WaitConditionRecord {
                id: format!("wait-{index}"),
                agent_id: OTHER.into(),
                work_item_id: Some(item.id),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Operator,
                source: None,
                subject_ref: None,
                waiting_for: "operator_input".into(),
                wake_sources: vec![WakeSource::OperatorInput],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
                trigger_message_id: None,
                triggered_at: None,
            })?;
        }
        Ok(Self {
            storage,
            db,
            _root: root,
        })
    }

    fn verify_rows(&self, own: usize, other: usize, waits: usize) -> Result<Value> {
        let connection = self.db.connection()?;
        let count =
            |sql: &str| -> Result<usize> { Ok(connection.query_row(sql, [], |row| row.get(0))?) };
        let work_items = count("SELECT count(*) FROM work_items")?;
        let completed = count("SELECT count(*) FROM work_items WHERE state = 'completed'")?;
        let own_completed = count(
            "SELECT count(*) FROM work_items WHERE agent_id = 'attribution' AND state = 'completed'",
        )?;
        let active_waits = count("SELECT count(*) FROM wait_conditions WHERE status = 'active'")?;
        let linked_waits = count(
            "SELECT count(DISTINCT w.work_item_id) FROM wait_conditions w
             JOIN work_items i ON i.work_item_id = w.work_item_id
             WHERE w.status = 'active' AND i.state = 'open'
             AND w.agent_id = 'other' AND i.agent_id = 'other'",
        )?;
        ensure!(work_items == 1 + own + other + waits, "work item row count");
        ensure!(
            completed == own + other && own_completed == own,
            "completed row count"
        );
        ensure!(
            active_waits == waits && linked_waits == waits,
            "active wait linkage"
        );
        let projected_items = projected_item_count(own);
        verify_queue(&self.storage, projected_items)?;
        Ok(json!({
            "work_items": work_items, "own_completed": own_completed,
            "other_completed": completed - own_completed, "active_waits": active_waits,
            "distinct_linked_open_items": linked_waits, "projected_items": projected_items,
        }))
    }
}

fn verify_queue(storage: &AppStorage, expected: usize) -> Result<()> {
    let queue = storage.work_queue_read_model()?;
    ensure!(queue.items.len() == expected, "projection item count");
    let runnable: Vec<_> = queue.items.iter().filter(|item| item.is_runnable).collect();
    ensure!(
        runnable.len() == 1 && runnable[0].id == "own-runnable",
        "projection runnable isolation"
    );
    Ok(())
}

#[derive(Serialize)]
struct Sample {
    worker: usize,
    operation: usize,
    wall_time_ns: u128,
}

fn fd_count() -> Option<usize> {
    // Subtract the descriptor opened by read_dir itself.
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count().saturating_sub(1))
}

fn summary(samples: &[Sample]) -> Value {
    let mut values: Vec<_> = samples.iter().map(|sample| sample.wall_time_ns).collect();
    values.sort_unstable();
    let percentile = |percent: usize| values[(values.len() * percent).div_ceil(100) - 1];
    json!({
        "count": values.len(), "p50_ns": percentile(50),
        "p95_ns": percentile(95), "p99_ns": percentile(99),
        "percentile_method": "nearest_rank",
    })
}

fn connection_case(extra_fds: usize, operations: usize) -> Result<Value> {
    let fixture = Fixture::new(0, 0, 0)?;
    let rows = fixture.verify_rows(0, 0, 0)?;
    // Avoid last-connection-close checkpoint effects throughout measurement.
    let _held_connection = fixture.db.connection()?;
    let baseline_fds = fd_count();
    let files = (0..extra_fds)
        .map(|_| std::fs::File::open("/dev/null"))
        .collect::<std::io::Result<Vec<_>>>()?;
    drop(fixture.db.connection()?); // Warm up outside the diagnostic interval.
    let before_fds = fd_count();
    if let (Some(base), Some(before)) = (baseline_fds, before_fds) {
        ensure!(before == base + extra_fds, "extra descriptor count");
    }
    let before = performance_snapshot();
    let mut samples = Vec::with_capacity(operations);
    for operation in 0..operations {
        let start = Instant::now();
        let connection = fixture.db.connection()?;
        let elapsed = start.elapsed().as_nanos();
        samples.push(Sample {
            worker: 0,
            operation,
            wall_time_ns: elapsed,
        });
        drop(black_box(connection)); // Connection close is not timed.
    }
    let after = performance_snapshot();
    let after_fds = fd_count();
    ensure!(
        before_fds == after_fds,
        "descriptor count changed in connection case extra_fds={extra_fds}: before={before_fds:?}, after={after_fds:?}"
    );
    fixture.verify_rows(0, 0, 0)?;
    black_box(&files);
    Ok(json!({
        "dimension": "connection_extra_fds", "extra_fds": extra_fds,
        "fd_baseline": baseline_fds, "fd_before": before_fds, "fd_after": after_fds,
        "rows": rows, "diagnostics_before": before, "diagnostics_after": after,
        "summary": summary(&samples), "samples": samples,
    }))
}

fn queue_case(
    dimension: &str,
    own: usize,
    other: usize,
    waits: usize,
    workers: usize,
    operations: usize,
) -> Result<Value> {
    let fixture = Fixture::new(own, other, waits)?;
    let rows = fixture.verify_rows(own, other, waits)?;
    let _held_connection = fixture.db.connection()?;
    verify_queue(&fixture.storage, projected_item_count(own))?;
    let warmup_barrier = Barrier::new(workers);
    thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let db = &fixture.db;
                let storage = &fixture.storage;
                let barrier = &warmup_barrier;
                scope.spawn(move || -> Result<()> {
                    let connection = db.connection()?;
                    barrier.wait();
                    verify_queue(storage, projected_item_count(own))?;
                    drop(connection);
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("warmup reader panicked"))??;
        }
        Ok(())
    })?;
    let before_fds = fd_count();
    let before = performance_snapshot();
    let barrier = Barrier::new(workers);
    let samples = thread::scope(|scope| -> Result<Vec<Sample>> {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let storage = &fixture.storage;
                let barrier = &barrier;
                scope.spawn(move || -> Result<Vec<Sample>> {
                    let mut samples = Vec::with_capacity(operations / workers);
                    barrier.wait();
                    for operation in 0..operations / workers {
                        let start = Instant::now();
                        let queue = storage.work_queue_read_model()?;
                        let elapsed = start.elapsed().as_nanos();
                        ensure!(
                            queue.items.len() == projected_item_count(own),
                            "measured projection count"
                        );
                        black_box(queue);
                        samples.push(Sample {
                            worker,
                            operation,
                            wall_time_ns: elapsed,
                        });
                    }
                    Ok(samples)
                })
            })
            .collect();
        let mut samples = Vec::with_capacity(operations);
        for handle in handles {
            samples.extend(
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("reader panicked"))??,
            );
        }
        Ok(samples)
    })?;
    let after = performance_snapshot();
    let after_fds = fd_count();
    ensure!(samples.len() == operations, "operation count");
    ensure!(
        before_fds == after_fds,
        "descriptor count changed in queue case dimension={dimension} own={own} other={other} waits={waits} workers={workers}: before={before_fds:?}, after={after_fds:?}"
    );
    let rows_after = fixture.verify_rows(own, other, waits)?;
    ensure!(rows == rows_after, "fixture changed");
    Ok(json!({
        "dimension": dimension, "own_completed": own, "other_completed": other,
        "other_active_waits": waits, "workers": workers, "total_operations": operations,
        "fd_before": before_fds, "fd_after": after_fds, "rows": rows,
        "diagnostics_before": before, "diagnostics_after": after,
        "summary": summary(&samples), "samples": samples,
    }))
}

fn main() -> Result<()> {
    let requested = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be an integer >= 10")?
        .unwrap_or(40);
    ensure!(requested >= 10, "at least 10 samples are required");
    // Equal total operations, divisible by all thread counts.
    let operations = requested.checked_add(3).context("sample count overflow")? / 4 * 4;
    let mut results = Vec::new();
    if cfg!(target_os = "linux") {
        for fds in [0, 128, 512] {
            results.push(connection_case(fds, operations)?);
        }
    }
    for count in [0, 100, 500] {
        results.push(queue_case("own_completed", count, 0, 0, 1, operations)?);
        results.push(queue_case("other_completed", 0, count, 0, 1, operations)?);
    }
    for waits in [0, 10, 100] {
        results.push(queue_case(
            "other_active_waits",
            0,
            0,
            waits,
            1,
            operations,
        )?);
    }
    for workers in [1, 2, 4] {
        results.push(queue_case(
            "concurrent_queue",
            0,
            0,
            0,
            workers,
            operations,
        )?);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": "holon.performance.attribution.v1",
            "benchmark_suite": "performance_attribution",
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "network_mode": "none", "requested_samples": requested,
            "actual_operations_per_case": operations,
            "connection_fd_dimension_skipped": !cfg!(target_os = "linux"),
            "limitations": [
                "Warm local temporary databases; no cold-cache or production workload claim.",
                "Diagnostics are cumulative process-global snapshots, not reset per case.",
                "Connection timing excludes close; queue timing excludes result destruction and assertions.",
                "Concurrency reports per-operation latency, not throughput; thread startup is excluded.",
                "Other-agent rows belong to one other agent; dimensions are not a factorial experiment.",
                "Fixed case order and small samples can make tail percentiles noisy."
            ],
            "results": results,
        }))?
    );
    Ok(())
}
