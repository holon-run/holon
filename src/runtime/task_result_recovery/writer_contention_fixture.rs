use super::{pending_command_result, *};
use crate::provider::StubProvider;
use crate::runtime::tests::support::{context_config, controlled_clock, isolate_test_user_home};
use std::time::{Duration, Instant};

const RESULTS: usize = 8;
const TRIALS: usize = 5;
const REPLY: &str = "task-result writer contention fixture reply";

fn defer_totals() -> Vec<crate::diagnostics::attribution::StageSnapshot> {
    crate::diagnostics::attribution::snapshot()
        .into_iter()
        .filter(|sample| sample.name.contains(".task_result.defer."))
        .collect()
}

async fn deliver_prompt(runtime: &RuntimeHandle) -> anyhow::Result<(u128, u128)> {
    let mut message = MessageEnvelope::new(
        "foreground",
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
    for header_count in [2_000, 20_000] {
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
            let record = pending_command_result(
                &background,
                &format!("contention-task-{index}"),
                clock.now() - chrono::Duration::minutes(5),
            )
            .await;
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
            records.push(record);
        }

        for trial in 0..TRIALS {
            let before = defer_totals();
            let scan = tokio::spawn({
                let runtime = background.clone();
                async move {
                    let started = Instant::now();
                    let admitted = runtime.emit_due_task_result_recovery().await?;
                    Ok::<_, anyhow::Error>((admitted, started.elapsed().as_micros()))
                }
            });
            let (provider_us, delivery_us) = deliver_prompt(&foreground).await?;
            let (admitted, scan_us) = scan.await??;
            assert!(!admitted);
            let stages = defer_totals()
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
            assert_eq!(stages["db.task_result.defer.result"]["rows"], RESULTS);
            for record in &records {
                let actual = db
                    .task_result_settlements()
                    .latest_for_message(&record.message_id)?
                    .context("fixture settlement missing")?;
                assert_eq!(actual.recovery_attempts, 0);
                assert_eq!(
                    actual.next_recheck_at,
                    Some(clock.now() + chrono::Duration::seconds(RESULT_RECHECK_SECONDS))
                );
            }
            println!(
                "RECHECK_CONTENTION {}",
                serde_json::json!({
                    "headers": header_count,
                    "trial": trial,
                    "results": RESULTS,
                    "scan_us": scan_us,
                    "provider_us": provider_us,
                    "delivery_us": delivery_us,
                    "stages": stages,
                })
            );
            clock.advance(Duration::from_secs(RESULT_RECHECK_SECONDS as u64 + 1));
        }
    }
    Ok(())
}
