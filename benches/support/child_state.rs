//! Opt-in, synthetic, uncached parent /state requests through the real scoped
//! projection. No daemon, network provider, or production database is used.
//!
//! HOLON_BENCH_FILTER=http.child_state.loaded.0,http.child_state.loaded.1,http.child_state.loaded.50
//! HOLON_BENCH_CHILD_HISTORY=0 (0..=100000; large history is never the default)
//! HOLON_BENCH_CHILD_REQUESTS=1 (requests per measured batch)
//! HOLON_BENCH_CHILD_BUDGET_MS=60000 (per cell, including warmup, excluding setup)
//! HOLON_BENCH_CHILD_TIMEOUT_MS=15000 (outer timeout, not a gate budget override)
//! HOLON_BENCH_COMPARISON=AA|ABBA HOLON_BENCH_POSITION=1..sequence length
//! HOLON_BENCH_CHILD_CONCURRENCY=1 (closed-loop workers sharing one AppState/gate)
//! HOLON_BENCH_CHILD_ARRIVAL_PER_SEC=0 (0 = closed loop; >0 = bounded-rate saturation)
//! HOLON_BENCH_CHILD_MAX_INFLIGHT=64 (in-flight bound for arrival-rate cells)
//! HOLON_BENCH_CHILD_ROUTE_MIX=state|snapshot|mixed
//! HOLON_BENCH_CHILD_PARENTS=1 (>1 = distinct state keys across parents)
//! HOLON_BENCH_CHILD_LOAD_MODE=loaded|unloaded|mixed
//! Run each position separately with the corresponding old/new executable;
//! labels alone neither switch implementations nor establish an improvement.

use std::{collections::BTreeMap, hint::black_box, sync::Arc, time::Duration, time::Instant};

use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use chrono::{DateTime, Utc};
use holon::{
    config::AppConfig,
    diagnostics::{attribution::StageSnapshot, performance_snapshot},
    host::RuntimeHost,
    http::{router, AppState},
    http_dto::AgentStateSnapshotDto,
    provider::StubProvider,
    runtime::RuntimeHandle,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        TaskKind, TaskRecord, TaskStatus,
    },
};
use rusqlite::{params, Connection, OpenFlags, StatementStatus};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const ACTIVE_PER_CHILD: usize = 3;
const ALL_TASKS_SQL: &str = "SELECT payload_json FROM tasks
    ORDER BY updated_at DESC, created_at DESC, task_id ASC";
// Keep this independent probe equivalent to TaskRepository::query_for_agent.
const ACTIVE_TASKS_SQL: &str = "SELECT payload_json FROM tasks
    WHERE owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')
    ORDER BY updated_at DESC, created_at DESC, task_id ASC LIMIT 9223372036854775807";

struct Settings {
    history: usize,
    requests: usize,
    budget: Duration,
    timeout: Duration,
    comparison: String,
    position: usize,
    concurrency: usize,
    max_inflight: usize,
    arrival_per_sec: usize,
    route_mix: &'static str,
    parents: usize,
    load_mode: &'static str,
}

impl Settings {
    fn load() -> Result<Self> {
        let comparison = std::env::var("HOLON_BENCH_COMPARISON").unwrap_or_else(|_| "AA".into());
        anyhow::ensure!(
            matches!(comparison.as_str(), "AA" | "ABBA"),
            "HOLON_BENCH_COMPARISON must be AA or ABBA"
        );
        let position = setting("HOLON_BENCH_POSITION", 1, 1, comparison.len())?;
        let choice =
            |name: &str, default: &str, options: &[&'static str]| -> Result<&'static str> {
                let value = std::env::var(name).unwrap_or_else(|_| default.to_owned());
                let selected = options.iter().find(|option| **option == value);
                anyhow::ensure!(
                    selected.is_some(),
                    "{name} must be one of {options:?}, got {value}"
                );
                Ok(selected.copied().unwrap())
            };
        Ok(Self {
            history: setting("HOLON_BENCH_CHILD_HISTORY", 0, 0, 100_000)?,
            requests: setting("HOLON_BENCH_CHILD_REQUESTS", 1, 1, 1_000)?,
            budget: Duration::from_millis(setting(
                "HOLON_BENCH_CHILD_BUDGET_MS",
                60_000,
                1,
                300_000,
            )? as u64),
            timeout: Duration::from_millis(setting(
                "HOLON_BENCH_CHILD_TIMEOUT_MS",
                15_000,
                1,
                60_000,
            )? as u64),
            comparison,
            position,
            concurrency: setting("HOLON_BENCH_CHILD_CONCURRENCY", 1, 1, 64)?,
            max_inflight: setting("HOLON_BENCH_CHILD_MAX_INFLIGHT", 64, 1, 1024)?,
            arrival_per_sec: setting("HOLON_BENCH_CHILD_ARRIVAL_PER_SEC", 0, 0, 10_000)?,
            route_mix: choice(
                "HOLON_BENCH_CHILD_ROUTE_MIX",
                "state",
                &["state", "snapshot", "mixed"],
            )?,
            parents: setting("HOLON_BENCH_CHILD_PARENTS", 1, 1, 64)?,
            load_mode: choice(
                "HOLON_BENCH_CHILD_LOAD_MODE",
                "loaded",
                &["loaded", "unloaded", "mixed"],
            )?,
        })
    }
}

fn setting(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let value = std::env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .with_context(|| format!("parsing {name}"))?
        .unwrap_or(default);
    anyhow::ensure!((min..=max).contains(&value), "{name} must be {min}..={max}");
    Ok(value)
}

pub(super) fn run(
    runtime: &tokio::runtime::Runtime,
    samples: usize,
    filter: &[String],
) -> Result<Vec<Value>> {
    // Explicit selection is required, even when the legacy suite has no filter.
    let mut selected = filter
        .iter()
        .filter_map(|id| id.strip_prefix("http.child_state."))
        .filter_map(|rest| rest.split_once('.').map(|(_, children)| children))
        .filter_map(|children| children.parse::<usize>().ok())
        .filter(|children| *children <= 1_000)
        .collect::<Vec<_>>();
    selected.sort_unstable();
    selected.dedup();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let settings = Settings::load()?;
    let planned = samples
        .checked_mul(settings.requests)
        .context("child-state request count overflow")?;
    anyhow::ensure!(planned <= 100_000, "at most 100000 requests per cell");
    let mut results = Vec::new();
    for &children in &selected {
        let fixture = runtime.block_on(Fixture::build(
            children,
            settings.history,
            settings.parents,
            settings.load_mode,
        ))?;
        let measured = measure(runtime, &fixture, children, samples, &settings);
        runtime.block_on(shutdown_host(&fixture.host))?;
        let mut result = measured?;
        result["fixture_cleanup"] = json!({
            "shutdown_completed": true,
            "timeout_ms": 15_000,
            "timed": false,
        });
        result["requested_loaded_child_cells"] = json!(selected);
        let aborted = result["load"]["aborted"] == true;
        results.push(result);
        if aborted {
            break;
        }
    }
    Ok(results)
}

async fn shutdown_host(host: &RuntimeHost) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), host.shutdown())
        .await
        .context("child-state fixture shutdown timed out; refusing another cell")??;
    Ok(())
}

struct Fixture {
    _home: TempDir,
    // Holding each handle makes the loaded branch deterministic; no work is
    // scheduled and no provider turns are started by these synthetic tasks.
    _loaded: Vec<RuntimeHandle>,
    host: RuntimeHost,
    state_uris: Vec<String>,
    child_prefixes: Vec<String>,
    snapshot_uri: &'static str,
    parents: usize,
    load_mode: &'static str,
}

impl Fixture {
    async fn build(
        children: usize,
        history: usize,
        parents: usize,
        load_mode: &'static str,
    ) -> Result<Self> {
        let parent_ids = (0..parents)
            .map(|index| {
                if index == 0 {
                    "child-state-parent".to_owned()
                } else {
                    format!("child-state-parent-{index:02}")
                }
            })
            .collect::<Vec<_>>();
        let child_prefixes = (0..parents)
            .map(|index| {
                if index == 0 {
                    "child-state-".to_owned()
                } else {
                    format!("child-state-p{index:02}-")
                }
            })
            .collect::<Vec<_>>();
        let home = tempfile::tempdir().context("creating child-state benchmark home")?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        let bootstrap = async {
            for id in &parent_ids {
                host.create_public_named_agent(id, None, None, None).await?;
            }
            let parent_id = host
                .configured_default_agent_id()?
                .context("child-state fixture has no default agent")?;
            let mut child_ids = Vec::new();
            for (parent, prefix) in child_prefixes.iter().enumerate() {
                for index in 0..children {
                    let id = format!("{prefix}{index:03}");
                    host.runtime_db()
                        .agent_identities()
                        .upsert(&AgentIdentityRecord::new(
                            &id,
                            AgentKind::Child,
                            AgentVisibility::Private,
                            AgentOwnership::ParentSupervised,
                            AgentProfilePreset::PrivateChild,
                            Some(parent_ids[parent].clone()),
                            None,
                        ))?;
                    child_ids.push(id);
                }
            }
            Ok::<_, anyhow::Error>((parent_id, child_ids, (*host.config()).clone()))
        }
        .await;
        shutdown_host(&host).await?;
        let (parent_id, child_ids, config) = bootstrap?;
        // Direct fixture writes do not refresh the host's identity roster.
        // Reopen it before activation, just as a normal host startup would.
        drop(host);
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        let initialized = async {
            let mut loaded = vec![host.get_or_create_agent(&parent_id).await?];
            for id in &parent_ids {
                if id != &parent_id {
                    loaded.push(host.get_or_create_agent(id).await?);
                }
            }
            // loaded keeps every child activated; unloaded keeps every child
            // storage-backed; mixed activates only the first half.
            let activate = |index: usize| match load_mode {
                "loaded" => true,
                "unloaded" => false,
                _ => index < child_ids.len().div_ceil(2),
            };
            for (index, id) in child_ids.iter().enumerate() {
                if activate(index) {
                    loaded.push(host.get_or_create_agent(id).await?);
                }
            }
            // Seed after activation so task-recovery cannot consume synthetic work.
            // A single transaction avoids 100000 setup fsyncs. Only this temporary
            // database is written; columns mirror the task repository's wire shape.
            let created_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse()?;
            host.runtime_db().transaction(|tx| {
                let mut statement = tx.prepare(
                    "INSERT INTO tasks (task_id, owner_agent_id, kind, status, summary,
                 revision, created_at, updated_at, completed_at, payload_json)
                 VALUES (?1, ?2, 'command_task', ?3, ?4, 1, ?5, ?5, ?6, ?7)",
                )?;
                for (owner, count, terminal) in child_ids
                    .iter()
                    .map(|id| (id.as_str(), ACTIVE_PER_CHILD, false))
                    .chain(std::iter::once(("unrelated-history-owner", history, true)))
                {
                    for index in 0..count {
                        let status = if terminal {
                            TaskStatus::Completed
                        } else {
                            [
                                TaskStatus::Queued,
                                TaskStatus::Running,
                                TaskStatus::Cancelling,
                            ][index % ACTIVE_PER_CHILD]
                                .clone()
                        };
                        let record = TaskRecord {
                            id: format!("{owner}-task-{index:06}"),
                            agent_id: owner.to_owned(),
                            kind: TaskKind::CommandTask,
                            status,
                            created_at,
                            updated_at: created_at,
                            parent_message_id: None,
                            work_item_id: None,
                            summary: Some("synthetic child-state task".into()),
                            detail: None,
                            recovery: None,
                        };
                        statement.execute(params![
                            record.id,
                            record.agent_id,
                            serde_json::to_value(&record.status)?.as_str(),
                            record.summary,
                            created_at.to_rfc3339(),
                            terminal.then(|| created_at.to_rfc3339()),
                            serde_json::to_string(&record)?,
                        ])?;
                    }
                }
                Ok(())
            })?;
            Ok::<_, anyhow::Error>(loaded)
        }
        .await;
        let loaded = match initialized {
            Ok(loaded) => loaded,
            Err(error) => {
                shutdown_host(&host).await?;
                return Err(error);
            }
        };
        Ok(Self {
            _home: home,
            _loaded: loaded,
            state_uris: parent_ids
                .iter()
                .map(|id| format!("/api/agents/{id}/state"))
                .collect(),
            child_prefixes,
            snapshot_uri: "/api/agents/snapshot",
            parents,
            load_mode,
            host,
        })
    }
}

fn stages() -> Vec<StageSnapshot> {
    performance_snapshot().attribution
}

fn stage_delta(before: &[StageSnapshot], after: &[StageSnapshot]) -> Vec<Value> {
    after
        .iter()
        .filter_map(|after| {
            let before = before.iter().find(|stage| stage.name == after.name)?;
            let count = after.count.saturating_sub(before.count);
            // Keep zero-count child stages visible when sorting is removed.
            (count > 0 || after.name.starts_with("projection.child.")).then(|| {
                json!({
                    "name": after.name,
                    "count": count,
                    "inclusive_total_ns": after.total_ns.saturating_sub(before.total_ns),
                    "rows": after.rows.saturating_sub(before.rows),
                })
            })
        })
        .collect()
}

fn validate_children(value: &Value, children: usize, prefix: &str) -> Result<()> {
    let snapshot: AgentStateSnapshotDto =
        serde_json::from_value(value.clone()).context("invalid parent /state DTO")?;
    let actual = snapshot.agent.active_children;
    anyhow::ensure!(actual.len() == children, "unexpected child count");
    for (index, child) in actual.iter().enumerate() {
        anyhow::ensure!(
            child.identity.agent_id == format!("{prefix}{index:03}")
                && child.active_task_count == ACTIVE_PER_CHILD,
            "unexpected child identity, active task count, or observability"
        );
    }
    Ok(())
}

fn validate_snapshot(value: &Value, parents: usize) -> Result<()> {
    let agents = value["agents"]
        .as_array()
        .context("roster snapshot missing agents array")?;
    anyhow::ensure!(
        agents.len() == parents,
        "unexpected roster agent count {} (expected {parents} public parents)",
        agents.len()
    );
    Ok(())
}

struct RequestSpec {
    uri: String,
    route: &'static str,
    parent: usize,
}

fn spec_from(
    state_uris: &[String],
    snapshot_uri: &str,
    parents: usize,
    route_mix: &'static str,
    index: usize,
) -> RequestSpec {
    let route = match route_mix {
        "mixed" => {
            if index % 2 == 0 {
                "state"
            } else {
                "snapshot"
            }
        }
        other => other,
    };
    let parent = if route == "state" && parents > 1 {
        index % parents
    } else {
        0
    };
    let uri = if route == "state" {
        state_uris[parent].clone()
    } else {
        snapshot_uri.to_owned()
    };
    RequestSpec { uri, route, parent }
}

fn request(
    runtime: &tokio::runtime::Runtime,
    fixture: &Fixture,
    children: usize,
    timeout: Duration,
    spec: &RequestSpec,
    shared_app: Option<&Router>,
) -> Result<Value> {
    // A fresh AppState resets only the byte/projection gate cache. The same
    // loaded host, database, and OS/SQLite caches are reused across requests.
    let fresh_app;
    let app = match shared_app {
        Some(app) => app.clone(),
        None => {
            fresh_app = router(AppState::for_unix(fixture.host.clone()));
            fresh_app.clone()
        }
    };
    let before = stages();
    let gate_before = performance_snapshot().projection_gate;
    let resources_before = super::resource_usage();
    let started = Instant::now();
    let response = runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            let response = app
                .oneshot(Request::builder().uri(&spec.uri).body(Body::empty())?)
                .await
                .expect("axum router is infallible");
            let status = response.status().as_u16();
            let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
                .await?
                .to_vec();
            Ok::<_, anyhow::Error>((status, body))
        })
        .await
    });
    let elapsed = started.elapsed().as_nanos();
    let resources_after = super::resource_usage();
    let after = stages();
    let gate_after = performance_snapshot().projection_gate;
    let gate_delta = json!({
        "leaders": gate_after.leaders.saturating_sub(gate_before.leaders),
        "joined_waiters": gate_after.joined_waiters.saturating_sub(gate_before.joined_waiters),
        "cache_hits": gate_after.cache_hits.saturating_sub(gate_before.cache_hits),
        "rejected": gate_after.rejected.saturating_sub(gate_before.rejected),
    });
    let mut status = None;
    let mut error = None;
    let mut reason = None::<String>;
    let outcome = match response {
        Err(_) => "outer_timeout",
        Ok(Err(failure)) => {
            error = Some(failure.to_string());
            "transport_error"
        }
        Ok(Ok((code, body))) => {
            status = Some(code);
            if code == 200 {
                let validation = serde_json::from_slice::<Value>(&body)
                    .map_err(anyhow::Error::from)
                    .and_then(|value| match spec.route {
                        "state" => validate_children(
                            &value,
                            children,
                            &fixture.child_prefixes[spec.parent],
                        ),
                        _ => validate_snapshot(&value, fixture.parents),
                    });
                if let Err(failure) = validation {
                    error = Some(failure.to_string());
                    "contract_error"
                } else if shared_app.is_none()
                    && spec.route == "state"
                    && (gate_after.leaders != gate_before.leaders + 1
                        || gate_after.cache_hits != gate_before.cache_hits)
                {
                    error = Some("expected one uncached scoped projection leader".into());
                    "contract_error"
                } else {
                    black_box(body);
                    "ok"
                }
            } else {
                if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    reason = value["code"].as_str().map(str::to_owned);
                }
                error = Some(String::from_utf8_lossy(&body).into_owned());
                if code == 503 {
                    "http_503"
                } else {
                    "http_error"
                }
            }
        }
    };
    Ok(json!({
        "outcome": outcome,
        "http_status": status,
        "error": error,
        "reason": reason,
        "wall_time_ns": elapsed,
        "user_cpu_ns": super::duration_delta(resources_after.user_cpu, resources_before.user_cpu).as_nanos(),
        "system_cpu_ns": super::duration_delta(resources_after.system_cpu, resources_before.system_cpu).as_nanos(),
        "peak_rss_kb": resources_after.peak_rss_kb,
        "stages": stage_delta(&before, &after),
        "gate": gate_delta,
    }))
}

fn percentile(values: &mut [u64], percent: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let rank = (values.len() * percent).div_ceil(100);
    Some(values[rank.saturating_sub(1)])
}

struct SharedTargets {
    state_uris: Vec<String>,
    prefixes: Vec<String>,
    snapshot_uri: String,
    parents: usize,
    parents_setting: usize,
    route_mix: &'static str,
}

async fn send_shared(
    app: &Router,
    targets: &SharedTargets,
    children: usize,
    timeout: Duration,
    index: usize,
) -> Value {
    let spec = spec_from(
        &targets.state_uris,
        &targets.snapshot_uri,
        targets.parents_setting,
        targets.route_mix,
        index,
    );
    let started = Instant::now();
    let response = tokio::time::timeout(timeout, async {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&spec.uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("axum router is infallible");
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await?
            .to_vec();
        Ok::<_, anyhow::Error>((status, body))
    })
    .await;
    let wall_time_ns = started.elapsed().as_nanos() as u64;
    let mut status = None;
    let mut error = None;
    let mut reason = None::<String>;
    let outcome = match response {
        Err(_) => "outer_timeout",
        Ok(Err(failure)) => {
            error = Some(failure.to_string());
            "transport_error"
        }
        Ok(Ok((code, body))) => {
            status = Some(code);
            if code == 200 {
                let validation = serde_json::from_slice::<Value>(&body)
                    .map_err(anyhow::Error::from)
                    .and_then(|value| match spec.route {
                        "state" => {
                            validate_children(&value, children, &targets.prefixes[spec.parent])
                        }
                        _ => validate_snapshot(&value, targets.parents),
                    });
                match validation {
                    Err(failure) => {
                        error = Some(failure.to_string());
                        "contract_error"
                    }
                    Ok(()) => {
                        black_box(body);
                        "ok"
                    }
                }
            } else {
                if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    reason = value["code"].as_str().map(str::to_owned);
                }
                error = Some(String::from_utf8_lossy(&body).into_owned());
                if code == 503 {
                    "http_503"
                } else {
                    "http_error"
                }
            }
        }
    };
    json!({
        "outcome": outcome,
        "http_status": status,
        "error": error,
        "reason": reason,
        "wall_time_ns": wall_time_ns,
        "stages": [],
    })
}

struct CellRecord {
    measurements: Vec<Value>,
    outcomes: BTreeMap<String, usize>,
    latency: Vec<u64>,
    failure_latency: Vec<u64>,
    reasons: BTreeMap<String, usize>,
    successes_per_batch: Vec<usize>,
    stopped: bool,
    gate: Value,
    stages: Value,
    resources: Value,
}

fn record_shared_measurement(
    sink: &std::sync::Mutex<Vec<Value>>,
    requests: usize,
    index: usize,
    sample: Value,
) {
    sink.lock().unwrap().push(json!({
        "batch": index / requests + 1,
        "request": index % requests + 1,
        "measurement": sample,
    }));
}

fn run_shared_cell(
    runtime: &tokio::runtime::Runtime,
    fixture: &Fixture,
    children: usize,
    samples: usize,
    settings: &Settings,
    deadline: Instant,
    warmup_ok: bool,
) -> Result<CellRecord> {
    let planned = samples * settings.requests;
    let requests = settings.requests;
    let timeout = settings.timeout;
    let targets = Arc::new(SharedTargets {
        state_uris: fixture.state_uris.clone(),
        prefixes: fixture.child_prefixes.clone(),
        snapshot_uri: fixture.snapshot_uri.to_owned(),
        parents: fixture.parents,
        parents_setting: settings.parents,
        route_mix: settings.route_mix,
    });
    runtime.block_on(async move {
        let app = router(AppState::for_unix(fixture.host.clone()));
        let stages_before = stages();
        let gate_before = performance_snapshot().projection_gate;
        let resources_before = super::resource_usage();
        let next_index = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(!warmup_ok));
        let sink = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let mut set = tokio::task::JoinSet::new();
        let keep_going = |outcome: &str, saturation: bool| match outcome {
            "ok" => true,
            // A classified queue-full 503 is expected only in saturation cells.
            "http_503" | "not_sent_inflight_bound" => saturation,
            _ => false,
        };
        if settings.arrival_per_sec == 0 {
            for _ in 0..settings.concurrency {
                let app = app.clone();
                let targets = Arc::clone(&targets);
                let next_index = Arc::clone(&next_index);
                let stopped = Arc::clone(&stopped);
                let sink = Arc::clone(&sink);
                set.spawn(async move {
                    loop {
                        let index = next_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if index >= planned
                            || stopped.load(std::sync::atomic::Ordering::SeqCst)
                            || Instant::now() >= deadline
                        {
                            break;
                        }
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        let sample =
                            send_shared(&app, &targets, children, timeout.min(remaining), index)
                                .await;
                        let outcome = sample["outcome"].as_str().unwrap_or("http_error");
                        if !keep_going(outcome, false) {
                            stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        record_shared_measurement(&sink, requests, index, sample);
                    }
                });
            }
        } else {
            let inflight = Arc::new(tokio::sync::Semaphore::new(settings.max_inflight));
            let interval =
                Duration::from_nanos(1_000_000_000u64 / settings.arrival_per_sec.max(1) as u64);
            let mut tick_at = tokio::time::Instant::now() + interval;
            while !stopped.load(std::sync::atomic::Ordering::SeqCst)
                && Instant::now() < deadline
                && next_index.load(std::sync::atomic::Ordering::SeqCst) < planned
            {
                tokio::time::sleep_until(tick_at).await;
                tick_at += interval;
                let index = next_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if index >= planned {
                    break;
                }
                let permit = match inflight.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        // Offered but not accepted by the in-flight bound.
                        let sample = json!({"outcome": "not_sent_inflight_bound"});
                        record_shared_measurement(&sink, requests, index, sample);
                        continue;
                    }
                };
                let app = app.clone();
                let targets = Arc::clone(&targets);
                let stopped = Arc::clone(&stopped);
                let sink = Arc::clone(&sink);
                set.spawn(async move {
                    let _permit = permit;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let sample =
                        send_shared(&app, &targets, children, timeout.min(remaining), index).await;
                    let outcome = sample["outcome"].as_str().unwrap_or("http_error");
                    if !keep_going(outcome, true) {
                        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    record_shared_measurement(&sink, requests, index, sample);
                });
            }
            // Wait until every accepted request has finished.
            let _ = inflight.acquire_many(settings.max_inflight as u32).await;
        }
        while set.join_next().await.is_some() {}
        let measurements: Vec<Value> = std::mem::take(&mut *sink.lock().unwrap());
        let mut outcomes = BTreeMap::<String, usize>::new();
        let mut latency = Vec::new();
        let mut failure_latency = Vec::new();
        let mut reasons = BTreeMap::<String, usize>::new();
        let mut successes_per_batch = vec![0usize; samples];
        for entry in &measurements {
            let sample = &entry["measurement"];
            let outcome = sample["outcome"]
                .as_str()
                .context("missing request outcome")?;
            *outcomes.entry(outcome.to_owned()).or_default() += 1;
            if let Some(reason) = sample["reason"].as_str() {
                *reasons.entry(format!("{outcome}::{reason}")).or_default() += 1;
            }
            let batch = entry["batch"].as_u64().context("missing batch")? as usize;
            if outcome == "ok" {
                successes_per_batch[batch - 1] += 1;
                latency.push(sample["wall_time_ns"].as_u64().context("invalid latency")?);
            } else if let Some(latency) = sample["wall_time_ns"].as_u64() {
                // Offered-but-not-accepted markers carry no latency.
                failure_latency.push(latency);
            }
        }
        let stages_after = stages();
        let gate_after = performance_snapshot().projection_gate;
        let resources_after = super::resource_usage();
        let gate = json!({
            "leaders": gate_after.leaders.saturating_sub(gate_before.leaders),
            "joined_waiters": gate_after
                .joined_waiters
                .saturating_sub(gate_before.joined_waiters),
            "cache_hits": gate_after.cache_hits.saturating_sub(gate_before.cache_hits),
            "rejected": gate_after.rejected.saturating_sub(gate_before.rejected),
            "active_permits": gate_after.active_permits,
            "max_active_permits": gate_after.max_active_permits,
        });
        Ok(CellRecord {
            stopped: stopped.load(std::sync::atomic::Ordering::SeqCst),
            measurements,
            outcomes,
            latency,
            failure_latency,
            reasons,
            successes_per_batch,
            gate,
            stages: json!(stage_delta(&stages_before, &stages_after)),
            resources: json!({
                "user_cpu_ns": super::duration_delta(
                    resources_after.user_cpu,
                    resources_before.user_cpu,
                )
                .as_nanos(),
                "system_cpu_ns": super::duration_delta(
                    resources_after.system_cpu,
                    resources_before.system_cpu,
                )
                .as_nanos(),
                "peak_rss_kb": resources_after.peak_rss_kb,
            }),
        })
    })
}

fn measure(
    runtime: &tokio::runtime::Runtime,
    fixture: &Fixture,
    children: usize,
    samples: usize,
    settings: &Settings,
) -> Result<Value> {
    let deadline = Instant::now() + settings.budget;
    let shared_mode = settings.concurrency > 1 || settings.arrival_per_sec > 0;
    let shared_app = shared_mode.then(|| router(AppState::for_unix(fixture.host.clone())));
    // Warmup is the cold phase: serial mode warms its single target with a
    // fresh AppState; shared mode warms every distinct target once through the
    // same shared gate the measured phase will use.
    let mut warmup = Vec::new();
    if let Some(app) = shared_app.as_ref() {
        let mut cold_specs = Vec::new();
        if settings.route_mix != "snapshot" {
            cold_specs.extend((0..settings.parents).map(|parent| RequestSpec {
                uri: fixture.state_uris[parent].clone(),
                route: "state",
                parent,
            }));
        }
        if settings.route_mix != "state" {
            cold_specs.push(RequestSpec {
                uri: fixture.snapshot_uri.to_owned(),
                route: "snapshot",
                parent: 0,
            });
        }
        for spec in &cold_specs {
            warmup.push(request(
                runtime,
                fixture,
                children,
                settings.timeout.min(settings.budget),
                spec,
                Some(app),
            )?);
        }
    } else {
        let spec = spec_from(
            &fixture.state_uris,
            fixture.snapshot_uri,
            settings.parents,
            settings.route_mix,
            0,
        );
        warmup.push(request(
            runtime,
            fixture,
            children,
            settings.timeout.min(settings.budget),
            &spec,
            None,
        )?);
    }
    let warmup_ok = warmup.iter().all(|sample| sample["outcome"] == "ok");
    let mut measurements = Vec::new();
    let mut outcomes = BTreeMap::<String, usize>::new();
    let mut latency = Vec::new();
    let mut failure_latency = Vec::new();
    let mut reasons = BTreeMap::<String, usize>::new();
    let mut child_latency = Vec::new();
    let mut successes_per_batch = vec![0usize; samples];
    let mut cell_gate = Value::Null;
    let mut cell_stages = Value::Null;
    let mut cell_resources = Value::Null;
    // Stop after warmup failure or a timeout: an expired outer future can leave
    // a blocking projection draining. Never pile more work onto that scope.
    let mut stopped = !warmup_ok;
    if shared_mode {
        let cell = run_shared_cell(
            runtime, fixture, children, samples, settings, deadline, warmup_ok,
        )?;
        measurements = cell.measurements;
        outcomes = cell.outcomes;
        latency = cell.latency;
        failure_latency = cell.failure_latency;
        reasons = cell.reasons;
        successes_per_batch = cell.successes_per_batch;
        stopped = cell.stopped;
        cell_gate = cell.gate;
        cell_stages = cell.stages;
        cell_resources = cell.resources;
    } else {
        for batch in 1..=samples {
            for index in 1..=settings.requests {
                if stopped || Instant::now() >= deadline {
                    stopped = true;
                    break;
                }
                let spec = spec_from(
                    &fixture.state_uris,
                    fixture.snapshot_uri,
                    settings.parents,
                    settings.route_mix,
                    index - 1,
                );
                let sample = request(
                    runtime,
                    fixture,
                    children,
                    settings
                        .timeout
                        .min(deadline.saturating_duration_since(Instant::now())),
                    &spec,
                    None,
                )?;
                let outcome = sample["outcome"]
                    .as_str()
                    .context("missing request outcome")?;
                *outcomes.entry(outcome.to_owned()).or_default() += 1;
                if let Some(reason) = sample["reason"].as_str() {
                    *reasons.entry(format!("{outcome}::{reason}")).or_default() += 1;
                }
                if outcome == "ok" {
                    successes_per_batch[batch - 1] += 1;
                    latency.push(sample["wall_time_ns"].as_u64().context("invalid latency")?);
                    for stage in sample["stages"].as_array().context("missing stages")? {
                        if stage["name"] == "projection.state.children" {
                            if let Some(ns) = stage["inclusive_total_ns"].as_u64() {
                                child_latency.push(ns);
                            }
                        }
                    }
                } else {
                    failure_latency
                        .push(sample["wall_time_ns"].as_u64().context("invalid latency")?);
                }
                stopped = outcome != "ok";
                measurements.push(json!({
                    "batch": batch,
                    "request": index,
                    "measurement": sample,
                }));
            }
            if stopped {
                break;
            }
        }
    }
    let planned = samples * settings.requests;
    let success = latency.len();
    let attempted = measurements.len();
    let p99_eligible = samples >= 3 && successes_per_batch.iter().all(|count| *count >= 1000);
    // Probe after HTTP measurement, separately and under its own timeout. Its
    // counters are never attributed to a parent HTTP request.
    let pending_timeout = warmup
        .iter()
        .any(|sample| sample["outcome"] == "outer_timeout")
        || measurements
            .last()
            .is_some_and(|sample| sample["measurement"]["outcome"] == "outer_timeout");
    let sql = if pending_timeout {
        json!({"outcome": "skipped_after_outer_timeout"})
    } else {
        sql_diagnostics(fixture, settings.timeout)
            .unwrap_or_else(|error| json!({"outcome": "error", "error": error.to_string()}))
    };
    Ok(json!({
        "benchmark_id": format!("http.child_state.{}.{}", fixture.load_mode, children),
        "fixture_version": "child-state.v3",
        "fixture": {
            "loaded_children": children,
            "load_mode": fixture.load_mode,
            "parents": fixture.parents,
            "retained_loaded_handles_including_parent": fixture._loaded.len(),
            "active_per_child": ACTIVE_PER_CHILD,
            "active_statuses": ["queued", "running", "cancelling"],
            "unrelated_terminal_history": settings.history,
            "timestamp": "2026-01-01T00:00:00Z",
            "provider": "StubProvider",
            "setup_timed": false,
            "history_payload": "TaskRecord.command_task.synthetic",
        },
        "comparison": {
            "sequence": settings.comparison,
            "position": settings.position,
            "variant_label": &settings.comparison[settings.position - 1..settings.position],
            "automatic_implementation_switch": false,
        },
        "load": {
            "concurrency": settings.concurrency,
            "rate": if settings.arrival_per_sec == 0 {
                json!("closed_loop")
            } else {
                json!({
                    "arrival_per_sec": settings.arrival_per_sec,
                    "max_inflight": settings.max_inflight,
                })
            },
            "route_mix": settings.route_mix,
            "key_mix": if settings.parents > 1 { "distinct_parents" } else { "same_parent" },
            "gate_sharing": if shared_mode {
                "shared AppState and gate"
            } else {
                "fresh AppState per request"
            },
            "saturation_expected_503": settings.arrival_per_sec > 0,
            "projection_gate": "production gate budgets unchanged",
            "batches": samples,
            "requests_per_batch": settings.requests,
            "planned_requests": planned,
            "attempted_requests": attempted,
            "not_attempted_requests": planned - attempted,
            "budget_ms": settings.budget.as_millis(),
            "budget_scope": "HTTP warmup and measured requests; setup and isolated SQL probes excluded",
            "outer_timeout_ms": settings.timeout.as_millis(),
            "aborted": stopped,
        },
        "warmup": warmup,
        "warmup_failed_requests": usize::from(!warmup_ok),
        "outcomes": outcomes,
        "failure_reasons": reasons,
        "successful_requests": success,
        "successful_requests_per_batch": successes_per_batch,
        "failed_requests": attempted - success,
        "summary": {
            "percentile_method": "nearest_rank_successful_requests_only",
            "http_p50_ns": percentile(&mut latency, 50),
            "http_p95_ns": percentile(&mut latency, 95),
            "p99_eligible": p99_eligible,
            "http_p99_ns": if p99_eligible { percentile(&mut latency, 99) } else { None },
            "failure_p50_ns": percentile(&mut failure_latency.clone(), 50),
            "failure_p95_ns": percentile(&mut failure_latency.clone(), 95),
            "child_summary_samples": child_latency.len(),
            "child_summary_p50_ns": percentile(&mut child_latency, 50),
            "child_summary_p95_ns": percentile(&mut child_latency, 95),
            "child_summary_p99_ns": if p99_eligible && child_latency.len() == success { percentile(&mut child_latency, 99) } else { None },
        },
        "cell_gate_delta": cell_gate,
        "cell_stage_delta": cell_stages,
        "cell_resources": cell_resources,
        "measurements": measurements,
        "sql_diagnostics": sql,
        "limitations": [
            "阶段计数为本进程请求窗口增量；嵌套 span 含重叠，不能相加当作独占耗时。并发 cell 只记录 cell 前后总体增量，不做逐请求 stage 相加。",
            "db.reader.operation 不是单条 SQL 独占耗时，也不是 SQL 解码行数。",
            "projection.child.* 保留零计数；task_sort.count=0 可表示该请求窗口未执行排序，不能单独证明性能收益。",
            "projection.child.task_read 含 reader 等待及读取/解码，不能当作 SQL 独占耗时。",
            "SQL probe 为同一合成 DB 上独立 SELECT+JSON 解码，不包含 reader 获取或后续 Rust 筛选/排序。",
            "本 workload 为非生产合成基座；/snapshot 与并发 cell 仅覆盖显式选择的参数组合，不构成生产延迟或 503 改善结论。",
            "AA/ABBA 仅记录外部运行位置；必须由同 fixture 的对应旧/新 executable 分别执行，不自动证明收益。",
            "p99 仅在至少 3 批、每批至少 1000 次成功请求时输出，否则为 null。",
            "RSS 为进程生命周期高水位，不是该请求独占或增量 RSS。"
        ],
    }))
}
fn sql_diagnostics(fixture: &Fixture, timeout: Duration) -> Result<Value> {
    let connection = Connection::open_with_flags(
        fixture.host.runtime_db().path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.busy_timeout(timeout)?;
    let version: String = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let options = connection
        .prepare("PRAGMA compile_options")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let probes = [
        ("tasks.latest_all.reference_select", ALL_TASKS_SQL, false),
        (
            "tasks.active_for_agent.reference_select",
            ACTIVE_TASKS_SQL,
            true,
        ),
    ]
    .into_iter()
    .map(|(name, sql, owner_filtered)| {
        let deadline = Instant::now() + timeout;
        connection.progress_handler(1000, Some(move || Instant::now() >= deadline));
        let parameters: Vec<&dyn rusqlite::ToSql> = if owner_filtered {
            vec![&"child-state-000"]
        } else {
            Vec::new()
        };
        let mut plan = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let plan = plan
            .query_map(parameters.as_slice(), |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut statement = connection.prepare(sql)?;
        let mut returned = 0usize;
        let mut decoded = 0usize;
        let started = Instant::now();
        let result = (|| -> Result<()> {
            let mut rows = statement.query(parameters.as_slice())?;
            while let Some(row) = rows.next()? {
                returned += 1;
                let payload: String = row.get(0)?;
                let task: TaskRecord = serde_json::from_str(&payload)?;
                decoded += 1;
                black_box(task);
            }
            Ok(())
        })();
        let elapsed = started.elapsed().as_nanos();
        Ok(json!({
            "name": name,
            "sql": sql,
            "owner": if owner_filtered { Some("child-state-000") } else { None },
            "query_plan": plan,
            "returned_rows": returned,
            "decoded_rows": decoded,
            "fullscan_steps": statement.get_status(StatementStatus::FullscanStep),
            "vm_steps": statement.get_status(StatementStatus::VmStep),
            "sort_operations": statement.get_status(StatementStatus::Sort),
            "select_and_decode_ns": elapsed,
            "timeout_ms": timeout.as_millis(),
            "outcome": if result.is_ok() { "ok" } else { "error" },
            "error": result.err().map(|error| error.to_string()),
        }))
    })
    .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "connection": "independent_read_only_bundled_rusqlite",
        "sqlite_version": version,
        "compile_options": options,
        "probes": probes,
    }))
}
