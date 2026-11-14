//! Offline State byte-retention comparison. Never opens the running Holon home.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use holon::{
    config::AppConfig,
    diagnostics::performance_snapshot,
    host::RuntimeHost,
    http::{router, AppState},
    provider::StubProvider,
    runtime::RuntimeHandle,
    types::{AgentStateChangedEvent, AgentStatus, AuditEvent, WorkItemPlanStatus},
};
use serde_json::{json, Value};
use tower::ServiceExt;

// Requested Rust allocations, including client envelopes and fixture background
// work during the measurement window; not SQLite C allocations or live RSS.
struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the allocator contract unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwards the allocator contract unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwards the allocator contract unchanged.
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Debug)]
struct Settings {
    cold: bool,
    agents: usize,
    clients: usize,
    work_items: usize,
    requests: usize,
    batches: usize,
    pause_ms: usize,
    mutation_ms: usize,
}

fn setting(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let value = std::env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .with_context(|| format!("{name} must be an integer"))?
        .unwrap_or(default);
    anyhow::ensure!((min..=max).contains(&value), "{name} outside {min}..={max}");
    Ok(value)
}

impl Settings {
    fn load() -> Result<Self> {
        let mode = std::env::var("HOLON_STATE_MODE").unwrap_or_else(|_| "loaded".into());
        anyhow::ensure!(matches!(mode.as_str(), "loaded" | "cold"), "invalid mode");
        let settings = Self {
            cold: mode == "cold",
            agents: setting("HOLON_STATE_AGENTS", 1, 1, 16)?,
            clients: setting("HOLON_STATE_CLIENTS", 1, 1, 10)?,
            work_items: setting("HOLON_STATE_WORK_ITEMS", 0, 0, 2_000)?,
            requests: setting("HOLON_STATE_REQUESTS", 100, 1, 10_000)?,
            batches: setting("HOLON_STATE_BATCHES", 3, 1, 10)?,
            pause_ms: setting("HOLON_STATE_PAUSE_MS", 10, 0, 1_000)?,
            mutation_ms: setting("HOLON_STATE_MUTATION_MS", 0, 0, 1_000)?,
        };
        anyhow::ensure!(
            settings.mutation_ms == 0 || (!settings.cold && settings.work_items > 0),
            "mutation cells require loaded agents and at least one Work item"
        );
        Ok(settings)
    }
}

struct Fixture {
    // The home outlives host shutdown, including error paths.
    _home: tempfile::TempDir,
    host: RuntimeHost,
    runtimes: Vec<RuntimeHandle>,
    first_work_ids: Vec<String>,
    uris: Vec<String>,
}

async fn shutdown(host: &RuntimeHost, home: tempfile::TempDir) -> Result<tempfile::TempDir> {
    let result = tokio::time::timeout(Duration::from_secs(15), host.shutdown())
        .await
        .context("fixture shutdown timed out")
        .and_then(|result| result);
    if let Err(error) = result {
        let path = home.keep();
        return Err(error.context(format!("fixture home preserved at {}", path.display())));
    }
    Ok(home)
}

impl Fixture {
    async fn await_initial_idle_audit(runtime: &RuntimeHandle) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut poll = tokio::time::interval(Duration::from_millis(1));
            loop {
                poll.tick().await;
                let asleep = runtime.all_events()?.into_iter().any(|event| {
                    event.kind == "agent_state_changed"
                        && serde_json::from_value::<AgentStateChangedEvent>(event.data)
                            .is_ok_and(|state| state.status == AgentStatus::Asleep)
                });
                if asleep {
                    // The persisted tail can be visible before its publication.
                    // A subsequent original Audit ACK serializes behind that
                    // mutation and completes before any gate is warmed.
                    runtime
                        .append_audit_event_for_fixture(AuditEvent::legacy(
                            "state_benchmark_setup_barrier",
                            json!({}),
                        ))
                        .await?;
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .context("fixture initial idle Audit did not finish within five seconds")?
    }

    async fn build(settings: &Settings) -> Result<Self> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let config = AppConfig::load_with_home(Some(home.path().to_owned()))?;
        let host =
            RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("done")))?;
        let setup = async {
            let mut runtimes = Vec::new();
            let mut first_work_ids = Vec::new();
            let mut uris = Vec::new();
            for index in 0..settings.agents {
                let id = format!("state-bench-{index:02}");
                host.create_public_named_agent(&id, None, None, None)
                    .await?;
                let runtime = host.get_or_create_agent(&id).await?;
                for work in 0..settings.work_items {
                    let record = runtime
                        .create_work_item(
                            format!("Synthetic Work {work:03}"),
                            Some(WorkItemPlanStatus::NeedsInput),
                            None,
                            Vec::new(),
                        )
                        .await?;
                    if work == 0 {
                        first_work_ids.push(record.id);
                    }
                }
                uris.push(format!("/api/agents/{id}/state"));
                runtimes.push(runtime);
            }
            for runtime in &runtimes {
                Self::await_initial_idle_audit(runtime).await?;
            }
            Ok::<_, anyhow::Error>((runtimes, first_work_ids, uris))
        }
        .await;
        let (runtimes, first_work_ids, uris) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _home = shutdown(&host, home).await?;
                return Err(error);
            }
        };
        let (host, runtimes, home) = if settings.cold {
            let home = shutdown(&host, home).await?;
            drop(runtimes);
            drop(host);
            (
                RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?,
                Vec::new(),
                home,
            )
        } else {
            (host, runtimes, home)
        };
        Ok(Self {
            _home: home,
            host,
            runtimes,
            first_work_ids,
            uris,
        })
    }
}

#[derive(Default)]
struct Resources {
    cpu_us: u64,
    peak_rss_kib: u64,
    allocations: u64,
    allocated_bytes: u64,
}

fn resources() -> Resources {
    let mut result = Resources {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        ..Resources::default()
    };
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage writes an initialized rusage on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            // SAFETY: the preceding syscall succeeded.
            let usage = unsafe { usage.assume_init() };
            result.cpu_us = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
                + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64;
            result.peak_rss_kib = usage.ru_maxrss as u64;
            if cfg!(target_os = "macos") {
                result.peak_rss_kib /= 1024;
            }
        }
    }
    result
}

struct Sample {
    latency_ns: u64,
    status: String,
    bytes: usize,
}

async fn request(app: Router, uri: &str) -> Result<(Sample, axum::body::Bytes)> {
    let started = Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(15), async {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await
            .expect("router is infallible");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
        Ok::<_, anyhow::Error>((status, bytes))
    })
    .await
    .context("outer request deadline exhausted")??;
    if !response.0.is_success() {
        eprintln!(
            "{uri}: HTTP {}: {}",
            response.0,
            String::from_utf8_lossy(&response.1[..response.1.len().min(2048)])
        );
    }
    Ok((
        Sample {
            latency_ns: started.elapsed().as_nanos() as u64,
            status: response.0.as_u16().to_string(),
            bytes: response.1.len(),
        },
        response.1,
    ))
}

fn percentile(values: &mut [u64], percent: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[(values.len() * percent).div_ceil(100) - 1])
}

async fn measure(fixture: &Fixture, settings: &Settings) -> Result<Value> {
    let app = router(AppState::for_unix(fixture.host.clone()));
    // Validate and warm the real shared gate, not a replacement per request.
    let mut warmup_retries = 0usize;
    for uri in &fixture.uris {
        let (sample, body) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (sample, body) = request(app.clone(), uri).await?;
                let retryable = sample.status == "503"
                    && serde_json::from_slice::<Value>(&body)
                        .is_ok_and(|value| value["retryable"] == true);
                if !retryable {
                    break Ok::<_, anyhow::Error>((sample, body));
                }
                // Setup writes may still be publishing; never retry measured reads.
                warmup_retries += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("fixture warmup did not become ready within five seconds")??;
        anyhow::ensure!(
            sample.status == "200",
            "warmup {}: {}",
            sample.status,
            String::from_utf8_lossy(&body)
        );
        let value: Value = serde_json::from_slice(&body)?;
        anyhow::ensure!(
            value["active_work_item_count"].as_u64() == Some(settings.work_items as u64),
            "Work fixture count mismatch: {value}"
        );
    }
    let mut batches = Vec::new();
    for batch in 0..settings.batches {
        let performance_before = performance_snapshot();
        let before = resources();
        let started = Instant::now();
        let next = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (done, mut finished) = tokio::sync::oneshot::channel();
        let reads = async {
            let mut workers = tokio::task::JoinSet::new();
            for _ in 0..settings.clients {
                let app = app.clone();
                let next = next.clone();
                let stopped = stopped.clone();
                let uris = fixture.uris.clone();
                let count = settings.requests;
                let pause = Duration::from_millis(settings.pause_ms as u64);
                workers.spawn(async move {
                    let mut samples = Vec::new();
                    while !stopped.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            break;
                        }
                        let (sample, _) = request(app.clone(), &uris[index % uris.len()]).await?;
                        let failed = sample.status != "200";
                        samples.push(sample);
                        if failed {
                            stopped.store(true, Ordering::Relaxed);
                            break;
                        }
                        tokio::time::sleep(pause).await;
                    }
                    Ok::<_, anyhow::Error>(samples)
                });
            }
            let result = async {
                let mut samples = Vec::new();
                while let Some(result) = workers.join_next().await {
                    samples.extend(result??);
                }
                Ok::<_, anyhow::Error>(samples)
            }
            .await;
            let _ = done.send(());
            result
        };
        let writes = async {
            let mut count = 0usize;
            if settings.mutation_ms != 0 {
                loop {
                    tokio::select! {
                        _ = &mut finished => break,
                        _ = tokio::time::sleep(Duration::from_millis(settings.mutation_ms as u64)) => {
                            let index = count % fixture.runtimes.len();
                            fixture.runtimes[index]
                                .update_work_item_fields(
                                    fixture.first_work_ids[index].clone(),
                                    Some(format!("Synthetic mutation {count:08}")),
                                    None, None, None, None,
                                )
                                .await?;
                            count += 1;
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(count)
        };
        let (samples, mutations) = tokio::join!(reads, writes);
        let samples = samples?;
        let mutations = mutations?;
        let after = resources();
        let elapsed = started.elapsed();
        let performance_after = performance_snapshot();
        let mut statuses = BTreeMap::new();
        let mut latency = Vec::new();
        let mut sizes = Vec::new();
        for sample in &samples {
            *statuses.entry(sample.status.clone()).or_insert(0usize) += 1;
            if sample.status == "200" {
                latency.push(sample.latency_ns);
                sizes.push(sample.bytes as u64);
            }
        }
        let failed =
            latency.len() != settings.requests || (settings.mutation_ms != 0 && mutations == 0);
        let stages = performance_after
            .attribution
            .iter()
            .filter_map(|stage| {
                let before = performance_before
                    .attribution
                    .iter()
                    .find(|before| before.name == stage.name)?;
                let count = stage.count.saturating_sub(before.count);
                (count > 0).then(|| {
                    json!({
                        "name": stage.name, "count": count,
                        "inclusive_total_ns": stage.total_ns.saturating_sub(before.total_ns),
                        "rows": stage.rows.saturating_sub(before.rows),
                    })
                })
            })
            .collect::<Vec<_>>();
        batches.push(json!({
            "batch": batch + 1, "attempted": samples.len(), "statuses": statuses,
            "committed_work_mutations": mutations,
            "aborted": failed, "wall_ns": elapsed.as_nanos(),
            "http_p50_ns": percentile(&mut latency, 50),
            "http_p95_ns": percentile(&mut latency, 95),
            "http_p99_ns": if settings.batches >= 3 && latency.len() >= 1000 { percentile(&mut latency, 99) } else { None },
            "response_bytes_p50": percentile(&mut sizes, 50),
            "response_bytes_p95": percentile(&mut sizes, 95),
            "process_cpu_us": after.cpu_us.saturating_sub(before.cpu_us),
            "process_peak_rss_kib": after.peak_rss_kib,
            "rust_allocations": after.allocations.saturating_sub(before.allocations),
            "rust_requested_bytes": after.allocated_bytes.saturating_sub(before.allocated_bytes),
            "gate_before": performance_before.projection_gate,
            "gate_after": performance_after.projection_gate,
            "inclusive_stage_delta": stages,
            "projection_metrics_before": performance_before.projections,
            "projection_metrics_after": performance_after.projections,
        }));
        if failed {
            break;
        }
    }
    Ok(json!({
        "schema": "holon.state-http.v1",
        "source_sha256": option_env!("HOLON_BENCH_SOURCE_SHA256"),
        "label": std::env::var("HOLON_STATE_LABEL").unwrap_or_default(),
        "warmup_retries": warmup_retries,
        "settings": {
            "mode": if settings.cold { "cold" } else { "loaded" },
            "agents": settings.agents, "clients": settings.clients,
            "work_items_per_agent": settings.work_items,
            "requests_per_batch": settings.requests, "batches": settings.batches,
            "pause_ms_per_client": settings.pause_ms, "mutation_delay_ms_after_each_commit": settings.mutation_ms,
        },
        "batches": batches,
        "limitations": [
            "In-process Router::oneshot; no socket or transfer cost.",
            "CPU/allocations include client envelopes and fixture background work.",
            "Rust requested bytes are not C/SQLite allocations or retained heap.",
            "Peak RSS is process lifetime high-water, including fixture setup.",
            "Synthetic non-runnable Work items; no provider requests.",
            "Mutation cells include writer CPU/allocation, reported separately from idle cells.",
            "Stage totals are inclusive and overlap; do not add them as exclusive CPU.",
        ],
    }))
}

fn main() -> Result<()> {
    let settings = Settings::load()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let artifact = runtime.block_on(async {
        let fixture = Fixture::build(&settings).await?;
        let result = measure(&fixture, &settings).await;
        // Do not remove a home while its owner/indexer still uses it.
        let _home = shutdown(&fixture.host, fixture._home).await?;
        result
    })?;
    let failed = artifact["batches"]
        .as_array()
        .context("missing batches")?
        .iter()
        .any(|batch| batch["aborted"] == true);
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    anyhow::ensure!(
        !failed,
        "measurement aborted; preserve the emitted artifact"
    );
    Ok(())
}
