use std::{
    collections::BTreeMap,
    hint::black_box,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Utc;
use holon::diagnostics;
use serde::Serialize;

const DEFAULT_SAMPLES: usize = 20;
const DEFAULT_OPERATIONS: usize = 100_000;
const WARMUP_SAMPLES: usize = 3;

#[derive(Serialize)]
struct BenchmarkArtifact {
    schema_version: &'static str,
    benchmark_suite: &'static str,
    workload_version: &'static str,
    started_at: String,
    git_revision: Option<String>,
    git_dirty: Option<bool>,
    config: BenchmarkConfig,
    modes: BTreeMap<&'static str, ModeResult>,
}

#[derive(Serialize)]
struct BenchmarkConfig {
    warmup_repetitions: usize,
    measured_repetitions: usize,
    operations_per_repetition: usize,
}

#[derive(Serialize)]
struct ModeResult {
    available: bool,
    unavailable_reason: Option<&'static str>,
    samples: Vec<Sample>,
    summary: Option<Summary>,
}

#[derive(Clone, Serialize)]
struct Sample {
    iteration: usize,
    elapsed_ns: u128,
    ns_per_operation: f64,
    peak_rss_kb: Option<u64>,
}

#[derive(Serialize)]
struct Summary {
    median_ns_per_operation: f64,
    p95_ns_per_operation: f64,
    max_ns_per_operation: f64,
}

#[derive(Clone, Copy)]
enum Mode {
    Disabled,
    AggregateOnly,
}

fn main() -> Result<()> {
    let samples = positive_env("HOLON_BENCH_SAMPLES", DEFAULT_SAMPLES)?;
    let operations = positive_env("HOLON_BENCH_OPERATIONS", DEFAULT_OPERATIONS)?;

    for _ in 0..WARMUP_SAMPLES {
        measure(Mode::Disabled, 0, operations);
        measure(Mode::AggregateOnly, 0, operations);
    }

    let mut disabled = Vec::with_capacity(samples);
    let mut aggregate_only = Vec::with_capacity(samples);
    for iteration in 0..samples {
        if iteration % 2 == 0 {
            disabled.push(measure(Mode::Disabled, iteration, operations));
            aggregate_only.push(measure(Mode::AggregateOnly, iteration, operations));
        } else {
            aggregate_only.push(measure(Mode::AggregateOnly, iteration, operations));
            disabled.push(measure(Mode::Disabled, iteration, operations));
        }
    }

    let mut modes = BTreeMap::new();
    modes.insert("disabled", available(disabled));
    modes.insert("aggregate_only", available(aggregate_only));
    modes.insert(
        "sampled_local_trace",
        unavailable("implemented in Phase 1 after the recent-trace layer exists"),
    );
    modes.insert(
        "full_trace",
        unavailable("implemented in Phase 3 after bounded persistence exists"),
    );

    let artifact = BenchmarkArtifact {
        schema_version: "holon.observability-overhead.v0",
        benchmark_suite: "observability_overhead",
        workload_version: "atomic_record.v1",
        started_at: Utc::now().to_rfc3339(),
        git_revision: command_output("git", &["rev-parse", "HEAD"]),
        git_dirty: command_output("git", &["status", "--porcelain"])
            .map(|output| !output.is_empty()),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            operations_per_repetition: operations,
        },
        modes,
    };

    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}

fn positive_env(name: &str, default: usize) -> Result<usize> {
    let value = std::env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .with_context(|| format!("{name} must be a positive integer"))?
        .unwrap_or(default);
    anyhow::ensure!(value > 0, "{name} must be positive");
    Ok(value)
}

fn measure(mode: Mode, iteration: usize, operations: usize) -> Sample {
    let started = Instant::now();
    match mode {
        Mode::Disabled => {
            for operation in 0..operations {
                black_box(operation);
            }
        }
        Mode::AggregateOnly => {
            for operation in 0..operations {
                diagnostics::record_turn_total(Duration::from_nanos(black_box(
                    operation as u64 + 1,
                )));
            }
        }
    }
    let elapsed_ns = started.elapsed().as_nanos();
    Sample {
        iteration,
        elapsed_ns,
        ns_per_operation: elapsed_ns as f64 / operations as f64,
        peak_rss_kb: peak_rss_kb(),
    }
}

fn available(samples: Vec<Sample>) -> ModeResult {
    let summary = summarize(&samples);
    ModeResult {
        available: true,
        unavailable_reason: None,
        samples,
        summary: Some(summary),
    }
}

fn unavailable(reason: &'static str) -> ModeResult {
    ModeResult {
        available: false,
        unavailable_reason: Some(reason),
        samples: Vec::new(),
        summary: None,
    }
}

fn summarize(samples: &[Sample]) -> Summary {
    let mut values = samples
        .iter()
        .map(|sample| sample.ns_per_operation)
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    Summary {
        median_ns_per_operation: percentile(&values, 0.50),
        p95_ns_per_operation: percentile(&values, 0.95),
        max_ns_per_operation: *values.last().expect("samples are non-empty"),
    }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let index = ((values.len() - 1) as f64 * quantile).ceil() as usize;
    values[index]
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(unix)]
fn peak_rss_kb() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: getrusage returned success, so usage is initialized.
    let usage = unsafe { usage.assume_init() };
    Some(normalize_peak_rss_kb(usage.ru_maxrss))
}

#[cfg(target_os = "macos")]
fn normalize_peak_rss_kb(value: libc::c_long) -> u64 {
    value as u64 / 1024
}

#[cfg(all(unix, not(target_os = "macos")))]
fn normalize_peak_rss_kb(value: libc::c_long) -> u64 {
    value as u64
}

#[cfg(not(unix))]
fn peak_rss_kb() -> Option<u64> {
    None
}
