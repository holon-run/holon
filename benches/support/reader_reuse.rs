//! Fixed-work, synthetic A/B workload. Uses only APIs present before pooling.
use std::{
    hint::black_box,
    sync::{Arc, Barrier},
    thread,
    time::Instant,
};

use anyhow::{ensure, Context, Result};
use holon::{
    diagnostics::performance_snapshot,
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{AgentState, WorkItemRecord, WorkItemState},
};
use serde_json::{json, Value};

pub(super) fn run(repetitions: usize) -> Result<Vec<Value>> {
    let operations = std::env::var("HOLON_BENCH_READER_OPERATIONS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_READER_OPERATIONS must be a multiple of 16")?
        .unwrap_or(256);
    ensure!(
        operations >= 16 && operations % 16 == 0,
        "reader operations must be positive and divisible by 16"
    );
    let mut results = Vec::new();
    for agents in [1, 60] {
        let root = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            root.path().join("runtime.sqlite"),
            root.path().join("runtime.lock"),
        )?;
        let ids: Vec<_> = (0..agents)
            .map(|index| format!("reader-{index:03}"))
            .collect();
        for id in &ids {
            let storage = AppStorage::new_for_agent(root.path(), id, db.clone())?;
            storage.write_agent(&AgentState::new(id))?;
            let mut item = WorkItemRecord::new(id, "synthetic work", WorkItemState::Open);
            item.id = format!("item-{id}");
            storage.append_work_item(&item)?;
        }
        db.transaction(|tx| {
            tx.execute_batch("CREATE TABLE reader_bench_writes (value INTEGER NOT NULL)")?;
            Ok(())
        })?;
        let start = Instant::now();
        ensure!(db.agent_list_posture_rows(&ids)?.rows.len() == agents);
        let first_read_ns = start.elapsed().as_nanos();
        for workers in [1, 4, 16] {
            for writes in [0, 64] {
                // Warm every worker before taking fixed-work resource snapshots.
                let _ = measure(&db, &ids, workers, workers, 0)?;
                for repetition in 0..repetitions {
                    let before = performance_snapshot();
                    let resources = super::resource_usage();
                    let fds_before = fd_count();
                    let measured = measure(&db, &ids, workers, operations, writes)?;
                    let after_resources = super::resource_usage();
                    results.push(json!({
                        "agents": agents, "workers": workers, "operations": operations,
                        "writes": writes, "repetition": repetition,
                        "fixture_first_read_ns": first_read_ns,
                        "wall_time_ns": measured.wall_ns,
                        "user_cpu_ns": super::duration_delta(
                            after_resources.user_cpu, resources.user_cpu).as_nanos(),
                        "system_cpu_ns": super::duration_delta(
                            after_resources.system_cpu, resources.system_cpu).as_nanos(),
                        "peak_rss_kb": after_resources.peak_rss_kb,
                        "fd_before": fds_before, "fd_after": fd_count(),
                        "wal_bytes": std::fs::metadata(root.path().join("runtime.sqlite-wal"))
                            .ok().map(|metadata| metadata.len()),
                        "read_latency": percentiles(&measured.reads),
                        "writer_latency": percentiles(&measured.writes),
                        "read_samples_ns": measured.reads, "writer_samples_ns": measured.writes,
                        "diagnostics_before": before, "diagnostics_after": performance_snapshot(),
                        "limitations": [
                            "Synthetic warm local database, not production or cold filesystem cache.",
                            "Wall/CPU include scoped thread startup and join; per-read latency does not.",
                            "RSS is process lifetime peak; descriptors and WAL are end snapshots.",
                            "Writer is a fixed burst of dedicated-writer transactions, not a rate model.",
                            "Diagnostics are cumulative process-global; use before/after deltas.",
                            "Fixed case order; repeat A/B/A to assess host and ordering noise."
                        ]
                    }));
                }
            }
        }
    }
    Ok(results)
}

struct Measurement {
    wall_ns: u128,
    reads: Vec<u128>,
    writes: Vec<u128>,
}

fn measure(
    db: &RuntimeDb,
    ids: &[String],
    workers: usize,
    operations: usize,
    writes: usize,
) -> Result<Measurement> {
    let gate = Arc::new(Barrier::new(workers + usize::from(writes > 0)));
    let start = Instant::now();
    let (reads, writer_samples) = thread::scope(|scope| -> Result<_> {
        let mut readers = Vec::new();
        for _ in 0..workers {
            let gate = gate.clone();
            readers.push(scope.spawn(move || -> Result<Vec<u128>> {
                gate.wait();
                let mut samples = Vec::with_capacity(operations / workers);
                for _ in 0..operations / workers {
                    let start = Instant::now();
                    let rows = db.agent_list_posture_rows(ids)?;
                    ensure!(rows.rows.len() == ids.len(), "list row count");
                    ensure!(
                        rows.rows.iter().zip(ids).all(|(row, id)| {
                            row.agent_id == *id
                                && row.agent_state_json.is_some()
                                && row.facts.as_ref().is_ok_and(|facts| {
                                    facts.open_work_items.len() == 1
                                        && facts.open_work_items[0].agent_id == *id
                                })
                        }),
                        "list facts must not silently degrade or cross agent boundaries"
                    );
                    drop(black_box(rows));
                    samples.push(start.elapsed().as_nanos());
                }
                Ok(samples)
            }));
        }
        let writer = (writes > 0).then(|| {
            scope.spawn(move || -> Result<Vec<u128>> {
                gate.wait();
                let mut samples = Vec::with_capacity(writes);
                for value in 0..writes {
                    let start = Instant::now();
                    db.transaction(|tx| {
                        tx.execute(
                            "INSERT INTO reader_bench_writes VALUES (?1)",
                            [value as i64],
                        )?;
                        Ok(())
                    })?;
                    samples.push(start.elapsed().as_nanos());
                }
                Ok(samples)
            })
        });
        let mut samples = Vec::with_capacity(operations);
        for reader in readers {
            samples.extend(
                reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("reader panicked"))??,
            );
        }
        let writer_samples = writer
            .map(|writer| {
                writer
                    .join()
                    .map_err(|_| anyhow::anyhow!("writer panicked"))?
            })
            .transpose()?
            .unwrap_or_default();
        Ok((samples, writer_samples))
    })?;
    ensure!(reads.len() == operations, "fixed read work");
    Ok(Measurement {
        wall_ns: start.elapsed().as_nanos(),
        reads,
        writes: writer_samples,
    })
}

fn percentiles(samples: &[u128]) -> Value {
    if samples.is_empty() {
        return Value::Null;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100) - 1];
    json!({
        "count": sorted.len(), "p50_ns": percentile(50),
        "p95_ns": percentile(95), "p99_ns": percentile(99),
        "method": "nearest_rank"
    })
}

fn fd_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count().saturating_sub(1))
}
