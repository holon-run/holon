//! Opt-in, synthetic, uncached parent /state requests through the real scoped
//! projection. No daemon, network provider, or production database is used.
//!
//! HOLON_BENCH_FILTER=http.child_state.loaded.0,http.child_state.loaded.1,http.child_state.loaded.50
//! HOLON_BENCH_CHILD_HISTORY=0 (0..=100000; large history is never the default)
//! HOLON_BENCH_CHILD_REQUESTS=1 (requests per measured batch)
//! HOLON_BENCH_CHILD_BUDGET_MS=60000 (per cell, including warmup, excluding setup)
//! HOLON_BENCH_CHILD_TIMEOUT_MS=15000 (outer timeout, not a gate budget override)
//! HOLON_BENCH_COMPARISON=AA|ABBA HOLON_BENCH_POSITION=1..sequence length
//! Run each position separately with the corresponding old/new executable;
//! labels alone neither switch implementations nor establish an improvement.

use std::{collections::BTreeMap, hint::black_box, sync::Arc, time::Duration, time::Instant};

use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    http::Request,
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
}

impl Settings {
    fn load() -> Result<Self> {
        let comparison = std::env::var("HOLON_BENCH_COMPARISON").unwrap_or_else(|_| "AA".into());
        anyhow::ensure!(
            matches!(comparison.as_str(), "AA" | "ABBA"),
            "HOLON_BENCH_COMPARISON must be AA or ABBA"
        );
        let position = setting("HOLON_BENCH_POSITION", 1, 1, comparison.len())?;
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
    let selected = [0, 1, 50]
        .into_iter()
        .filter(|children| {
            let id = format!("http.child_state.loaded.{children}");
            filter.iter().any(|selected| selected == &id)
        })
        .collect::<Vec<_>>();
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
        let fixture = runtime.block_on(Fixture::build(children, settings.history))?;
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
    uri: String,
}

impl Fixture {
    async fn build(children: usize, history: usize) -> Result<Self> {
        let home = tempfile::tempdir().context("creating child-state benchmark home")?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        let bootstrap = async {
            host.create_public_named_agent("child-state-parent", None, None, None)
                .await?;
            let parent_id = host
                .configured_default_agent_id()?
                .context("child-state fixture has no default agent")?;
            let mut child_ids = Vec::new();
            for index in 0..children {
                let id = format!("child-state-{index:03}");
                host.runtime_db()
                    .agent_identities()
                    .upsert(&AgentIdentityRecord::new(
                        &id,
                        AgentKind::Child,
                        AgentVisibility::Private,
                        AgentOwnership::ParentSupervised,
                        AgentProfilePreset::PrivateChild,
                        Some(parent_id.clone()),
                        None,
                    ))?;
                child_ids.push(id);
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
            for id in &child_ids {
                loaded.push(host.get_or_create_agent(id).await?);
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
            uri: format!("/api/agents/{parent_id}/state"),
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

fn validate_children(value: &Value, children: usize) -> Result<()> {
    let snapshot: AgentStateSnapshotDto =
        serde_json::from_value(value.clone()).context("invalid parent /state DTO")?;
    let actual = snapshot.agent.active_children;
    anyhow::ensure!(actual.len() == children, "unexpected child count");
    for (index, child) in actual.iter().enumerate() {
        anyhow::ensure!(
            child.identity.agent_id == format!("child-state-{index:03}")
                && child.active_task_count == ACTIVE_PER_CHILD,
            "unexpected child identity, active task count, or observability"
        );
    }
    Ok(())
}

fn request(
    runtime: &tokio::runtime::Runtime,
    fixture: &Fixture,
    children: usize,
    timeout: Duration,
) -> Result<Value> {
    // A fresh AppState resets only the byte/projection gate cache. The same
    // loaded host, database, and OS/SQLite caches are reused across requests.
    let app = router(AppState::for_unix(fixture.host.clone()));
    let before = stages();
    let gate_before = performance_snapshot().projection_gate;
    let resources_before = super::resource_usage();
    let started = Instant::now();
    let response = runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            let response = app
                .oneshot(Request::builder().uri(&fixture.uri).body(Body::empty())?)
                .await
                .expect("axum router is infallible");
            let status = response.status().as_u16();
            let body = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
            Ok::<_, anyhow::Error>((status, body))
        })
        .await
    });
    let elapsed = started.elapsed().as_nanos();
    let resources_after = super::resource_usage();
    let after = stages();
    let gate_after = performance_snapshot().projection_gate;
    let mut status = None;
    let mut error = None;
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
                    .and_then(|value| validate_children(&value, children));
                if let Err(failure) = validation {
                    error = Some(failure.to_string());
                    "contract_error"
                } else if gate_after.leaders != gate_before.leaders + 1
                    || gate_after.cache_hits != gate_before.cache_hits
                {
                    error = Some("expected one uncached scoped projection leader".into());
                    "contract_error"
                } else {
                    black_box(body);
                    "ok"
                }
            } else {
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
        "wall_time_ns": elapsed,
        "user_cpu_ns": super::duration_delta(resources_after.user_cpu, resources_before.user_cpu).as_nanos(),
        "system_cpu_ns": super::duration_delta(resources_after.system_cpu, resources_before.system_cpu).as_nanos(),
        "peak_rss_kb": resources_after.peak_rss_kb,
        "stages": stage_delta(&before, &after),
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

fn measure(
    runtime: &tokio::runtime::Runtime,
    fixture: &Fixture,
    children: usize,
    samples: usize,
    settings: &Settings,
) -> Result<Value> {
    let deadline = Instant::now() + settings.budget;
    let warmup = request(
        runtime,
        fixture,
        children,
        settings.timeout.min(settings.budget),
    )?;
    let warmup_ok = warmup["outcome"] == "ok";
    let mut measurements = Vec::new();
    let mut outcomes = BTreeMap::<String, usize>::new();
    let mut latency = Vec::new();
    let mut child_latency = Vec::new();
    let mut successes_per_batch = vec![0usize; samples];
    // Stop after warmup failure or a timeout: an expired outer future can leave
    // a blocking projection draining. Never pile more work onto that scope.
    let mut stopped = !warmup_ok;
    for batch in 1..=samples {
        for index in 1..=settings.requests {
            if stopped || Instant::now() >= deadline {
                stopped = true;
                break;
            }
            let sample = request(
                runtime,
                fixture,
                children,
                settings
                    .timeout
                    .min(deadline.saturating_duration_since(Instant::now())),
            )?;
            let outcome = sample["outcome"]
                .as_str()
                .context("missing request outcome")?;
            *outcomes.entry(outcome.to_owned()).or_default() += 1;
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
            }
            stopped = outcome != "ok";
            measurements.push(json!({"batch": batch, "request": index, "measurement": sample}));
        }
        if stopped {
            break;
        }
    }
    let planned = samples * settings.requests;
    let success = latency.len();
    let attempted = measurements.len();
    let p99_eligible = samples >= 3 && successes_per_batch.iter().all(|count| *count >= 1000);
    // Probe after HTTP measurement, separately and under its own timeout. Its
    // counters are never attributed to a parent HTTP request.
    let pending_timeout = warmup["outcome"] == "outer_timeout"
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
        "benchmark_id": format!("http.child_state.loaded.{children}"),
        "fixture_version": "child-state.v2",
        "fixture": {
            "loaded_children": children,
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
            "concurrency": 1,
            "rate": "closed_loop",
            "projection_gate": "fresh AppState per request; production gate budgets unchanged",
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
        "successful_requests": success,
        "successful_requests_per_batch": successes_per_batch,
        "failed_requests": attempted - success,
        "summary": {
            "percentile_method": "nearest_rank_successful_requests_only",
            "http_p50_ns": percentile(&mut latency, 50),
            "http_p95_ns": percentile(&mut latency, 95),
            "p99_eligible": p99_eligible,
            "http_p99_ns": if p99_eligible { percentile(&mut latency, 99) } else { None },
            "child_summary_samples": child_latency.len(),
            "child_summary_p50_ns": percentile(&mut child_latency, 50),
            "child_summary_p95_ns": percentile(&mut child_latency, 95),
            "child_summary_p99_ns": if p99_eligible && child_latency.len() == success { percentile(&mut child_latency, 99) } else { None },
        },
        "measurements": measurements,
        "sql_diagnostics": sql,
        "limitations": [
            "阶段计数为本进程请求窗口增量；嵌套 span 含重叠，不能相加当作独占耗时。",
            "db.reader.operation 不是单条 SQL 独占耗时，也不是 SQL 解码行数。",
            "projection.child.* 保留零计数；task_sort.count=0 可表示该请求窗口未执行排序，不能单独证明性能收益。",
            "projection.child.task_read 含 reader 等待及读取/解码，不能当作 SQL 独占耗时。",
            "SQL probe 为同一合成 DB 上独立 SELECT+JSON 解码，不包含 reader 获取或后续 Rust 筛选/排序。",
            "仅串行 /state gate miss；没有 /snapshot、并发容量曲线或生产 503 改善结论。",
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
