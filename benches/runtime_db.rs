use std::{
    hint::black_box,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Utc;
use holon::{
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{AgentState, AuditEvent, BriefKind, BriefRecord},
};
use serde::Serialize;
use tempfile::TempDir;

const AGENT_ID: &str = "benchmark-agent";
const DEFAULT_SAMPLES: usize = 10;
const WARMUP_SAMPLES: usize = 1;

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
    brief_count: usize,
    event_count: usize,
    task_count: usize,
    concurrency: usize,
}

#[derive(Serialize)]
struct BenchmarkResult {
    benchmark_id: &'static str,
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
        benchmark_suite: "runtime_db",
        workload_version: "v1",
        started_at: Utc::now().to_rfc3339(),
        environment: environment(),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            brief_count: 50,
            event_count: 100,
            task_count: 0,
            concurrency: 1,
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
    let root = tempfile::tempdir().context("creating runtime_db benchmark root")?;
    let existing = root.path().join("existing");
    let existing_db = existing.join("runtime.sqlite");
    let existing_lock = existing.join("runtime.lock");
    drop(RuntimeDb::open_and_migrate(&existing_db, &existing_lock)?);

    let projection_root = root.path().join("projection");
    let projection_db = projection_root.join(".holon/state/runtime.sqlite");
    let projection_lock = projection_root.join(".holon/state/runtime.lock");
    let runtime_db = RuntimeDb::open_and_migrate(&projection_db, &projection_lock)?;
    let storage = AppStorage::new_for_agent(&projection_root, AGENT_ID, runtime_db)?;
    let agent = AgentState::new(AGENT_ID);
    storage.write_agent(&agent)?;
    for index in 0..100 {
        storage.append_event(&AuditEvent::legacy(
            format!("benchmark_event_{index}"),
            serde_json::json!({"index": index}),
        ))?;
    }
    for index in 0..50 {
        storage.append_brief(&BriefRecord::new(
            AGENT_ID,
            BriefKind::Result,
            format!("benchmark brief {index}"),
            None,
            None,
        ))?;
    }

    let mut results = Vec::new();
    if benchmark_selected(filter, "runtime_db.open_and_migrate.fresh") {
        results.push(measure(
            "runtime_db.open_and_migrate.fresh",
            samples,
            || {
                let sample_root = TempDir::new_in(root.path())?;
                let db = RuntimeDb::open_and_migrate(
                    sample_root.path().join("runtime.sqlite"),
                    sample_root.path().join("runtime.lock"),
                )?;
                black_box(db.current_schema_version()?);
                Ok(())
            },
        )?);
    }
    if benchmark_selected(filter, "runtime_db.open_and_migrate.existing") {
        results.push(measure(
            "runtime_db.open_and_migrate.existing",
            samples,
            || {
                let db = RuntimeDb::open_and_migrate(&existing_db, &existing_lock)?;
                black_box(db.current_schema_version()?);
                Ok(())
            },
        )?);
    }
    if benchmark_selected(filter, "projection.work_queue.empty") {
        results.push(measure("projection.work_queue.empty", samples, || {
            black_box(storage.work_queue_read_model()?);
            Ok(())
        })?);
    }
    if benchmark_selected(filter, "projection.agent_posture.empty") {
        results.push(measure("projection.agent_posture.empty", samples, || {
            black_box(storage.agent_posture_projection(&agent)?);
            Ok(())
        })?);
    }
    if benchmark_selected(filter, "projection.recent_briefs.50") {
        results.push(measure("projection.recent_briefs.50", samples, || {
            black_box(storage.read_recent_briefs(50)?);
            Ok(())
        })?);
    }
    if benchmark_selected(filter, "projection.recent_events.100") {
        results.push(measure("projection.recent_events.100", samples, || {
            black_box(storage.read_recent_events(100)?);
            Ok(())
        })?);
    }
    if benchmark_selected(
        filter,
        "projection.agent_summary_storage.50_briefs_100_events",
    ) {
        results.push(measure(
            "projection.agent_summary_storage.50_briefs_100_events",
            samples,
            || {
                let active_tasks = storage.active_task_count_for_agent(AGENT_ID)?;
                let work_queue = storage.work_queue_read_model()?;
                let posture =
                    storage.agent_posture_projection_with_work_queue(&agent, &work_queue)?;
                let briefs = storage.read_recent_briefs(50)?;
                let events = storage.read_recent_events(100)?;
                black_box((active_tasks, posture, briefs.len(), events.len()));
                Ok(())
            },
        )?);
    }
    Ok(results)
}

fn measure(
    benchmark_id: &'static str,
    repetitions: usize,
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

    let summary = summarize(&samples);
    Ok(BenchmarkResult {
        benchmark_id,
        samples,
        summary,
    })
}

fn summarize(samples: &[Sample]) -> Summary {
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
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return ResourceUsage::default();
    }
    // SAFETY: getrusage returned success, so usage is initialized.
    let usage = unsafe { usage.assume_init() };
    ResourceUsage {
        user_cpu: timeval_duration(usage.ru_utime),
        system_cpu: timeval_duration(usage.ru_stime),
        peak_rss_kb: peak_rss_kb(usage.ru_maxrss),
    }
}

#[cfg(not(unix))]
fn resource_usage() -> ResourceUsage {
    ResourceUsage::default()
}

#[cfg(unix)]
fn timeval_duration(value: libc::timeval) -> Duration {
    Duration::new(
        value.tv_sec.max(0) as u64,
        (value.tv_usec.max(0) as u32) * 1_000,
    )
}

#[cfg(all(unix, target_os = "macos"))]
fn peak_rss_kb(value: libc::c_long) -> u64 {
    value.max(0) as u64 / 1024
}

#[cfg(all(unix, not(target_os = "macos")))]
fn peak_rss_kb(value: libc::c_long) -> u64 {
    value.max(0) as u64
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
        logical_cpus: std::thread::available_parallelism().ok().map(usize::from),
        rustc_version: command_output("rustc", &["--version"]),
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "bench"
        },
        network_mode: "disabled",
    }
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "linux")]
fn cpu_model() -> Option<String> {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_string))
}

#[cfg(not(target_os = "linux"))]
fn cpu_model() -> Option<String> {
    None
}
