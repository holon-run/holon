//! Offline historical Task-result evidence and reverse-source business reads.
//! Runtime TaskOutput coverage gaps are explicit in the emitted artifact.

use std::{hint::black_box, io::Write, process::Command, time::Instant};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use holon::{
    diagnostics::attribution::{self, StageSnapshot},
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, TaskKind, TaskRecord, TaskStatus,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::TempDir;

const AGENT: &str = "default";
const HISTORICAL_TASK: &str = "task_scale_historical";
const HISTORICAL_MESSAGE: &str = "message_scale_historical_result";
const NOISE_TASK: &str = "task_scale_noise";
const INVOCATION_TASK: &str = "task_scale_parent_invocation";
const INVOCATION_MESSAGE: &str = "message_scale_invocation";
const HISTORICAL_OUTPUT: &str = "deterministic historical Task result\n";
const DEFAULT_SAMPLES: usize = 3;
const DEFAULT_HISTORY_ROWS: usize = 2_000;
const MAX_HISTORY_ROWS: usize = 100_000;
const TASK_ROWS: usize = 4;
const FIXED_MESSAGE_ROWS: usize = 2;
const BATCH_ROWS: usize = 256;

struct Fixture {
    db: RuntimeDb,
    storage: AppStorage,
    // The directory outlives both database handles.
    _root: TempDir,
}

impl Fixture {
    fn new(history_rows: usize) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            root.path().join(".holon/state/runtime.sqlite"),
            root.path().join(".holon/state/runtime.lock"),
        )?;
        let storage = AppStorage::new_for_agent(root.path(), AGENT, db.clone())?;
        let at: DateTime<Utc> = "2024-01-02T03:04:05Z".parse()?;
        for (id, status) in [
            (HISTORICAL_TASK, TaskStatus::Completed),
            (NOISE_TASK, TaskStatus::Completed),
            ("task_scale_active_0", TaskStatus::Queued),
            ("task_scale_active_1", TaskStatus::Queued),
        ] {
            storage.append_task(&TaskRecord {
                id: id.into(),
                agent_id: AGENT.into(),
                kind: TaskKind::CommandTask,
                status,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: Some(format!("deterministic fixture {id}")),
                detail: None,
                recovery: None,
            })?;
        }
        // Same minimal source shape as the supervised-invocation repository
        // tests. This tests the repository lookup, not a child-task lifecycle.
        let mut invocation = message(
            INVOCATION_MESSAGE,
            MessageKind::InternalFollowup,
            INVOCATION_TASK,
            "supervised invocation",
            at,
        );
        invocation.metadata = Some(json!({
            "parent_agent_id": "scale-parent",
            "child_agent_id": AGENT,
            "delegated_task_id": INVOCATION_TASK,
            "supervision_task_id": INVOCATION_TASK,
            "parent_supervised": true,
        }));
        invocation.normalize_admission_fields();
        storage.append_message(&invocation)?;
        storage.append_message(&message(
            HISTORICAL_MESSAGE,
            MessageKind::TaskResult,
            HISTORICAL_TASK,
            HISTORICAL_OUTPUT,
            at,
        ))?;
        let noise = "unrelated historical Task result ".repeat(8);
        for start in (0..history_rows).step_by(BATCH_ROWS) {
            let batch: Vec<_> = (start..(start + BATCH_ROWS).min(history_rows))
                .map(|index| {
                    message(
                        &format!("message_scale_history_{index:06}"),
                        MessageKind::TaskResult,
                        NOISE_TASK,
                        &noise,
                        at,
                    )
                })
                .collect();
            db.messages().upsert_many_committed(&batch)?;
        }
        ensure!(
            storage.count_messages()? == history_rows + FIXED_MESSAGE_ROWS,
            "fixture Message cardinality changed"
        );
        ensure!(
            storage.latest_task_records()?.len() == TASK_ROWS,
            "fixture Task cardinality changed"
        );
        ensure!(
            storage
                .latest_active_task_records_for_agent(AGENT, 3)?
                .len()
                == 2,
            "fixture active Task cardinality changed"
        );
        Ok(Self {
            db,
            storage,
            _root: root,
        })
    }
}

fn message(
    id: &str,
    kind: MessageKind,
    task_id: &str,
    text: &str,
    at: DateTime<Utc>,
) -> MessageEnvelope {
    let mut record = MessageEnvelope::new(
        AGENT,
        kind,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    record.id = id.into();
    record.created_at = at;
    record.task_id = Some(task_id.into());
    record.metadata = Some(json!({"task_id": task_id, "task_status": "completed"}));
    record
}

#[derive(Serialize)]
struct StageDelta {
    name: String,
    calls: u64,
    rows: u64,
    total_ns: u64,
}

#[derive(Serialize)]
struct Sample {
    iteration: usize,
    wall_time_ns: u128,
    stages: Vec<StageDelta>,
    exit_status: &'static str,
}

#[derive(Serialize)]
struct Summary {
    median_wall_time_ns: u128,
    min_wall_time_ns: u128,
    max_wall_time_ns: u128,
    median_absolute_deviation_ns: u128,
}

#[derive(Serialize)]
struct BenchmarkResult {
    id: &'static str,
    operations_per_sample: usize,
    samples: Vec<Sample>,
    summary: Summary,
}

fn measure<T>(
    repetitions: usize,
    benchmark_id: &'static str,
    mut operation: impl FnMut() -> Result<T>,
    validate: impl Fn(&T) -> Result<()>,
) -> Result<BenchmarkResult> {
    validate(&operation()?)?;
    let mut samples = Vec::with_capacity(repetitions);
    for iteration in 0..repetitions {
        let before = attribution::snapshot();
        let started = Instant::now();
        let output = black_box(operation()?);
        let wall_time_ns = started.elapsed().as_nanos();
        let stages = attribution::snapshot()
            .into_iter()
            .filter_map(|after| {
                let previous: &StageSnapshot = before.iter().find(|s| s.name == after.name)?;
                let calls = after.count.saturating_sub(previous.count);
                (calls > 0).then(|| StageDelta {
                    name: after.name,
                    calls,
                    rows: after.rows.saturating_sub(previous.rows),
                    total_ns: after.total_ns.saturating_sub(previous.total_ns),
                })
            })
            .collect();
        validate(&output)?;
        samples.push(Sample {
            iteration,
            wall_time_ns,
            stages,
            exit_status: "success",
        });
    }
    let mut times: Vec<_> = samples.iter().map(|s| s.wall_time_ns).collect();
    times.sort_unstable();
    let median = times[times.len() / 2];
    let mut deviations: Vec<_> = times.iter().map(|time| time.abs_diff(median)).collect();
    deviations.sort_unstable();
    Ok(BenchmarkResult {
        id: benchmark_id,
        operations_per_sample: 1,
        summary: Summary {
            median_wall_time_ns: median,
            min_wall_time_ns: times[0],
            max_wall_time_ns: times[times.len() - 1],
            median_absolute_deviation_ns: deviations[deviations.len() / 2],
        },
        samples,
    })
}

fn setting(name: &str, default: usize) -> Result<usize> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be an unsigned integer")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("reading {name}")),
    }
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn coverage_gaps() -> Value {
    json!([
        {
            "benchmark_id": "task.output.head",
            "reason": "Output-head reads and native Task/Input ACK fixture APIs are crate-private; raw canonical writes do not qualify a Fresh runtime namespace.",
        },
        {
            "benchmark_id": "task.output.selected",
            "reason": "Task-result evidence lookup below is not RuntimeHandle::task_output and does not establish coverage of #4105's historical output scan.",
        },
    ])
}

fn main() -> Result<()> {
    let repetitions = setting("HOLON_BENCH_SAMPLES", DEFAULT_SAMPLES)?;
    let history_rows = setting("HOLON_BENCH_HISTORY_ROWS", DEFAULT_HISTORY_ROWS)?;
    ensure!(repetitions > 0, "HOLON_BENCH_SAMPLES must be positive");
    ensure!(
        history_rows <= MAX_HISTORY_ROWS,
        "HOLON_BENCH_HISTORY_ROWS must not exceed {MAX_HISTORY_ROWS}"
    );
    let started_at = Utc::now().to_rfc3339();
    let setup = Instant::now();
    let fixture = Fixture::new(history_rows)?;
    let fixture_setup_wall_time_ns = setup.elapsed().as_nanos();
    let results = vec![
        measure(
            repetitions,
            "task.history.selected_result_by_id",
            || fixture.storage.read_message_by_id(HISTORICAL_MESSAGE),
            |record| {
                let record = record
                    .as_ref()
                    .context("historical result Message missing")?;
                ensure!(
                    record.id == HISTORICAL_MESSAGE
                        && record.kind == MessageKind::TaskResult
                        && record.task_id.as_deref() == Some(HISTORICAL_TASK)
                        && matches!(&record.body, MessageBody::Text { text } if text == HISTORICAL_OUTPUT),
                    "historical result evidence mismatch"
                );
                Ok(())
            },
        )?,
        measure(
            repetitions,
            "task.history.reverse_source_lookup",
            || {
                fixture
                    .db
                    .messages()
                    .supervised_invocation_sources(AGENT, INVOCATION_TASK)
            },
            |sources| {
                ensure!(
                    sources.len() == 1
                        && sources[0].id == INVOCATION_MESSAGE
                        && matches!(&sources[0].body, MessageBody::Text { text } if text == "supervised invocation"),
                    "reverse lookup did not return the exact unique invocation source"
                );
                Ok(())
            },
        )?,
        measure(
            repetitions,
            "task.history.record_by_id",
            || fixture.storage.latest_task_record(HISTORICAL_TASK),
            |record| {
                ensure!(
                    record
                        .as_ref()
                        .is_some_and(|record| record.id == HISTORICAL_TASK
                            && record.status == TaskStatus::Completed),
                    "historical Task record mismatch"
                );
                Ok(())
            },
        )?,
    ];
    let artifact = json!({
        "schema": "holon.task-history-scale.v1",
        "benchmark_suite": "task_history_scale",
        "workload_version": "v1",
        "started_at": started_at,
        "coverage_status": "partial",
        "coverage_gaps": coverage_gaps(),
        "environment": {
            "git_revision": command_output("git", &["rev-parse", "HEAD"]),
            "git_dirty": command_output("git", &["status", "--porcelain"]).map(|s| !s.is_empty()),
            "build_source_sha256": option_env!("HOLON_BENCH_SOURCE_SHA256"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "logical_cpus": std::thread::available_parallelism().ok().map(usize::from),
            "rustc_version": command_output("rustc", &["--version"]),
            "debug_assertions": cfg!(debug_assertions),
            "build_profile": if cfg!(debug_assertions) { "debug" } else { "bench" },
            "network_mode": "not_used",
            "sqlite_version": rusqlite::version(),
        },
        "fixture": {
            "history_rows": history_rows,
            "fixed_message_rows": FIXED_MESSAGE_ROWS,
            "total_message_rows": history_rows + FIXED_MESSAGE_ROWS,
            "task_rows": TASK_ROWS,
            "active_tasks": 2,
            "selected_records_per_read": 1,
            "warmup_repetitions": 1,
            "measured_repetitions": repetitions,
            "fixture_setup_wall_time_ns": fixture_setup_wall_time_ns,
            "cache_mode": "warm_sqlite_and_os_cache",
        },
        "results": results,
    });
    // Never emit a partial-success artifact if any API or validation failed.
    let mut encoded = serde_json::to_vec(&artifact)?;
    encoded.push(b'\n');
    std::io::stdout().lock().write_all(&encoded)?;
    Ok(())
}
