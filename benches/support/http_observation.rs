//! Optional sustained observations. The existing short CI workloads stay intact.
use super::{
    benchmark_selected, duration_delta, repeat_request, resource_usage, summarize, BenchmarkResult,
    HttpFixture, Sample, PERFORMANCE_REQUESTS_PER_SAMPLE, WORK_ITEMS,
    WORK_ITEM_REQUESTS_PER_SAMPLE,
};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use holon::{
    diagnostics::{attribution, performance_snapshot},
    http::{router, AppState},
};
use serde::Serialize;

const HIT_BATCH: usize = 256;

#[derive(Clone, Copy)]
pub(super) struct Settings {
    pub seconds: u64,
    min_batches: usize,
}

impl Settings {
    pub fn load(samples: usize) -> Result<Option<Self>> {
        let Some(value) = std::env::var_os("HOLON_BENCH_WINDOW_SECS") else {
            return Ok(None);
        };
        let seconds = value
            .to_str()
            .context("HOLON_BENCH_WINDOW_SECS must be UTF-8")?
            .parse()?;
        ensure!(
            (10..=30).contains(&seconds),
            "HOLON_BENCH_WINDOW_SECS must be 10..=30"
        );
        Ok(Some(Self {
            seconds,
            min_batches: samples.max(100),
        }))
    }
}

#[derive(Serialize)]
pub(super) struct Observation {
    minimum_request_time_ns: u128,
    measured_request_time_ns: u128,
    sampling_wall_time_ns: u128,
    minimum_batches: usize,
    warmup_batches: usize,
    timed_operations: usize,
    sample_unit: &'static str,
    stages: Vec<StageDelta>,
}

#[derive(Serialize)]
struct StageDelta {
    name: String,
    count: u64,
    inclusive_total_ns: u64,
    rows: u64,
}

#[derive(Clone, Copy)]
enum Cache {
    Miss,
    Hit,
    Unspecified,
}

pub(super) fn run(
    runtime: &tokio::runtime::Runtime,
    fixture: &HttpFixture,
    settings: Settings,
    filter: &[String],
) -> Result<Vec<BenchmarkResult>> {
    let state = format!("/api/agents/{}/state", fixture.default_agent_id);
    let work = format!(
        "/api/agents/{}/work-items?limit={WORK_ITEMS}",
        fixture.default_agent_id
    );
    let mut results = Vec::new();
    for (id, uri, cache, operations) in [
        (
            "http.agents_list.gate_miss",
            "/api/agents/list",
            Cache::Miss,
            1,
        ),
        (
            "http.agents_list.gate_hit",
            "/api/agents/list",
            Cache::Hit,
            HIT_BATCH,
        ),
        ("http.agent_state.gate_miss", &state, Cache::Miss, 1),
        ("http.agent_state.gate_hit", &state, Cache::Hit, HIT_BATCH),
        (
            "http.runtime_performance.100",
            "/api/control/runtime/performance",
            Cache::Unspecified,
            PERFORMANCE_REQUESTS_PER_SAMPLE,
        ),
        (
            "http.agents_list.101",
            "/api/agents/list",
            Cache::Unspecified,
            HIT_BATCH,
        ),
        ("http.work_items.200", &work, Cache::Unspecified, 1),
        (
            "http.work_items.200x10",
            &work,
            Cache::Unspecified,
            WORK_ITEM_REQUESTS_PER_SAMPLE,
        ),
    ] {
        if benchmark_selected(filter, id) {
            eprintln!("{}", serde_json::json!({"case": id, "event": "started"}));
            results.push(measure(
                runtime, fixture, settings, id, uri, cache, operations,
            )?);
            eprintln!("{}", serde_json::json!({"case": id, "event": "completed"}));
        }
    }
    Ok(results)
}

fn measure(
    runtime: &tokio::runtime::Runtime,
    fixture: &HttpFixture,
    settings: Settings,
    id: &'static str,
    uri: &str,
    cache: Cache,
    operations: usize,
) -> Result<BenchmarkResult> {
    let target = Duration::from_secs(settings.seconds);
    let mut samples = Vec::new();
    let mut totals: Vec<_> = attribution::snapshot()
        .into_iter()
        .map(|stage| StageDelta {
            name: stage.name,
            count: 0,
            inclusive_total_ns: 0,
            rows: 0,
        })
        .collect();
    let mut measured = Duration::ZERO;
    let mut warmup = Duration::ZERO;
    let mut warmup_batches = 0;
    let mut sampling_started = None;
    while warmup < Duration::from_secs(1)
        || samples.len() < settings.min_batches
        || measured < target
    {
        // Reset only HTTP bytes. Priming, router construction, diagnostic reads
        // and resource snapshots are outside request timing and stage deltas.
        let app = match cache {
            Cache::Unspecified => fixture.app.clone(),
            _ => router(AppState::for_unix(fixture.host.clone())),
        };
        if matches!(cache, Cache::Hit) {
            runtime.block_on(repeat_request(&app, uri, 1))?;
        }
        let gate_before = performance_snapshot().projection_gate;
        let before = attribution::snapshot();
        let resources_before = resource_usage();
        let started = Instant::now();
        runtime.block_on(repeat_request(&app, uri, operations))?;
        let elapsed = started.elapsed();
        let resources_after = resource_usage();
        let after = attribution::snapshot();
        let gate_after = performance_snapshot().projection_gate;
        match cache {
            Cache::Miss => ensure!(
                gate_after.leaders == gate_before.leaders + 1
                    && gate_after.cache_hits == gate_before.cache_hits,
                "{id}: expected exactly one miss"
            ),
            Cache::Hit => ensure!(
                gate_after.cache_hits == gate_before.cache_hits + operations as u64
                    && gate_after.leaders == gate_before.leaders,
                "{id}: TTL expired or a batch was not entirely cache hits"
            ),
            Cache::Unspecified => {}
        }
        if warmup < Duration::from_secs(1) {
            warmup += elapsed;
            warmup_batches += 1;
            continue;
        }
        // Includes setup between batches, unlike the timed-request duration.
        sampling_started.get_or_insert(started);
        measured += elapsed;
        for ((total, before), after) in totals.iter_mut().zip(before).zip(after) {
            ensure!(
                total.name == before.name && before.name == after.name,
                "stage set changed"
            );
            total.count += after
                .count
                .checked_sub(before.count)
                .context("stage count reset")?;
            total.inclusive_total_ns += after
                .total_ns
                .checked_sub(before.total_ns)
                .context("stage time reset")?;
            total.rows += after
                .rows
                .checked_sub(before.rows)
                .context("stage rows reset")?;
        }
        samples.push(Sample {
            iteration: samples.len() + 1,
            wall_time_ns: elapsed.as_nanos(),
            user_cpu_ns: duration_delta(resources_after.user_cpu, resources_before.user_cpu)
                .as_nanos(),
            system_cpu_ns: duration_delta(resources_after.system_cpu, resources_before.system_cpu)
                .as_nanos(),
            peak_rss_kb: resources_after.peak_rss_kb,
            exit_status: "ok",
        });
    }
    Ok(BenchmarkResult {
        benchmark_id: id,
        operations_per_sample: operations,
        summary: summarize(&samples, operations),
        observation: Some(Observation {
            minimum_request_time_ns: target.as_nanos(),
            measured_request_time_ns: measured.as_nanos(),
            sampling_wall_time_ns: sampling_started
                .context("no measured batches")?
                .elapsed()
                .as_nanos(),
            minimum_batches: settings.min_batches,
            warmup_batches,
            timed_operations: samples.len() * operations,
            sample_unit: if operations == 1 { "request" } else { "batch" },
            stages: totals,
        }),
        samples,
    })
}
