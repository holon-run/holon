use std::{
    hint::black_box,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use holon::{
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{
        AgentState, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
        WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};
use serde::Serialize;

const AGENT_ID: &str = "benchmark-agent";
const DEFAULT_SAMPLES: usize = 10;
const WARMUP_SAMPLES: usize = 1;
const RUNNABLE_ITEMS: usize = 100;
const BLOCKED_ITEMS: usize = 50;
const WAITING_ITEMS: usize = 50;
const CONCURRENT_READERS: usize = 8;
const READS_PER_READER: usize = 25;

#[derive(Serialize)]
struct BenchmarkArtifact {
    schema_version: &'static str,
    benchmark_suite: &'static str,
    workload_version: &'static str,
    started_at: String,
    environment: Environment,
    config: BenchmarkConfig,
    results: Vec<BenchmarkResult>,
}

#[derive(Serialize)]
struct Environment {
    git_revision: Option<String>,
    git_dirty: Option<bool>,
    os: &'static str,
    arch: &'static str,
    cpu_model: Option<String>,
    logical_cpus: Option<usize>,
    rustc_version: Option<String>,
    build_profile: &'static str,
    network_mode: &'static str,
}

#[derive(Serialize)]
struct BenchmarkConfig {
    warmup_repetitions: usize,
    measured_repetitions: usize,
    runnable_work_items: usize,
    blocked_work_items: usize,
    waiting_work_items: usize,
    concurrency: usize,
    operations_per_concurrent_sample: usize,
}

#[derive(Serialize)]
struct BenchmarkResult {
    benchmark_id: &'static str,
    operations_per_sample: usize,
    samples: Vec<Sample>,
    summary: Summary,
}

#[derive(Clone, Serialize)]
struct Sample {
    iteration: usize,
    wall_time_ns: u128,
    user_cpu_ns: u128,
    system_cpu_ns: u128,
    peak_rss_kb: u64,
    exit_status: &'static str,
}

#[derive(Serialize)]
struct Summary {
    median_wall_time_ns: u128,
    min_wall_time_ns: u128,
    max_wall_time_ns: u128,
    median_absolute_deviation_ns: u128,
    median_operations_per_second: f64,
}

fn main() -> Result<()> {
    let samples = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be a positive integer")?
        .unwrap_or(DEFAULT_SAMPLES);
    anyhow::ensure!(samples > 0, "HOLON_BENCH_SAMPLES must be positive");

    let filter = benchmark_filter();
    let results = run_benchmarks(samples, &filter)?;
    anyhow::ensure!(
        !results.is_empty(),
        "HOLON_BENCH_FILTER selected no workloads"
    );
    let artifact = BenchmarkArtifact {
        schema_version: "holon.performance.v0",
        benchmark_suite: "scheduler",
        workload_version: "v1",
        started_at: Utc::now().to_rfc3339(),
        environment: environment(),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            runnable_work_items: RUNNABLE_ITEMS,
            blocked_work_items: BLOCKED_ITEMS,
            waiting_work_items: WAITING_ITEMS,
            concurrency: CONCURRENT_READERS,
            operations_per_concurrent_sample: CONCURRENT_READERS * READS_PER_READER,
        },
        results,
    };

    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}

fn benchmark_filter() -> Vec<String> {
    std::env::var("HOLON_BENCH_FILTER")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn benchmark_selected(filter: &[String], benchmark_id: &str) -> bool {
    filter.is_empty() || filter.iter().any(|selected| selected == benchmark_id)
}

fn run_benchmarks(samples: usize, filter: &[String]) -> Result<Vec<BenchmarkResult>> {
    let root = tempfile::tempdir().context("creating scheduler benchmark root")?;
    let db = RuntimeDb::open_and_migrate(
        root.path().join(".holon/state/runtime.sqlite"),
        root.path().join(".holon/state/runtime.lock"),
    )?;
    let storage = AppStorage::new_for_agent(root.path(), AGENT_ID, db)?;
    let now = Utc::now();
    let runnable_ids = seed_scheduler_state(&storage, now)?;

    let mut results = Vec::new();
    if benchmark_selected(filter, "scheduler.work_queue_read_model.200") {
        results.push(measure(
            "scheduler.work_queue_read_model.200",
            samples,
            1,
            || verify_fifo_projection(&storage, &runnable_ids),
        )?);
    }
    if benchmark_selected(filter, "scheduler.due_rechecks.50") {
        results.push(measure("scheduler.due_rechecks.50", samples, 1, || {
            let due = storage.due_blocked_work_item_rechecks(AGENT_ID, now)?;
            anyhow::ensure!(due.len() == BLOCKED_ITEMS, "expected 50 due rechecks");
            black_box(due);
            Ok(())
        })?);
    }
    if benchmark_selected(filter, "scheduler.active_waits.50") {
        results.push(measure("scheduler.active_waits.50", samples, 1, || {
            let waits = storage.active_wait_conditions_for_agent(AGENT_ID)?;
            anyhow::ensure!(waits.len() == WAITING_ITEMS, "expected 50 active waits");
            black_box(waits);
            Ok(())
        })?);
    }
    if benchmark_selected(filter, "scheduler.work_queue_read_model.concurrent_8x25") {
        results.push(measure(
            "scheduler.work_queue_read_model.concurrent_8x25",
            samples,
            CONCURRENT_READERS * READS_PER_READER,
            || {
                thread::scope(|scope| -> Result<()> {
                    let mut readers = Vec::with_capacity(CONCURRENT_READERS);
                    for _ in 0..CONCURRENT_READERS {
                        let storage = storage.clone();
                        let runnable_ids = &runnable_ids;
                        readers.push(scope.spawn(move || -> Result<()> {
                            for _ in 0..READS_PER_READER {
                                verify_fifo_projection(&storage, runnable_ids)?;
                            }
                            Ok(())
                        }));
                    }
                    for reader in readers {
                        reader
                            .join()
                            .expect("scheduler benchmark reader panicked")?;
                    }
                    Ok(())
                })
            },
        )?);
    }
    Ok(results)
}

fn seed_scheduler_state(storage: &AppStorage, now: chrono::DateTime<Utc>) -> Result<Vec<String>> {
    let mut agent = AgentState::new(AGENT_ID);
    let mut runnable_ids = Vec::with_capacity(RUNNABLE_ITEMS);

    for index in 0..RUNNABLE_ITEMS {
        let mut item = WorkItemRecord::new(
            AGENT_ID,
            format!("runnable benchmark work item {index}"),
            WorkItemState::Open,
        );
        item.id = format!("work_runnable_{index:03}");
        item.plan_status = WorkItemPlanStatus::Ready;
        item.created_at = now - ChronoDuration::milliseconds((RUNNABLE_ITEMS - index) as i64);
        item.updated_at = item.created_at;
        if index == 0 {
            agent.current_work_item_id = Some(item.id.clone());
        } else {
            runnable_ids.push(item.id.clone());
        }
        storage.append_work_item(&item)?;
    }

    for index in 0..BLOCKED_ITEMS {
        let mut item = WorkItemRecord::new(
            AGENT_ID,
            format!("blocked benchmark work item {index}"),
            WorkItemState::Open,
        );
        item.id = format!("work_blocked_{index:03}");
        item.plan_status = WorkItemPlanStatus::Ready;
        item.blocked_by = Some("benchmark_recheck".into());
        item.recheck_at = Some(now - ChronoDuration::seconds(1));
        storage.append_work_item(&item)?;
    }

    for index in 0..WAITING_ITEMS {
        let mut item = WorkItemRecord::new(
            AGENT_ID,
            format!("waiting benchmark work item {index}"),
            WorkItemState::Open,
        );
        item.id = format!("work_waiting_{index:03}");
        item.plan_status = WorkItemPlanStatus::Ready;
        item.blocked_by = Some("operator_input".into());
        storage.append_work_item(&item)?;
        storage.append_wait_condition(&WaitConditionRecord {
            id: format!("wait_benchmark_{index:03}"),
            agent_id: AGENT_ID.into(),
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

    storage.write_agent(&agent)?;
    Ok(runnable_ids)
}

fn verify_fifo_projection(storage: &AppStorage, expected_ids: &[String]) -> Result<()> {
    let projection = storage.work_queue_read_model()?;
    anyhow::ensure!(
        projection.items.len() == RUNNABLE_ITEMS + BLOCKED_ITEMS + WAITING_ITEMS,
        "scheduler projection lost work items"
    );
    let actual_ids = projection
        .items
        .iter()
        .filter(|item| item.is_runnable && !item.is_current)
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        actual_ids.len() == expected_ids.len(),
        "scheduler projection changed runnable candidate count"
    );
    anyhow::ensure!(
        actual_ids
            .iter()
            .zip(expected_ids)
            .all(|(actual, expected)| *actual == expected),
        "scheduler projection changed FIFO candidate order"
    );
    black_box(projection);
    Ok(())
}

fn measure(
    benchmark_id: &'static str,
    repetitions: usize,
    operations_per_sample: usize,
    mut operation: impl FnMut() -> Result<()>,
) -> Result<BenchmarkResult> {
    for _ in 0..WARMUP_SAMPLES {
        operation().with_context(|| format!("warming up {benchmark_id}"))?;
    }

    let mut samples = Vec::with_capacity(repetitions);
    for iteration in 1..=repetitions {
        let resources_before = resource_usage();
        let started_at = Instant::now();
        operation().with_context(|| format!("measuring {benchmark_id}"))?;
        let wall_time = started_at.elapsed();
        let resources_after = resource_usage();
        samples.push(Sample {
            iteration,
            wall_time_ns: wall_time.as_nanos(),
            user_cpu_ns: duration_delta(resources_after.user_cpu, resources_before.user_cpu)
                .as_nanos(),
            system_cpu_ns: duration_delta(resources_after.system_cpu, resources_before.system_cpu)
                .as_nanos(),
            peak_rss_kb: resources_after.peak_rss_kb,
            exit_status: "ok",
        });
    }

    let summary = summarize(&samples, operations_per_sample);
    Ok(BenchmarkResult {
        benchmark_id,
        operations_per_sample,
        samples,
        summary,
    })
}

fn summarize(samples: &[Sample], operations_per_sample: usize) -> Summary {
    let mut values: Vec<u128> = samples.iter().map(|sample| sample.wall_time_ns).collect();
    values.sort_unstable();
    let median_value = median(&values);
    let mut deviations: Vec<u128> = values
        .iter()
        .map(|value| value.abs_diff(median_value))
        .collect();
    deviations.sort_unstable();
    Summary {
        median_wall_time_ns: median_value,
        min_wall_time_ns: values[0],
        max_wall_time_ns: values[values.len() - 1],
        median_absolute_deviation_ns: median(&deviations),
        median_operations_per_second: operations_per_sample as f64 * 1_000_000_000.0
            / median_value as f64,
    }
}

fn median(values: &[u128]) -> u128 {
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2
    } else {
        values[middle]
    }
}

#[derive(Default)]
struct ResourceUsage {
    user_cpu: Duration,
    system_cpu: Duration,
    peak_rss_kb: u64,
}

#[cfg(unix)]
fn resource_usage() -> ResourceUsage {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return ResourceUsage::default();
    }
    // SAFETY: getrusage returned success.
    let usage = unsafe { usage.assume_init() };
    ResourceUsage {
        user_cpu: timeval_duration(usage.ru_utime),
        system_cpu: timeval_duration(usage.ru_stime),
        peak_rss_kb: usage.ru_maxrss as u64,
    }
}

#[cfg(not(unix))]
fn resource_usage() -> ResourceUsage {
    ResourceUsage::default()
}

#[cfg(unix)]
fn timeval_duration(value: libc::timeval) -> Duration {
    Duration::new(value.tv_sec as u64, (value.tv_usec as u32) * 1_000)
}

fn duration_delta(after: Duration, before: Duration) -> Duration {
    after.checked_sub(before).unwrap_or_default()
}

fn environment() -> Environment {
    Environment {
        git_revision: command_output("git", &["rev-parse", "HEAD"]),
        git_dirty: command_output("git", &["status", "--porcelain"])
            .map(|output| !output.is_empty()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        cpu_model: cpu_model(),
        logical_cpus: thread::available_parallelism().ok().map(usize::from),
        rustc_version: command_output("rustc", &["--version"]),
        build_profile: "bench",
        network_mode: "disabled",
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn cpu_model() -> Option<String> {
    let cpu_info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpu_info.lines().find_map(|line| {
        line.strip_prefix("model name")
            .and_then(|value| value.split_once(':'))
            .map(|(_, value)| value.trim().to_string())
    })
}
