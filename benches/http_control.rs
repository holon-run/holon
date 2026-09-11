use std::{
    hint::black_box,
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use chrono::Utc;
use holon::{
    config::AppConfig,
    host::RuntimeHost,
    http::{router, AppState},
    provider::StubProvider,
};
use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const DEFAULT_SAMPLES: usize = 10;
const WARMUP_SAMPLES: usize = 1;
const EXTRA_AGENTS: usize = 100;
const WORK_ITEMS: usize = 200;
const PERFORMANCE_REQUESTS_PER_SAMPLE: usize = 100;
const WORK_ITEM_REQUESTS_PER_SAMPLE: usize = 10;

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
    extra_agents: usize,
    work_items: usize,
    performance_requests_per_sample: usize,
    work_item_requests_per_sample: usize,
    transport: &'static str,
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

struct HttpFixture {
    _home: TempDir,
    app: Router,
    host: RuntimeHost,
    default_agent_id: String,
}

fn main() -> Result<()> {
    let samples = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be a positive integer")?
        .unwrap_or(DEFAULT_SAMPLES);
    anyhow::ensure!(samples > 0, "HOLON_BENCH_SAMPLES must be positive");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building HTTP benchmark runtime")?;
    let fixture = runtime.block_on(build_fixture())?;
    let filter = benchmark_filter();
    let results = run_benchmarks(&runtime, &fixture, samples, &filter)?;
    anyhow::ensure!(
        !results.is_empty(),
        "HOLON_BENCH_FILTER selected no workloads"
    );
    let artifact = BenchmarkArtifact {
        schema_version: "holon.performance.v0",
        benchmark_suite: "http_control",
        workload_version: "v1",
        started_at: Utc::now().to_rfc3339(),
        environment: environment(),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            extra_agents: EXTRA_AGENTS,
            work_items: WORK_ITEMS,
            performance_requests_per_sample: PERFORMANCE_REQUESTS_PER_SAMPLE,
            work_item_requests_per_sample: WORK_ITEM_REQUESTS_PER_SAMPLE,
            transport: "tower.oneshot.in_process",
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

async fn build_fixture() -> Result<HttpFixture> {
    let home = tempfile::tempdir().context("creating HTTP benchmark home")?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let default_agent_id = config.default_agent_id.clone();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;

    for index in 0..EXTRA_AGENTS {
        host.create_public_named_agent(&format!("http-bench-agent-{index:03}"), None, None, None)
            .await?;
    }

    let runtime = host.get_or_create_agent(&default_agent_id).await?;
    for index in 0..WORK_ITEMS {
        runtime
            .create_work_item(
                format!("HTTP benchmark work item {index}"),
                None,
                None,
                Vec::new(),
            )
            .await?;
    }

    let app = router(AppState::for_unix(host.clone()));
    validate_json_array(&app, "/api/agents/list", EXTRA_AGENTS + 1).await?;
    validate_json_array(
        &app,
        &format!("/api/agents/{default_agent_id}/work-items?limit={WORK_ITEMS}"),
        WORK_ITEMS,
    )
    .await?;
    validate_json_object(&app, "/api/control/runtime/performance").await?;

    Ok(HttpFixture {
        _home: home,
        app,
        host,
        default_agent_id,
    })
}

fn run_benchmarks(
    runtime: &tokio::runtime::Runtime,
    fixture: &HttpFixture,
    samples: usize,
    filter: &[String],
) -> Result<Vec<BenchmarkResult>> {
    let work_items_uri = format!(
        "/api/agents/{}/work-items?limit={WORK_ITEMS}",
        fixture.default_agent_id
    );
    let mut results = Vec::new();
    for (uri, miss_id, hit_id) in [
        (
            "/api/agents/list".to_owned(),
            "http.agents_list.gate_miss",
            "http.agents_list.gate_hit",
        ),
        (
            format!("/api/agents/{}/state", fixture.default_agent_id),
            "http.agent_state.gate_miss",
            "http.agent_state.gate_hit",
        ),
    ] {
        if benchmark_selected(filter, miss_id) || benchmark_selected(filter, hit_id) {
            let pair = measure_projection_gate(runtime, fixture, &uri, samples, miss_id, hit_id)?;
            results.extend(
                pair.into_iter()
                    .filter(|result| benchmark_selected(filter, result.benchmark_id)),
            );
        }
    }
    if benchmark_selected(filter, "http.runtime_performance.100") {
        results.push(measure(
            "http.runtime_performance.100",
            samples,
            PERFORMANCE_REQUESTS_PER_SAMPLE,
            || {
                runtime.block_on(repeat_request(
                    &fixture.app,
                    "/api/control/runtime/performance",
                    PERFORMANCE_REQUESTS_PER_SAMPLE,
                ))
            },
        )?);
    }
    if benchmark_selected(filter, "http.agents_list.101") {
        results.push(measure("http.agents_list.101", samples, 1, || {
            runtime.block_on(repeat_request(&fixture.app, "/api/agents/list", 1))
        })?);
    }
    if benchmark_selected(filter, "http.work_items.200") {
        results.push(measure("http.work_items.200", samples, 1, || {
            runtime.block_on(repeat_request(&fixture.app, &work_items_uri, 1))
        })?);
    }
    if benchmark_selected(filter, "http.work_items.200x10") {
        results.push(measure(
            "http.work_items.200x10",
            samples,
            WORK_ITEM_REQUESTS_PER_SAMPLE,
            || {
                runtime.block_on(repeat_request(
                    &fixture.app,
                    &work_items_uri,
                    WORK_ITEM_REQUESTS_PER_SAMPLE,
                ))
            },
        )?);
    }
    Ok(results)
}

// Reset only the HTTP byte cache, not SQLite/OS or host caches. Constructing the
// router and checking gate counters happen outside the measured requests.
fn measure_projection_gate(
    runtime: &tokio::runtime::Runtime,
    fixture: &HttpFixture,
    uri: &str,
    repetitions: usize,
    miss_id: &'static str,
    hit_id: &'static str,
) -> Result<Vec<BenchmarkResult>> {
    let mut samples = [Vec::new(), Vec::new()];
    for iteration in 0..(repetitions + WARMUP_SAMPLES) {
        let app = router(AppState::for_unix(fixture.host.clone()));
        for (index, bucket) in samples.iter_mut().enumerate() {
            let before = holon::diagnostics::performance_snapshot().projection_gate;
            let resources_before = resource_usage();
            let started = Instant::now();
            runtime.block_on(repeat_request(&app, uri, 1))?;
            let wall_time = started.elapsed();
            let resources_after = resource_usage();
            let after = holon::diagnostics::performance_snapshot().projection_gate;
            if index == 0 {
                anyhow::ensure!(after.leaders == before.leaders + 1, "expected cache miss");
            } else {
                anyhow::ensure!(
                    after.cache_hits == before.cache_hits + 1 && after.leaders == before.leaders,
                    "expected immediate cache hit"
                );
            }
            if iteration >= WARMUP_SAMPLES {
                bucket.push(Sample {
                    iteration: iteration - WARMUP_SAMPLES + 1,
                    wall_time_ns: wall_time.as_nanos(),
                    user_cpu_ns: duration_delta(
                        resources_after.user_cpu,
                        resources_before.user_cpu,
                    )
                    .as_nanos(),
                    system_cpu_ns: duration_delta(
                        resources_after.system_cpu,
                        resources_before.system_cpu,
                    )
                    .as_nanos(),
                    peak_rss_kb: resources_after.peak_rss_kb,
                    exit_status: "ok",
                });
            }
        }
    }
    Ok([miss_id, hit_id]
        .into_iter()
        .zip(samples)
        .map(|(benchmark_id, samples)| BenchmarkResult {
            benchmark_id,
            operations_per_sample: 1,
            summary: summarize(&samples, 1),
            samples,
        })
        .collect())
}

async fn repeat_request(app: &Router, uri: &str, repetitions: usize) -> Result<()> {
    for _ in 0..repetitions {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await
            .expect("axum router is infallible");
        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "{uri} returned {}",
            response.status()
        );
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        anyhow::ensure!(!body.is_empty(), "{uri} returned an empty body");
        black_box(body);
    }
    Ok(())
}

async fn validate_json_array(app: &Router, uri: &str, expected_len: usize) -> Result<()> {
    let value = request_json(app, uri).await?;
    anyhow::ensure!(
        value.as_array().map(Vec::len) == Some(expected_len),
        "{uri} returned an unexpected array length"
    );
    Ok(())
}

async fn validate_json_object(app: &Router, uri: &str) -> Result<()> {
    let value = request_json(app, uri).await?;
    anyhow::ensure!(value.is_object(), "{uri} did not return a JSON object");
    Ok(())
}

async fn request_json(app: &Router, uri: &str) -> Result<Value> {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty())?)
        .await
        .expect("axum router is infallible");
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "{uri} returned {}",
        response.status()
    );
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    serde_json::from_slice(&body).with_context(|| format!("parsing JSON response from {uri}"))
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
