use super::*;
use crate::{
    runtime::clock::TestClock,
    runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    },
    types::{CommandTaskSpec, TaskStatus, TurnTerminalKind, WorkItemPlanStatus},
};
use std::{
    collections::BTreeSet,
    sync::atomic::Ordering,
    time::{Duration as StdDuration, Instant},
};

#[test]
fn new_task_result_root_reaches_fifth_http_fallback_with_old_lineages_retained() {
    run_fifth_root_http(false);
}

#[test]
fn queued_task_result_root_reaches_fifth_http_fallback_after_restart() {
    run_fifth_root_http(true);
}

fn run_fifth_root_http(restart: bool) {
    let thread = std::thread::Builder::new()
        .name("task-result-root-http-fairness".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(fifth_root_http(restart));
        })
        .unwrap();
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn fifth_root_http(restart: bool) {
    let primary = OutageServer::start(false).await;
    let fallback = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&primary.url, &fallback.url]);
    let clock = Arc::new(TestClock::new(Utc::now()));
    let mut runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
    let owner = runtime
        .create_work_item(
            "task-result HTTP fairness".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let task = runtime
        .schedule_command_task(
            "root fairness command".into(),
            CommandTaskSpec {
                cmd: "printf root-fairness-evidence".into(),
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 0,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::RuntimeInstruction,
        )
        .await
        .unwrap();
    let deadline = Instant::now() + StdDuration::from_secs(10);
    let result_id = loop {
        let task = runtime.task_record(&task.id).await.unwrap().unwrap();
        if task.status == TaskStatus::Completed {
            break task.parent_message_id.unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "{task:?}; events={:?}",
            runtime.storage().read_recent_events(20).unwrap()
        );
        tokio::time::sleep(StdDuration::from_millis(1)).await;
    };
    let latest = |runtime: &RuntimeHandle| {
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&result_id)
            .unwrap()
            .unwrap()
    };
    let mut new_sources = BTreeSet::new();
    let mut fifth_source = None;
    let mut fifth_fallback_calls = None;
    let mut restarted = false;
    let mut dispatches = std::collections::VecDeque::new();
    let mut scanner_dispatches = Vec::new();
    let mut fifth_dispatches = Vec::new();
    for _ in 0..512 {
        let emitted = runtime.emit_due_task_result_recovery().await.unwrap();
        let record = latest(&runtime);
        if restart && !restarted && record.recovery_attempts == 2 && emitted {
            drop(runtime);
            runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
            Box::pin(runtime.recover_scheduler_bootstrap_claims())
                .await
                .unwrap();
            assert_eq!(latest(&runtime).recovery_attempts, 2);
            restarted = true;
        }
        if record.settled_at.is_some() {
            break;
        }
        let poll = SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap();
        if let RunLoopPoll::Message(scheduled) = poll {
            let message = scheduled.message;
            let root =
                crate::runtime::provider_recovery::resolve_for_context(runtime.storage(), &message)
                    .unwrap()
                    .map_or_else(|| message.id.clone(), |source| source.root_message.id);
            let fresh_scanner = matches!(
                &message.origin,
                MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
            ) && new_sources.insert(message.id.clone());
            let fifth = fresh_scanner && record.recovery_attempts == 4;
            if fifth {
                fifth_source = Some(root.clone());
                fifth_fallback_calls = Some(fallback.calls.load(Ordering::SeqCst));
            }
            fallback
                .healthy
                .store(fifth_source.as_ref() == Some(&root), Ordering::SeqCst);
            let calls_before = (
                primary.calls.load(Ordering::SeqCst),
                fallback.calls.load(Ordering::SeqCst),
            );
            let provider = format!("{:?}", scheduled.dispatch_plan.provider_admission);
            let episode = scheduled
                .dispatch_plan
                .provider_admission
                .as_ref()
                .map(|admission| admission.episode().unwrap());
            let terminal = Box::pin(runtime.process_message_with_plan_deferred(
                message.clone(),
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            ))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "context dispatch failed: {error:#}; message={}, root={root}, \
                     attempts={}, fresh={fresh_scanner}, fifth={fifth_source:?}, \
                     provider={provider}, recent_dispatches={dispatches:#?}",
                    message.id, record.recovery_attempts,
                )
            });
            let dispatch = format!(
                "message={}, root={root}, attempts={}, fresh={fresh_scanner}, fifth={fifth_source:?}, \
                 calls={calls_before:?}->{:?}, now={}, provider={provider}, \
                 episode={episode:?}, terminal={:?}",
                message.id,
                record.recovery_attempts,
                (
                    primary.calls.load(Ordering::SeqCst),
                    fallback.calls.load(Ordering::SeqCst)
                ),
                runtime.now(),
                terminal.terminal,
            );
            if fresh_scanner {
                scanner_dispatches.push(dispatch.clone());
            }
            if fifth_source.as_ref() == Some(&root) && fifth_dispatches.len() < 16 {
                fifth_dispatches.push(dispatch.clone());
            }
            dispatches.push_back(dispatch);
            if dispatches.len() > 12 {
                dispatches.pop_front();
            }
            let mut entry = runtime
                .runtime_db()
                .queue_entries()
                .latest(&message.id)
                .unwrap()
                .unwrap();
            entry.status = crate::types::QueueEntryStatus::Processed;
            entry.updated_at = runtime.now().max(entry.updated_at);
            assert!(runtime
                .commit_queue_terminal_settlement(entry, Vec::new(), true, Some(&terminal))
                .await
                .unwrap());
            continue;
        }
        let now = clock.now();
        let guard = runtime.inner.agent.lock().await;
        let mut deadlines = guard
            .queue
            .input_snapshot(512)
            .iter()
            .filter_map(|message| {
                runtime
                    .runtime_db()
                    .provider_queue_parking("default", &message.id)
                    .unwrap()
                    .map(|parking| parking.not_before)
            })
            .filter(|deadline| *deadline > now)
            .collect::<Vec<_>>();
        deadlines.extend(
            latest(&runtime)
                .next_recheck_at
                .filter(|deadline| *deadline > now),
        );
        drop(guard);
        let next = deadlines
            .into_iter()
            .min()
            .unwrap_or(now + Duration::seconds(1));
        clock.advance((next - now).to_std().unwrap());
    }
    let delivered = latest(&runtime);
    let pending = runtime
        .inner
        .agent
        .lock()
        .await
        .queue
        .input_snapshot(32)
        .into_iter()
        .map(|message| (message.id, message.priority, message.origin))
        .collect::<Vec<_>>();
    assert_eq!(
        delivered.recovery_attempts,
        5,
        "retained provider lineages must not starve a new recovery root: \
         selected_scanner_roots={}, primary_calls={}, fallback_calls={}, pending={pending:?}, \
         settlement={delivered:?}",
        new_sources.len(),
        primary.calls.load(Ordering::SeqCst),
        fallback.calls.load(Ordering::SeqCst),
    );
    assert_eq!(
        delivered.state,
        TaskResultSettlementState::Settled,
        "settlement={delivered:?}, pending={pending:?}, \
         scanners={scanner_dispatches:#?}, fifth_dispatches={fifth_dispatches:#?}, \
         recent_dispatches={dispatches:#?}"
    );
    assert!(delivered.settled_at.is_some(), "{delivered:?}");
    assert_eq!(
        delivered.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(restarted, restart);
    let ledger: (u32, u32) = runtime
        .runtime_db()
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT root_attempt_id)
                 FROM task_result_recovery_admissions WHERE result_identity = ?1",
                [&delivered.result_identity],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        ledger,
        (5, 5),
        "fallbacks must not create additional charges"
    );
    assert!(
        fallback.calls.load(Ordering::SeqCst) > fifth_fallback_calls.unwrap(),
        "the fifth root must make a real fallback HTTP request"
    );
    assert!(primary.calls.load(Ordering::SeqCst) >= 5);
    let completed = runtime
        .storage()
        .read_recent_turns(512)
        .unwrap()
        .into_iter()
        .filter(|turn| {
            turn.terminal.as_ref().map(|terminal| terminal.kind)
                == Some(TurnTerminalKind::Completed)
        })
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1, "one successful durable consumption");
    let protocol = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        protocol.attempts[delivered.activation_id.as_ref().unwrap()]
            .turn_id
            .as_deref(),
        Some(completed[0].turn_id.as_str())
    );
    assert_eq!(
        completed[0].current_work_item_id.as_deref(),
        Some(owner.id.as_str())
    );
    let successes = runtime
        .storage()
        .read_recent_briefs(512)
        .unwrap()
        .into_iter()
        .filter(|brief| {
            brief.text == "restored"
                && brief.turn_id.as_deref() == Some(completed[0].turn_id.as_str())
        })
        .count();
    assert_eq!(successes, 1);
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .settle_activation(
                "default",
                delivered.activation_id.as_deref().unwrap(),
                TaskResultSettlementDisposition::ModelDelivered,
                runtime.now(),
            )
            .unwrap(),
        0,
        "replayed settlement cannot consume the result twice"
    );
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
}
