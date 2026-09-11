use std::{
    fs,
    hint::black_box,
    path::Path,
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{Context, Result};
use chrono::Utc;
use holon::{
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{AgentState, AuditEvent, BriefKind, BriefRecord},
};
use serde::{Deserialize, Serialize};

const AGENT_ID: &str = "memory-benchmark-agent";
const DEFAULT_SAMPLES: usize = 5;
const WARMUP_SAMPLES: usize = 1;
const EVENT_COUNT: usize = 1_000;
const EVENT_PAYLOAD_BYTES: usize = 1_024;
const BRIEF_COUNT: usize = 100;
const BRIEF_PAYLOAD_BYTES: usize = 2_048;
const PROJECTION_REPETITIONS: usize = 20;
const WORKER_ENV: &str = "HOLON_MEMORY_BENCH_WORKER";

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
    event_count: usize,
    event_payload_bytes: usize,
    brief_count: usize,
    brief_payload_bytes: usize,
    projection_repetitions: usize,
    process_isolation: bool,
}

#[derive(Serialize)]
struct BenchmarkResult {
    benchmark_id: &'static str,
    samples: Vec<Sample>,
    summary: Summary,
}

#[derive(Serialize)]
struct Sample {
    iteration: usize,
    wall_time_ns: u128,
    baseline_rss_kb: u64,
    after_write_rss_kb: u64,
    after_projection_rss_kb: u64,
    after_drop_rss_kb: u64,
    peak_rss_kb: u64,
    retained_rss_delta_kb: i64,
    peak_rss_delta_kb: u64,
    database_bytes: u64,
    exit_status: &'static str,
}

#[derive(Serialize)]
struct Summary {
    median_wall_time_ns: u128,
    median_retained_rss_delta_kb: i64,
    median_peak_rss_delta_kb: u64,
    median_database_bytes: u64,
}

#[derive(Deserialize, Serialize)]
struct WorkerResult {
    baseline_rss_kb: u64,
    after_write_rss_kb: u64,
    after_projection_rss_kb: u64,
    after_drop_rss_kb: u64,
    peak_rss_kb: u64,
    database_bytes: u64,
}

fn main() -> Result<()> {
    if std::env::var_os(WORKER_ENV).is_some() {
        println!("{}", serde_json::to_string(&run_worker()?)?);
        return Ok(());
    }

    let samples = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be a positive integer")?
        .unwrap_or(DEFAULT_SAMPLES);
    anyhow::ensure!(samples > 0, "HOLON_BENCH_SAMPLES must be positive");

    for _ in 0..WARMUP_SAMPLES {
        black_box(run_isolated_worker()?);
    }

    let mut measured = Vec::with_capacity(samples);
    for iteration in 1..=samples {
        let started = Instant::now();
        let worker = run_isolated_worker()?;
        let retained_rss_delta_kb = worker.after_drop_rss_kb as i64 - worker.baseline_rss_kb as i64;
        measured.push(Sample {
            iteration,
            wall_time_ns: started.elapsed().as_nanos(),
            baseline_rss_kb: worker.baseline_rss_kb,
            after_write_rss_kb: worker.after_write_rss_kb,
            after_projection_rss_kb: worker.after_projection_rss_kb,
            after_drop_rss_kb: worker.after_drop_rss_kb,
            peak_rss_kb: worker.peak_rss_kb,
            retained_rss_delta_kb,
            peak_rss_delta_kb: worker.peak_rss_kb.saturating_sub(worker.baseline_rss_kb),
            database_bytes: worker.database_bytes,
            exit_status: "ok",
        });
    }

    let artifact = BenchmarkArtifact {
        schema_version: "holon.performance.v0",
        benchmark_suite: "memory_lifecycle",
        workload_version: "v1",
        started_at: Utc::now().to_rfc3339(),
        environment: environment(),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            event_count: EVENT_COUNT,
            event_payload_bytes: EVENT_PAYLOAD_BYTES,
            brief_count: BRIEF_COUNT,
            brief_payload_bytes: BRIEF_PAYLOAD_BYTES,
            projection_repetitions: PROJECTION_REPETITIONS,
            process_isolation: true,
        },
        results: vec![BenchmarkResult {
            benchmark_id: "memory.long_session.events_1000_briefs_100",
            summary: summarize(&measured),
            samples: measured,
        }],
    };

    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}

fn run_isolated_worker() -> Result<WorkerResult> {
    let output = Command::new(std::env::current_exe()?)
        .env(WORKER_ENV, "1")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("running isolated memory benchmark worker")?;
    anyhow::ensure!(
        output.status.success(),
        "memory benchmark worker exited with {}",
        output.status
    );
    serde_json::from_slice(&output.stdout).context("parsing memory benchmark worker output")
}

fn run_worker() -> Result<WorkerResult> {
    let root = tempfile::tempdir().context("creating memory benchmark root")?;
    let database_root = root.path().join(".holon/state");
    let runtime_db = RuntimeDb::open_and_migrate(
        database_root.join("runtime.sqlite"),
        database_root.join("runtime.lock"),
    )?;
    let storage = AppStorage::new_for_agent(root.path(), AGENT_ID, runtime_db)?;
    storage.write_agent(&AgentState::new(AGENT_ID))?;
    let baseline_rss_kb = current_rss_kb()?;

    let event_payload = "e".repeat(EVENT_PAYLOAD_BYTES);
    for index in 0..EVENT_COUNT {
        storage.append_event(&AuditEvent::legacy(
            format!("memory_benchmark_event_{index}"),
            serde_json::json!({"index": index, "payload": event_payload}),
        ))?;
    }
    let brief_payload = "b".repeat(BRIEF_PAYLOAD_BYTES);
    for index in 0..BRIEF_COUNT {
        storage.append_brief(&BriefRecord::new(
            AGENT_ID,
            BriefKind::Result,
            format!("memory benchmark brief {index}: {brief_payload}"),
            None,
            None,
        ))?;
    }
    let after_write_rss_kb = current_rss_kb()?;

    for _ in 0..PROJECTION_REPETITIONS {
        let events = storage.read_recent_events(100)?;
        let briefs = storage.read_recent_briefs(50)?;
        black_box((events, briefs));
    }
    let after_projection_rss_kb = current_rss_kb()?;
    let database_bytes = directory_bytes(&database_root)?;

    drop(storage);
    let after_drop_rss_kb = current_rss_kb()?;
    Ok(WorkerResult {
        baseline_rss_kb,
        after_write_rss_kb,
        after_projection_rss_kb,
        after_drop_rss_kb,
        peak_rss_kb: peak_rss_kb()?,
        database_bytes,
    })
}

fn current_rss_kb() -> Result<u64> {
    status_memory_kb("VmRSS:")
}

fn peak_rss_kb() -> Result<u64> {
    status_memory_kb("VmHWM:")
}

fn status_memory_kb(field: &str) -> Result<u64> {
    let status = fs::read_to_string("/proc/self/status").context("reading /proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with(field))
        .with_context(|| format!("{field} is missing from /proc/self/status"))?;
    line.split_whitespace()
        .nth(1)
        .with_context(|| format!("{field} value is missing"))?
        .parse()
        .with_context(|| format!("parsing {field}"))
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn summarize(samples: &[Sample]) -> Summary {
    let mut wall_times = samples
        .iter()
        .map(|sample| sample.wall_time_ns)
        .collect::<Vec<_>>();
    let mut retained = samples
        .iter()
        .map(|sample| sample.retained_rss_delta_kb)
        .collect::<Vec<_>>();
    let mut peak = samples
        .iter()
        .map(|sample| sample.peak_rss_delta_kb)
        .collect::<Vec<_>>();
    let mut database = samples
        .iter()
        .map(|sample| sample.database_bytes)
        .collect::<Vec<_>>();
    wall_times.sort_unstable();
    retained.sort_unstable();
    peak.sort_unstable();
    database.sort_unstable();
    Summary {
        median_wall_time_ns: median(&wall_times),
        median_retained_rss_delta_kb: median(&retained),
        median_peak_rss_delta_kb: median(&peak),
        median_database_bytes: median(&database),
    }
}

fn median<T: Copy>(values: &[T]) -> T {
    values[values.len() / 2]
}

fn environment() -> Environment {
    Environment {
        git_revision: command_output("git", &["rev-parse", "HEAD"]),
        git_dirty: command_output("git", &["status", "--porcelain"])
            .map(|output| !output.is_empty()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        cpu_model: fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))
        }),
        logical_cpus: std::thread::available_parallelism().ok().map(usize::from),
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
