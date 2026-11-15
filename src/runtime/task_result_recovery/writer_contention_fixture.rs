use super::{pending_command_result, *};
use crate::provider::StubProvider;
use crate::runtime::tests::support::{
    context_config, controlled_clock, isolate_test_user_home, LifecycleHarness,
};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::Instrument;
use tracing_subscriber::prelude::*;

#[derive(Default)]
struct WriterEvent {
    held_us: u64,
    queue_ms: u64,
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
}
impl tracing::field::Visit for PreparationEvent {
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "caller" => value.clone_into(&mut self.caller),
            "outcome" => value.clone_into(&mut self.outcome),
            "miss" => value.clone_into(&mut self.miss),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct WriterTrace(
    Arc<Mutex<Vec<WriterEvent>>>,
    Arc<Mutex<std::collections::BTreeMap<String, u64>>>,
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
        if event.metadata().target() == "holon::performance"
            && event.metadata().fields().field("caller").is_some()
            && event.metadata().fields().field("outcome").is_some()
        {
            let mut fields = PreparationEvent::default();
            event.record(&mut fields);
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
        if event.metadata().target() != "holon::runtime_db::connection"
            || event
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

fn recheck_stage_totals() -> Vec<crate::diagnostics::attribution::StageSnapshot> {
    crate::diagnostics::attribution::snapshot()
        .into_iter()
        .filter(|sample| {
            sample.name.contains(".task_result.defer.")
                || sample.name.starts_with("runtime.task.")
                || sample.name.starts_with("db.task.domain_")
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
                    "stages": stages,
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

    let writer = WriterTrace::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(writer.clone()))?;
    let stage_totals = || {
        crate::diagnostics::attribution::snapshot()
            .into_iter()
            .filter(|stage| {
                stage.name.starts_with("db.input.")
                    || stage.name.starts_with("db.queue.input.")
                    || matches!(
                        stage.name.as_str(),
                        "db.task.output_heads" | "db.task.domain_prepare"
                    )
            })
            .collect::<Vec<_>>()
    };
    let stage_delta = |before: Vec<crate::diagnostics::attribution::StageSnapshot>| {
        stage_totals()
            .into_iter()
            .zip(before)
            .map(|(after, before)| (after.name, (after.total_ns - before.total_ns) / 1_000))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    for header_count in [2_000, 20_000, 100_000] {
        let harness = LifecycleHarness::with_provider(Arc::new(StubProvider::new(REPLY)));
        let runtime = harness.runtime();
        let db = runtime.runtime_db();
        let tasks = (0..128)
            .map(|index| TaskRecord {
                id: format!("fixture-output-{index:03}"),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Cancelled,
                created_at: harness.now(),
                updated_at: harness.now(),
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: None,
                recovery: None,
            })
            .collect::<Vec<_>>();
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
            for index in 0..128 {
                let header_index = if index % 2 == 0 {
                    index
                } else {
                    header_count - 128 + index
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
        })?;

        for trial in 0..3 {
            let mut mutation = runtime.begin_mutation().await?;
            let mut request = TaskCaptureRequest::current();
            request.required_keys.insert(TaskKey::new(
                TaskFamily::Task,
                format!("fixture-missing-{trial}"),
            ));
            writer.0.lock().unwrap().clear();
            let before = stage_totals();
            mutation.prepare_owner_task_request(&request).await?;
            let captured = mutation.capture_tasks(&request)?;
            assert_eq!(captured.coverage.terminal.limit, 128);
            assert_eq!(captured.coverage.output.heads.len(), 128);
            let source = captured.coverage.output.heads["fixture-output-000"]
                .source
                .as_ref()
                .context("fixture output source missing")?;
            assert_eq!(source.message.key.id, "history-00000000");
            assert_eq!(
                source.availability,
                crate::runtime::resident_task::TaskOutputSourceAvailability::MissingVersion
            );
            drop((captured, mutation));
            let writes = std::mem::take(&mut *writer.0.lock().unwrap());
            let prepare = writes
                .first()
                .context("Task prepare writer trace missing")?;
            assert_eq!(writes.len(), 1);
            let prepare_stages = stage_delta(before);

            let before = stage_totals();
            let (provider_us, delivery_us) = deliver_prompt(runtime, "default").await?;
            let writes = std::mem::take(&mut *writer.0.lock().unwrap());
            let writer_event = |operation| {
                writes
                    .iter()
                    .find(|write| write.operation == operation)
                    .with_context(|| format!("writer trace missing for {operation}"))
            };
            let admit = writer_event("runtime_db.prepared_input.queue_admit")?;
            let claim = writer_event("runtime_db.prepared_input.queue_claim")?;
            println!(
                "TASK_OUTPUT_HISTORY {}",
                serde_json::json!({
                    "headers": header_count,
                    "trial": trial,
                    "prepare_hold_us": prepare.held_us,
                    "prepare_queue_ms": prepare.queue_ms,
                    "prepare_stages_us": prepare_stages,
                    "prompt_stages_us": stage_delta(before),
                    "admit_hold_us": admit.held_us,
                    "admit_queue_ms": admit.queue_ms,
                    "claim_hold_us": claim.held_us,
                    "claim_queue_ms": claim.queue_ms,
                    "provider_us": provider_us,
                    "delivery_us": delivery_us,
                }),
            );
        }
    }
    Ok(())
}
