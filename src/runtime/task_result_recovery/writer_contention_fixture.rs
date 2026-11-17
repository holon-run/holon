use super::{pending_command_result, *};
use crate::provider::StubProvider;
use crate::runtime::tests::support::{
    context_config, controlled_clock, isolate_test_user_home, LifecycleHarness,
};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::Instrument;
use tracing_subscriber::prelude::*;

#[derive(Default, serde::Serialize)]
struct WriterEvent {
    held_us: u64,
    queue_ms: u64,
    callback_us: Option<u64>,
    commit_us: Option<u64>,
    begin_retries: Option<u64>,
    operation: String,
    recovery: bool,
    committed: bool,
}
impl tracing::field::Visit for WriterEvent {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.committed = format!("{value:?}") == "runtime db write committed";
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.committed = value == "runtime db write committed",
            "operation" => value.clone_into(&mut self.operation),
            _ => {}
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "held_write_lock_us" => self.held_us = value,
            "queue_wait_ms" => self.queue_ms = value,
            "callback_us" => self.callback_us = Some(value),
            "commit_us" => self.commit_us = Some(value),
            "begin_retry_count" => self.begin_retries = Some(value),
            _ => {}
        }
    }
    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.record_u64(field, value.try_into().unwrap_or(u64::MAX));
    }
}

#[derive(Default)]
struct PreparationEvent {
    caller: String,
    outcome: String,
    miss: String,
    message: String,
}
impl tracing::field::Visit for PreparationEvent {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "caller" => value.clone_into(&mut self.caller),
            "outcome" => value.clone_into(&mut self.outcome),
            "miss" => value.clone_into(&mut self.miss),
            "message" => value.clone_into(&mut self.message),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct WriterTrace(
    Arc<Mutex<Vec<WriterEvent>>>,
    Arc<Mutex<std::collections::BTreeMap<String, u64>>>,
    Arc<Mutex<u64>>,
);
impl<S> tracing_subscriber::Layer<S> for WriterTrace
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == "holon::performance" {
            let mut fields = PreparationEvent::default();
            event.record(&mut fields);
            if fields.message.contains("Task preparation candidate stale") {
                *self.2.lock().unwrap() += 1;
            }
            if fields.caller.is_empty() || fields.outcome.is_empty() {
                return;
            }
            let scope = if context
                .event_scope(event)
                .is_some_and(|mut scope| scope.any(|span| span.name() == "fixture_recovery_scan"))
            {
                "recovery"
            } else {
                "foreground"
            };
            let key = format!(
                "{scope}/{}/{}/{}",
                fields.caller, fields.outcome, fields.miss
            );
            *self.1.lock().unwrap().entry(key).or_default() += 1;
            return;
        }
        if !matches!(
            event.metadata().target(),
            "holon::runtime_db::connection" | "holon_storage::connection"
        ) || event
            .metadata()
            .fields()
            .field("held_write_lock_us")
            .is_none()
        {
            return;
        }
        let mut fields = WriterEvent::default();
        event.record(&mut fields);
        if fields.committed {
            fields.recovery = context
                .event_scope(event)
                .is_some_and(|mut scope| scope.any(|span| span.name() == "fixture_recovery_scan"));
            self.0.lock().unwrap().push(fields);
        }
    }
}

const RESULTS: usize = 64;
const TRIALS: usize = 18;
const REPLY: &str = "task-result writer contention fixture reply";
const HISTORY_TRIALS: usize = 3;
const OUTPUT_TASKS: usize = 128;
const HISTORY_COMMAND_TASK: &str = "fixture-history-command";
const TASK_DETAIL_STAGES: [&str; 4] = [
    "db.task.candidate.read",
    "db.task.candidate.validate",
    "db.task.candidate.apply",
    "db.task.write.delta_finalize",
];
const TASK_COUNTER_NOTE: &str =
    "Candidate read includes selection/decode/qualification; its rows count records, not independent decode work. Candidate validate rows count qualified records; candidate apply rows count aliases; delta finalize rows count actual final keys and include write-set finalization. No complete selection/decode census or independent qualification timer is available. Missing task stages are absent instrumentation, not measured zero work. Writer callback_us/commit_us are individual write timings; nested stages overlap and must not be summed.";

fn missing_task_stages(
    stages: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Vec<&'static str> {
    TASK_DETAIL_STAGES
        .into_iter()
        .filter(|name| !stages.contains_key(*name))
        .collect()
}

#[test]
fn fixture_reports_missing_task_stages_without_fabricating_measurements() {
    let mut stages = std::collections::BTreeMap::new();
    assert_eq!(missing_task_stages(&stages), TASK_DETAIL_STAGES);
    stages.insert(
        TASK_DETAIL_STAGES[0].to_owned(),
        serde_json::json!({"count": 0, "rows": 0, "ns": 0}),
    );
    assert_eq!(missing_task_stages(&stages), TASK_DETAIL_STAGES[1..]);
    assert_eq!(stages.len(), 1);
}

#[test]
fn fixture_records_current_and_legacy_writer_timings() {
    let writer = WriterTrace::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(writer.clone()), || {
        tracing::trace!(
            target: "holon_storage::connection",
            held_write_lock_us = 23_u128,
            callback_us = 17_u128,
            commit_us = 5_u128,
            "runtime db write committed"
        );
        tracing::trace!(
            target: "holon::runtime_db::connection",
            held_write_lock_us = 13_u64,
            callback_us = 7_u64,
            commit_us = 3_u64,
            "runtime db write committed"
        );
    });
    let writes = writer.0.lock().unwrap();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].held_us, 23);
    assert_eq!(writes[0].callback_us, Some(17));
    assert_eq!(writes[0].commit_us, Some(5));
    assert_eq!(writes[1].held_us, 13);
    assert_eq!(writes[1].callback_us, Some(7));
    assert_eq!(writes[1].commit_us, Some(3));
}

// These measurements intentionally use APIs present at d8c60e8e9 as well as in
// the patch. No branch chooses a different implementation based on the revision.
struct FixtureSample {
    started: Instant,
    before: Vec<crate::diagnostics::attribution::StageSnapshot>,
}

impl FixtureSample {
    fn start(writer: &WriterTrace) -> Self {
        writer.0.lock().unwrap().clear();
        writer.1.lock().unwrap().clear();
        *writer.2.lock().unwrap() = 0;
        Self {
            started: Instant::now(),
            before: crate::diagnostics::attribution::snapshot(),
        }
    }

    fn finish(self, writer: &WriterTrace) -> serde_json::Value {
        let elapsed_us = self.started.elapsed().as_micros();
        let before = self
            .before
            .into_iter()
            .map(|stage| (stage.name.clone(), stage))
            .collect::<std::collections::BTreeMap<_, _>>();
        let stages = crate::diagnostics::attribution::snapshot()
            .into_iter()
            .map(|after| {
                let old = before.get(&after.name);
                let count = after.count - old.map_or(0, |stage| stage.count);
                let rows = after.rows - old.map_or(0, |stage| stage.rows);
                let ns = after.total_ns - old.map_or(0, |stage| stage.total_ns);
                (
                    after.name,
                    serde_json::json!({"count": count, "rows": rows, "ns": ns}),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        serde_json::json!({
            "elapsed_us": elapsed_us,
            "preparations": std::mem::take(&mut *writer.1.lock().unwrap()),
            "candidate_retries": std::mem::take(&mut *writer.2.lock().unwrap()),
            "writes": std::mem::take(&mut *writer.0.lock().unwrap()),
            "missing_task_stages": missing_task_stages(&stages),
            "stages": stages,
            "selected_rows": null,
            "decoded_bytes": null,
            "qualification_ns": null,
            "counter_note": TASK_COUNTER_NOTE,
        })
    }
}

fn fixture_revision() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn history_tasks(at: chrono::DateTime<Utc>) -> Vec<TaskRecord> {
    let mut tasks = (0..OUTPUT_TASKS)
        .map(|index| TaskRecord {
            id: format!("fixture-output-{index:03}"),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Cancelled,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        })
        .collect::<Vec<_>>();
    // Terminal payloads are immutable; keep the fixed terminal selection and
    // update a separate active Task through the real command/recovery paths.
    let mut command = tasks[0].clone();
    command.id = HISTORY_COMMAND_TASK.into();
    command.status = TaskStatus::Running;
    tasks.push(command);
    tasks
}

async fn report_history_branch(
    runtime: &RuntimeHandle,
    writer: &WriterTrace,
    revision: &Option<String>,
    headers: usize,
    trial: usize,
    branch: &str,
    measurement: serde_json::Value,
) -> anyhow::Result<()> {
    let prompt = FixtureSample::start(writer);
    let (provider_us, delivery_us) = deliver_prompt(runtime, "default").await?;
    println!(
        "TASK_OUTPUT_HISTORY {}",
        serde_json::json!({
            "revision": revision,
            "branch": branch,
            "headers": headers,
            "trial": trial,
            "configuration": {
                "worker_threads": 4, "branch_concurrency": 1,
                "terminal_limit": OUTPUT_TASKS, "trials": HISTORY_TRIALS,
                "provider": "StubProvider", "database": "synthetic temporary",
            },
            "selected_tasks": OUTPUT_TASKS + 1,
            "selected_terminal_tasks": OUTPUT_TASKS,
            "selected_active_tasks": 1,
            "original_task_kind": "CommandTask update through commit_task_transition; no subprocess creation",
            "measurement": measurement,
            "prompt": prompt.finish(writer),
            "provider_us": provider_us,
            "delivery_us": delivery_us,
        }),
    );
    Ok(())
}

async fn prepare_history_request(
    runtime: &RuntimeHandle,
    request: &crate::runtime::resident_task::TaskCaptureRequest,
) -> anyhow::Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    mutation.prepare_owner_task_request(request).await?;
    let captured = mutation.capture_tasks(request)?;
    assert_eq!(captured.coverage.terminal.limit, OUTPUT_TASKS);
    assert_eq!(captured.coverage.output.heads.len(), OUTPUT_TASKS + 1);
    for index in 0..OUTPUT_TASKS {
        assert!(captured
            .coverage
            .output
            .heads
            .contains_key(&format!("fixture-output-{index:03}")));
    }
    assert!(captured.coverage.output.heads[HISTORY_COMMAND_TASK]
        .source
        .is_none());
    let source = captured.coverage.output.heads["fixture-output-000"]
        .source
        .as_ref()
        .context("fixture output source missing")?;
    assert_eq!(source.message.key.id, "history-00000000");
    assert_eq!(
        source.availability,
        crate::runtime::resident_task::TaskOutputSourceAvailability::MissingVersion
    );
    Ok(())
}

fn history_task_command(
    runtime: &RuntimeHandle,
    task: TaskRecord,
    fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
) -> crate::runtime_db::transitions::TaskTransitionCommand {
    crate::runtime_db::transitions::TaskTransitionCommand {
        agent_id: "default".into(),
        expected_owner_incarnation: runtime.instance_incarnation(),
        task,
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        agent_state: None,
        message_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: true,
        commit_on_idempotent: false,
        fault,
    }
}

fn seed_output_history(
    db: &crate::runtime_db::RuntimeDb,
    header_count: usize,
) -> anyhow::Result<()> {
    db.transaction(|tx| {
        let mut insert = tx.prepare(
            "INSERT INTO task_message_order_headers
             VALUES('default',1,?1,?1,NULL,'2000-01-01T00:00:00.000Z',1,?2)",
        )?;
        let digest = "0".repeat(64);
        for index in 0..header_count {
            insert.execute(rusqlite::params![format!("history-{index:08}"), digest])?;
        }
        let mut insert_source = tx.prepare(
            "INSERT INTO task_message_sources VALUES('default',1,?1,?2,1,'cancelled',?3)",
        )?;
        for index in 0..OUTPUT_TASKS {
            let header_index = if index % 2 == 0 {
                index
            } else {
                header_count - OUTPUT_TASKS + index
            };
            let source = format!("history-{header_index:08}");
            let locator = serde_json::json!({
                "scope": {"agent_id":"default","identity_incarnation":1},
                "key": {"family":"message","id":source},
                "revision": 1, "content_digest":digest,
            });
            insert_source.execute(rusqlite::params![
                source,
                format!("fixture-output-{index:03}"),
                locator.to_string(),
            ])?;
        }
        Ok(())
    })
}

async fn legacy_history_trial(
    writer: &WriterTrace,
    revision: &Option<String>,
    headers: usize,
    trial: usize,
    at: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
    };

    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let path = home.path().join("legacy.sqlite");
    let mut connection = rusqlite::Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 89)
    {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    crate::runtime_db::evidence::upsert_agent_identity_tx(
        &tx,
        &AgentIdentityRecord::new(
            "default",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
    )?;
    crate::runtime_db::evidence::upsert_agent_state_tx(&tx, &AgentState::new("default"))?;
    tx.execute(
        "INSERT INTO storage_domains
         (domain,schema_version,import_status,canonical_source,updated_at)
         VALUES('context_episode_anchors',1,'complete','db',?1)",
        [crate::runtime_db::repositories::timestamp(at)],
    )?;
    for task in history_tasks(at) {
        crate::runtime_db::repositories::upsert_task_tx(&tx, &task)?;
    }
    tx.commit()?;
    drop(connection);
    let upgrade_started = Instant::now();
    let db =
        crate::runtime_db::RuntimeDb::open_and_migrate(&path, home.path().join("legacy.lock"))?;
    let upgrade_us = upgrade_started.elapsed().as_micros();
    // History is varied after the real v89 upgrade, outside the measured
    // preparation. This measures first use of legacy-qualified Tasks, not
    // history-dependent schema-migration/backfill cost.
    seed_output_history(&db, headers)?;
    let sample = FixtureSample::start(writer);
    let runtime = RuntimeHandle::new_with_clock_and_runtime_db(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new(REPLY)),
        "default".into(),
        context_config(),
        controlled_clock(),
        Some(db),
    )?;
    isolate_test_user_home(&runtime);
    prepare_history_request(
        &runtime,
        &crate::runtime::resident_task::TaskCaptureRequest::current(),
    )
    .await?;
    let mut measurement = sample.finish(writer);
    measurement["schema_upgrade_setup_us"] = serde_json::json!(upgrade_us);
    measurement["legacy_note"] = serde_json::json!(
        "v89 Tasks upgraded by real migrations; history added afterward; schema upgrade is setup, not the history-scaling sample"
    );
    report_history_branch(
        &runtime,
        writer,
        revision,
        headers,
        trial,
        "legacy_first_prepare",
        measurement,
    )
    .await
}

fn recheck_stage_totals() -> Vec<crate::diagnostics::attribution::StageSnapshot> {
    crate::diagnostics::attribution::snapshot()
        .into_iter()
        .filter(|sample| {
            sample.name.contains(".task_result.defer.")
                || sample.name.starts_with("runtime.task.")
                || sample.name.starts_with("db.task.")
        })
        .collect()
}

async fn deliver_prompt(runtime: &RuntimeHandle, agent_id: &str) -> anyhow::Result<(u128, u128)> {
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Reply with the fixed fixture text.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    let trace = crate::observability::TraceContext::new_root(true);
    message.trace_context = Some(trace.clone());
    let message = runtime.enqueue(message).await?;
    let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await?;
    let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
        anyhow::bail!("fixture prompt was not scheduled");
    };
    assert_eq!(scheduled.message.id, message.id);
    let terminal = runtime
        .process_message_with_plan_deferred(
            scheduled.message,
            scheduled.dispatch_plan,
            &scheduled.scheduler_decision,
        )
        .await?;
    let mut entry = runtime
        .runtime_db()
        .queue_entries()
        .latest(&message.id)?
        .context("fixture queue entry missing")?;
    entry.status = QueueEntryStatus::Processed;
    entry.updated_at = runtime.now().max(entry.updated_at);
    runtime
        .commit_queue_terminal_settlement(entry, Vec::new(), true, Some(&terminal))
        .await?;
    let completed = crate::observability::recent_trace(&trace.trace_id)
        .context("fixture prompt trace missing")?;
    let provider = completed
        .spans
        .iter()
        .find(|span| span.name == "holon.provider.round")
        .context("fixture provider round missing")?;
    let provider_us = (provider.started_at - completed.started_at)
        .num_microseconds()
        .context("fixture provider timestamp overflow")?;
    let delivery = completed
        .spans
        .iter()
        .find(|span| span.name == "holon.delivery")
        .context("fixture delivery span missing")?;
    let delivery_us = (delivery.completed_at - completed.started_at)
        .num_microseconds()
        .context("fixture delivery timestamp overflow")?;
    let briefs = runtime.storage().read_recent_briefs(4)?;
    assert!(briefs.iter().any(|brief| {
        brief.related_message_id.as_deref() == Some(&message.id) && brief.text == REPLY
    }));
    Ok((u128::try_from(provider_us)?, u128::try_from(delivery_us)?))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit shared-writer performance fixture"]
async fn task_result_recheck_writer_contention() -> anyhow::Result<()> {
    let writer = WriterTrace::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(writer.clone()))?;
    let baseline = std::env::var_os("HOLON_RECHECK_CONTENTION_BASELINE").is_some();
    let revision = fixture_revision();
    for header_count in [2_000, 20_000, 100_000] {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let clock = controlled_clock();
        let provider = Arc::new(StubProvider::new(REPLY));
        let background = RuntimeHandle::new_with_clock_and_runtime_db(
            "default",
            home.path().join("background"),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
            clock.clone(),
            None,
        )?;
        let db = background.runtime_db().clone();
        let foreground = RuntimeHandle::new_with_clock_and_runtime_db(
            "foreground",
            home.path().join("foreground"),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider,
            "default".into(),
            context_config(),
            clock.clone(),
            Some(db.clone()),
        )?;
        isolate_test_user_home(&background);
        isolate_test_user_home(&foreground);

        // Seed the background order-header namespace with older synthetic
        // headers. Native result writes then publish coverage over this fixture.
        db.transaction(|tx| {
            let mut insert = tx.prepare(
                "INSERT INTO task_message_order_headers
                 (agent_id,identity_incarnation,evidence_id,message_id,message_seq,created_at,revision,content_digest)
                 VALUES('default',1,?1,?1,NULL,'2000-01-01T00:00:00.000Z',1,?2)",
            )?;
            let digest = "0".repeat(64);
            for index in 0..header_count {
                insert.execute(rusqlite::params![format!("fixture-history-{index:08}"), digest])?;
            }
            Ok(())
        })?;

        let mut records = Vec::new();
        for index in 0..RESULTS {
            if index % 8 == 0 {
                let owner = background
                    .create_work_item(
                        format!("fixture reply owner {index}"),
                        Some(crate::types::WorkItemPlanStatus::NeedsInput),
                        None,
                        Vec::new(),
                    )
                    .await?;
                let (task, reply_id) = super::super::repeated_failure_tests::reply_result_with_key(
                    &background,
                    &owner,
                    &format!("contention-reply-{index}"),
                )
                .await;
                let mut entry = db
                    .queue_entries()
                    .latest(&reply_id)?
                    .context("fixture reply queue missing")?;
                entry.status = QueueEntryStatus::Dropped;
                entry.updated_at = clock.now();
                background
                    .commit_queue_settlement(
                        entry,
                        vec![AuditEvent::legacy(
                            "scheduler_authority_input_rejected",
                            serde_json::json!({
                                "agent_id": "default", "message_id": reply_id,
                                "reason": "canonical_internal_followup_work_item_not_runnable",
                                "queue_disposition": "dropped",
                            }),
                        )],
                        true,
                    )
                    .await?;
                let record = db
                    .task_result_settlements()
                    .latest_for_message(task.parent_message_id.as_deref().unwrap())?
                    .context("fixture reply settlement missing")?;
                records.push((record, "dropped"));
                continue;
            }
            let record = pending_command_result(
                &background,
                &format!("contention-task-{index}"),
                clock.now() - chrono::Duration::minutes(5),
            )
            .await;
            if index % 8 == 1 {
                db.queue_entries().upsert(&QueueEntryRecord {
                    message_id: record.message_id.clone(),
                    agent_id: "default".into(),
                    priority: Priority::Next,
                    status: QueueEntryStatus::Quarantined,
                    created_at: record.created_at,
                    updated_at: clock.now(),
                })?;
                records.push((record, "invalid"));
                continue;
            }
            let waiter = WorkItemRecord::new("default", "holds exact result", WorkItemState::Open);
            background.storage().append_work_item(&waiter)?;
            background
                .storage()
                .append_wait_condition(&WaitConditionRecord {
                    id: format!("contention-wait-{index}"),
                    agent_id: "default".into(),
                    work_item_id: Some(waiter.id),
                    status: WaitConditionStatus::Active,
                    kind: WaitConditionKind::Task,
                    source: None,
                    subject_ref: Some(record.task_id.clone()),
                    waiting_for: "fixture task result".into(),
                    wake_sources: vec![WakeSource::TaskResult {
                        task_id: record.task_id.clone(),
                    }],
                    continuation: None,
                    created_at: clock.now(),
                    updated_at: clock.now(),
                    expires_at: None,
                    resolved_at: None,
                    cancelled_at: None,
                    turn_id: None,
                    trigger_message_id: None,
                    triggered_at: None,
                })?;
            records.push((record, "waiting"));
        }

        clock.advance(Duration::from_secs(60));
        let mut queue_waits = Vec::new();
        let mut steady_writer_us = Vec::new();
        let mut saw_convergence_write = false;
        for trial in 0..TRIALS {
            writer.0.lock().unwrap().clear();
            writer.1.lock().unwrap().clear();
            *writer.2.lock().unwrap() = 0;
            let before = recheck_stage_totals();
            let scan = tokio::spawn({
                let runtime = background.clone();
                async move {
                    let started = Instant::now();
                    let admitted = runtime.emit_due_task_result_recovery().await?;
                    Ok::<_, anyhow::Error>((admitted, started.elapsed().as_micros()))
                }
                .instrument(tracing::info_span!("fixture_recovery_scan"))
            });
            let (provider_us, delivery_us) = deliver_prompt(&foreground, "foreground").await?;
            let (admitted, scan_us) = scan.await??;
            assert!(!admitted);
            let stages = recheck_stage_totals()
                .into_iter()
                .zip(before)
                .map(|(after, before)| {
                    (
                        after.name,
                        serde_json::json!({
                            "count": after.count - before.count,
                            "rows": after.rows - before.rows,
                            "ns": after.total_ns - before.total_ns,
                            "us": (after.total_ns - before.total_ns) / 1_000,
                        }),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let writes = std::mem::take(&mut *writer.0.lock().unwrap());
            let preparations = std::mem::take(&mut *writer.1.lock().unwrap());
            assert_eq!(
                preparations.values().sum::<u64>(),
                stages["runtime.task.prepare.attempt"]["count"]
                    .as_u64()
                    .context("Task preparation attempt count missing")?,
                "caller/outcome trace must account for each preparation attempt"
            );
            let prompt_queue_ms = writes
                .iter()
                .find(|write| {
                    !write.recovery && write.operation == "runtime_db.prepared_input.queue_admit"
                })
                .with_context(|| {
                    format!(
                        "fixture prompt writer trace missing among {:?}",
                        writes
                            .iter()
                            .map(|write| &write.operation)
                            .collect::<Vec<_>>()
                    )
                })?
                .queue_ms;
            queue_waits.push(prompt_queue_ms);
            let recovery_writer_us: u64 = writes
                .iter()
                .filter(|write| write.recovery)
                .map(|write| write.held_us)
                .sum();
            saw_convergence_write |= writes.iter().any(|write| {
                write.recovery
                    && write.operation == "runtime_db.prepared_input.task_result.converge"
            });
            let defer_writer_us: u64 = writes
                .iter()
                .filter(|write| write.operation == "runtime_db.prepared_input.task_result.defer")
                .map(|write| write.held_us)
                .sum();
            if baseline {
                assert_eq!(
                    stages["db.task_result.defer.result"]["rows"],
                    RESULT_RECHECK_LIMIT
                );
                assert!(defer_writer_us > 0);
                assert!(recovery_writer_us >= defer_writer_us);
            } else {
                assert_eq!(stages["db.task_result.defer.result"]["rows"], 0);
                assert_eq!(defer_writer_us, 0);
                if trial >= RESULTS / RESULT_RECHECK_LIMIT {
                    steady_writer_us.push(recovery_writer_us);
                    for (record, kind) in &records {
                        let actual = db
                            .task_result_settlements()
                            .latest_for_message(&record.message_id)?
                            .context("fixture settlement missing")?;
                        match *kind {
                            "invalid" => {
                                assert_eq!(actual.state, TaskResultSettlementState::Settled);
                                assert_eq!(
                                    actual.disposition,
                                    Some(TaskResultSettlementDisposition::InvalidOrStale)
                                );
                            }
                            "dropped" => {
                                assert_eq!(
                                    actual.state,
                                    TaskResultSettlementState::PersistedPending
                                );
                                assert_eq!(
                                    actual.deferred_reason.as_deref(),
                                    Some("awaiting_exact_waiter")
                                );
                                assert_eq!(actual.next_recheck_at, None);
                            }
                            _ => assert_eq!(&actual, record),
                        }
                        assert_eq!(actual.recovery_attempts, 0);
                    }
                }
            }
            println!(
                "RECHECK_CONTENTION {}",
                serde_json::json!({
                    "revision": revision,
                    "branch": "shared_writer_result_recheck",
                    "headers": header_count,
                    "trial": trial,
                    "results": RESULTS,
                    "scan_us": scan_us,
                    "prompt_queue_ms": prompt_queue_ms,
                    "defer_writer_us": defer_writer_us,
                    "recovery_writer_us": recovery_writer_us,
                    "provider_us": provider_us,
                    "delivery_us": delivery_us,
                    "preparations": preparations,
                    "candidate_retries": *writer.2.lock().unwrap(),
                    "writes": writes,
                    "missing_task_stages": missing_task_stages(&stages),
                    "stages": stages,
                    "selected_rows": null,
                    "decoded_bytes": null,
                    "counter_note": TASK_COUNTER_NOTE,
                })
            );
            clock.advance(Duration::from_secs(RESULT_RECHECK_SECONDS as u64 + 1));
        }
        if !baseline {
            assert!(
                saw_convergence_write,
                "fixture did not attribute the recovery convergence write"
            );
            queue_waits.sort_unstable();
            let p95 = queue_waits[(queue_waits.len() * 95).div_ceil(100) - 1];
            assert!(
                p95 < 100,
                "prompt writer queue P95 was {p95} ms at {header_count} headers"
            );
            assert!(
                steady_writer_us
                    .windows(2)
                    .all(|pair| pair.iter().sum::<u64>() < 1_000_000),
                "recovery exceeded one second of writer time per minute"
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit Task output history performance fixture"]
async fn task_output_history_writer_cost() -> anyhow::Result<()> {
    use crate::runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
    use crate::runtime_db::transitions::TransitionFaultPoint;

    let writer = WriterTrace::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(writer.clone()))?;
    let revision = fixture_revision();
    for header_count in [2_000, 20_000, 100_000] {
        let mut harness = LifecycleHarness::with_provider(Arc::new(StubProvider::new(REPLY)));
        let runtime = harness.runtime();
        let db = runtime.runtime_db();
        let tasks = history_tasks(harness.now());
        let mut changed_task = tasks.last().context("fixture task missing")?.clone();
        let keys = tasks
            .iter()
            .map(|task| TaskKey::new(TaskFamily::Task, &task.id))
            .collect();
        super::owned_task_fixture_write(runtime, keys, move |tx| {
            for task in &tasks {
                crate::runtime_db::repositories::upsert_task_tx(tx, task)?;
            }
            Ok(())
        })
        .await;
        // Headers are older than native prompt writes. Half the Tasks have
        // a source in the recent window, and half use the historical fallback.
        seed_output_history(db, header_count)?;

        for trial in 0..HISTORY_TRIALS {
            let runtime = harness.runtime();
            // Prime the common request outside the sample, then force a new
            // exact absent key. Keep 128 terminal Tasks plus one active Task.
            prepare_history_request(runtime, &TaskCaptureRequest::current()).await?;
            let mut request = TaskCaptureRequest::current();
            request.required_keys.insert(TaskKey::new(
                TaskFamily::Task,
                format!("fixture-missing-{trial}"),
            ));
            let sample = FixtureSample::start(&writer);
            prepare_history_request(runtime, &request).await?;
            let miss = sample.finish(&writer);
            assert!(
                miss["stages"]["runtime.task.prepare.miss.coverage"]["count"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0,
                "fixture must execute a real coverage miss"
            );
            // No intervening prompt/publication: repeat the exact request, not
            // an Arc comparison, and account for the actual reuse event.
            let sample = FixtureSample::start(&writer);
            prepare_history_request(runtime, &request).await?;
            let warm = sample.finish(&writer);
            assert!(
                warm["stages"]["runtime.task.prepare.reuse"]["count"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0,
                "fixture must execute real warm preparation reuse"
            );
            report_history_branch(
                runtime,
                &writer,
                &revision,
                header_count,
                trial,
                "forced_coverage_miss",
                miss,
            )
            .await?;
            report_history_branch(
                runtime,
                &writer,
                &revision,
                header_count,
                trial,
                "warm_reuse",
                warm,
            )
            .await?;

            changed_task.summary = Some(format!("original Task command trial {trial}"));
            let sample = FixtureSample::start(&writer);
            let mut mutation = runtime.begin_mutation().await?;
            let (commit, deletion) = mutation
                .commit_task_transition(
                    format!("fixture-original-task-{trial}"),
                    history_task_command(runtime, changed_task.clone(), None),
                    Default::default(),
                    true,
                    None,
                )
                .await?;
            assert!(commit.applied);
            assert!(deletion.is_none());
            drop(mutation);
            runtime.apply_transition_commit(commit).await;
            let original = sample.finish(&writer);
            assert_eq!(
                runtime.task_record(&changed_task.id).await?,
                Some(changed_task.clone())
            );
            report_history_branch(
                runtime,
                &writer,
                &revision,
                header_count,
                trial,
                "original_task_command",
                original,
            )
            .await?;

            // A new RuntimeHandle has no resident Task publication. Reopening
            // the same synthetic DB exercises real cold preparation, not a
            // fabricated coverage flag. This is not a legacy schema upgrade.
            let sample = FixtureSample::start(&writer);
            harness.restart();
            let runtime = harness.runtime();
            prepare_history_request(runtime, &TaskCaptureRequest::current()).await?;
            let cold = sample.finish(&writer);
            report_history_branch(
                runtime,
                &writer,
                &revision,
                header_count,
                trial,
                "cold_restart",
                cold,
            )
            .await?;

            // Commit durably, fail before publication, then enter the original
            // known-commit recovery path. No second business callback is issued.
            changed_task.summary = Some(format!("receipt recovery trial {trial}"));
            let nonce = format!("fixture-receipt-recovery-{trial}");
            let sample = FixtureSample::start(&writer);
            let mut mutation = runtime.begin_mutation().await?;
            let error = mutation
                .commit_task_transition(
                    nonce.clone(),
                    history_task_command(
                        runtime,
                        changed_task.clone(),
                        Some(TransitionFaultPoint::BeforeCacheUpdate),
                    ),
                    Default::default(),
                    true,
                    None,
                )
                .await
                .expect_err("fixture must fail after the known commit");
            assert!(
                error.to_string().contains("committed but publication"),
                "{error:#}"
            );
            drop(mutation);
            let fault_commit = sample.finish(&writer);
            let events_before = runtime.storage().read_recent_events(100)?;
            let sample = FixtureSample::start(&writer);
            drop(runtime.begin_mutation().await?);
            let mut recovered = sample.finish(&writer);
            assert_eq!(
                runtime.task_record(&changed_task.id).await?,
                Some(changed_task.clone())
            );
            assert_eq!(runtime.storage().read_recent_events(100)?, events_before);
            let receipts: i64 = runtime.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
                [&nonce],
                |row| row.get(0),
            )?;
            assert_eq!(
                receipts, 1,
                "recovery must retain exactly the original receipt"
            );
            recovered["fault_commit_setup"] = fault_commit;
            report_history_branch(
                runtime,
                &writer,
                &revision,
                header_count,
                trial,
                "receipt_recovery",
                recovered,
            )
            .await?;
            legacy_history_trial(&writer, &revision, header_count, trial, harness.now()).await?;
        }
    }
    Ok(())
}
