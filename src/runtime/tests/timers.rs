use super::super::*;
use super::support::*;

// Keep the test task runnable so paused Tokio cannot jump to the old deadline.
async fn yield_timer_loop() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_settles_registered_wait_without_resuming_work() {
    for work_item_scope in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap();
        let work_id = if work_item_scope {
            Some(
                runtime
                    .create_work_item("timer wait".into(), None, None, Vec::new())
                    .await
                    .unwrap()
                    .id,
            )
        } else {
            None
        };
        let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                work_id.clone(),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        clock.rewind(std::time::Duration::from_secs(1));
        let cancellation =
            crate::tool::tools::timer::cancel(&runtime, &serde_json::json!({"timer_id": timer.id}))
                .await
                .unwrap();
        let evidence = cancellation.envelope.result.as_ref().unwrap();
        assert_eq!(evidence["id"], timer.id);
        assert_eq!(
            evidence["cancelled_wait_condition_ids"],
            serde_json::json!([registration.condition.id])
        );
        let wait = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == registration.condition.id)
            .unwrap();
        assert_eq!(wait.status, WaitConditionStatus::Cancelled);
        assert_eq!(wait.cancelled_at, Some(clock.now()));
        assert_eq!(wait.updated_at, registration.condition.updated_at);
        assert_eq!(
            wait.continuation.as_ref().unwrap()["terminal_reason"],
            "timer_cancelled"
        );
        assert!(runtime
            .storage()
            .raw_unresolved_wait_conditions_for_agent("default")
            .unwrap()
            .is_empty());
        if let Some(work_id) = work_id {
            let work = runtime
                .storage()
                .latest_work_item(&work_id)
                .unwrap()
                .unwrap();
            assert_eq!(work.state, WorkItemState::Open);
            assert!(work
                .blocked_by
                .as_deref()
                .unwrap()
                .contains("timer_cancelled"));
            assert!(work.blocked_by_wait_id.is_none());
            assert!(work.recheck_at.is_none());
        }
        assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
        let before = runtime.storage().latest_wait_conditions().unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
        assert_eq!(runtime.storage().latest_wait_conditions().unwrap(), before);
        clock.advance(std::time::Duration::from_secs(180));
        tokio::time::advance(std::time::Duration::from_secs(180)).await;
        yield_timer_loop().await;
        assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
        assert!(!runtime
            .storage()
            .read_recent_messages(100)
            .unwrap()
            .iter()
            .any(|message| message.kind == MessageKind::TimerTick));
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_fences_prepared_wait_commit() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let PrepareWaitForOutcome::Prepared(prepared) = runtime
        .prepare_wait_for_outcome(
            "default",
            None,
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "prepared timer wait".into(),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("timer wait must be prepared");
    };
    runtime.cancel_timer(&timer.id).await.unwrap();
    let error = runtime
        .inner
        .runtime_db
        .transitions()
        .commit_wait(&prepared.command)
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .is_some());
    assert!(runtime
        .storage()
        .raw_unresolved_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_fences_waiting_report_transaction() {
    use crate::runtime_db::transitions::{
        ExecutionProtocolTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
    };
    for cancel in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let PrepareWaitForOutcome::Prepared(prepared) = runtime
            .prepare_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "waiting report settlement".into(),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("timer wait must be prepared");
        };
        let source = MessageEnvelope::new(
            "default",
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: timer.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "source turn".into(),
            },
        );
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "waiting for timer",
            Some(source.id.clone()),
            None,
        );
        let command = QueueTransitionCommand {
            agent_id: "default".into(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Next,
                status: QueueEntryStatus::Processed,
                created_at: source.created_at,
                updated_at: source.created_at,
            }),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: vec![source.clone()],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: vec![brief.clone()],
        };
        if cancel {
            runtime.cancel_timer(&timer.id).await.unwrap();
        }
        let result = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue_terminal_with_wait(
                &command,
                &ExecutionProtocolTransition::default(),
                &prepared.command,
                None,
                &[],
                None,
            );
        if cancel {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("timer_wait"), "{error:#}");
            assert!(runtime
                .storage()
                .read_brief_by_id(&brief.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&source.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .storage()
                .raw_unresolved_wait_conditions_for_agent("default")
                .unwrap()
                .is_empty());
        } else {
            assert!(result.unwrap().applied);
            assert!(runtime
                .storage()
                .read_brief_by_id(&brief.id)
                .unwrap()
                .is_some());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_settles_only_matching_waits_and_owned_blockers() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let unrelated_timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let mut matching = Vec::new();
    let mut manual_blocker = None;
    for index in 0..2 {
        let work = runtime
            .create_work_item(format!("wait {index}"), None, None, Vec::new())
            .await
            .unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "matching dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        matching.push(registration.condition);
        if index == 1 {
            let mut work = runtime
                .storage()
                .latest_work_item(&work.id)
                .unwrap()
                .unwrap();
            work.blocked_by = Some("independent manual blocker".into());
            work.blocked_by_wait_id = None;
            work.revision += 1;
            runtime.storage().append_work_item(&work).unwrap();
            manual_blocker = Some(work);
        }
    }
    let unrelated = runtime
        .register_wait_for(
            "default",
            None,
            WaitForWakeKind::Timer,
            Some(unrelated_timer.id.clone()),
            "unrelated timer".into(),
            None,
        )
        .await
        .unwrap()
        .condition;
    // A malformed legacy foreign-agent reference must not expand cancellation scope.
    let mut foreign = matching[0].clone();
    foreign.id = "foreign-timer-wait".into();
    foreign.agent_id = "another-agent".into();
    foreign.work_item_id = None;
    runtime.storage().append_wait_condition(&foreign).unwrap();
    runtime.cancel_timer(&timer.id).await.unwrap();
    let waits = runtime.storage().latest_wait_conditions().unwrap();
    for expected in &matching {
        assert_eq!(
            waits
                .iter()
                .find(|wait| wait.id == expected.id)
                .unwrap()
                .status,
            WaitConditionStatus::Cancelled
        );
    }
    assert_eq!(
        waits.iter().find(|wait| wait.id == foreign.id).unwrap(),
        &foreign
    );
    assert_eq!(
        waits.iter().find(|wait| wait.id == unrelated.id).unwrap(),
        &unrelated
    );
    let manual_blocker = manual_blocker.unwrap();
    assert_eq!(
        runtime
            .storage()
            .latest_work_item(&manual_blocker.id)
            .unwrap()
            .unwrap(),
        manual_blocker
    );
    let projected = runtime
        .latest_work_item(matching[0].work_item_id.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(projected
        .blocked_by
        .as_deref()
        .unwrap()
        .contains("timer_cancelled"));
    assert!(projected.blocked_by_wait_id.is_none());
}

#[tokio::test(start_paused = true)]
async fn timer_fire_between_prepare_and_commit_remains_waitable() {
    for repeat in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let mut timer = runtime
            .schedule_timer(60_000, repeat.then_some(60_000), None)
            .await
            .unwrap();
        let PrepareWaitForOutcome::Prepared(prepared) = runtime
            .prepare_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer may fire during preparation".into(),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("timer wait must be prepared");
        };
        runtime.fire_timer_record(&mut timer).await.unwrap();
        runtime
            .inner
            .runtime_db
            .transitions()
            .commit_wait(&prepared.command)
            .unwrap();
        assert!(runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_respects_wake_admission_boundary() {
    for admitted in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let mut timer = runtime
            .schedule_timer(60_000, Some(60_000), None)
            .await
            .unwrap();
        let registered = runtime
            .register_wait_for(
                "default",
                None,
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer admission race".into(),
                None,
            )
            .await
            .unwrap();
        runtime.fire_timer_record(&mut timer).await.unwrap();
        let wake = runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .unwrap();
        if admitted {
            assert!(matches!(
                scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                    .poll()
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::Message(_)
            ));
        }
        let before = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == registered.condition.id)
            .unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
        let after = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == before.id)
            .unwrap();
        let queue = runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap();
        if admitted {
            assert_eq!(after, before, "cancellation must not retract admitted work");
            assert_eq!(queue.status, QueueEntryStatus::Dequeued);
        } else {
            assert_eq!(before.status, WaitConditionStatus::Triggered);
            assert_eq!(after.status, WaitConditionStatus::Cancelled);
            assert_eq!(after.trigger_message_id, before.trigger_message_id);
            assert_eq!(after.triggered_at, before.triggered_at);
            assert_eq!(queue.status, QueueEntryStatus::Dropped);
            assert!(matches!(
                scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                    .poll()
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::Idle
            ));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_wait_write_failure_rolls_back_every_domain() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let work = runtime
        .create_work_item("atomic cancel".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut timer = runtime
        .schedule_timer(60_000, Some(60_000), None)
        .await
        .unwrap();
    runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "cancel rollback".into(),
            Some(120_000),
        )
        .await
        .unwrap();
    runtime.fire_timer_record(&mut timer).await.unwrap();
    let before_waits = runtime.storage().latest_wait_conditions().unwrap();
    let before_work = runtime.storage().latest_work_item(&work.id).unwrap();
    let before_state = runtime.agent_state().await.unwrap();
    let before_events = runtime.storage().read_recent_events(100).unwrap();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_timer_wait_cancel BEFORE UPDATE ON wait_conditions
         WHEN NEW.status = 'cancelled'
         BEGIN SELECT RAISE(ABORT, 'injected wait cancellation failure'); END;",
        )
        .unwrap();
    assert!(runtime
        .cancel_timer(&timer.id)
        .await
        .unwrap_err()
        .to_string()
        .contains("injected wait cancellation failure"));
    assert_eq!(
        runtime.latest_timer(&timer.id).await.unwrap().unwrap(),
        timer
    );
    assert_eq!(
        runtime.storage().latest_wait_conditions().unwrap(),
        before_waits
    );
    assert_eq!(
        runtime.storage().latest_work_item(&work.id).unwrap(),
        before_work
    );
    assert_eq!(runtime.agent_state().await.unwrap(), before_state);
    assert_eq!(
        runtime.storage().read_recent_events(100).unwrap(),
        before_events
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap(),
        Some(wake.clone())
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_timer_wait_cancel;")
        .unwrap();
    runtime.cancel_timer(&timer.id).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_legacy_orphan_recovery_and_repeat_cancel_are_idempotent() {
    for restart in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let make_runtime = || {
            RuntimeHandle::new_with_clock(
                "default",
                dir.path().to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(StubProvider::new("must not run")),
                "default".into(),
                context_config(),
                clock.clone(),
            )
            .unwrap()
        };
        let runtime = make_runtime();
        let work = runtime
            .create_work_item("legacy orphan".into(), None, None, Vec::new())
            .await
            .unwrap();
        let mut timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let registered = runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "legacy cancelled dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        // Reproduce the pre-fix durable shape, bypassing the repaired command.
        timer.status = TimerStatus::Cancelled;
        timer.next_fire_at = None;
        runtime.storage().append_timer(&timer).unwrap();
        clock.rewind(std::time::Duration::from_secs(1));
        let runtime = if restart { make_runtime() } else { runtime };
        runtime.cancel_timer(&timer.id).await.unwrap();
        let waits = runtime.storage().latest_wait_conditions().unwrap();
        let wait = waits
            .iter()
            .find(|wait| wait.id == registered.condition.id)
            .unwrap();
        assert_eq!(wait.status, WaitConditionStatus::Cancelled);
        assert_eq!(wait.cancelled_at, Some(clock.now()));
        assert_eq!(wait.updated_at, registered.condition.updated_at);
        let work = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
        assert!(work
            .blocked_by
            .as_deref()
            .unwrap()
            .contains("timer_cancelled"));
        assert!(work.blocked_by_wait_id.is_none());
        assert!(work.recheck_at.is_none());
        let events = runtime.storage().read_recent_events(100).unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
        let recovered_again = make_runtime();
        assert_eq!(
            recovered_again.storage().latest_wait_conditions().unwrap(),
            waits
        );
        assert_eq!(
            recovered_again.storage().read_recent_events(100).unwrap(),
            events
        );
        assert_eq!(recovered_again.agent_state().await.unwrap().pending, 0);
    }
}

async fn assert_timer_fire_count(
    runtime: &RuntimeHandle,
    timer_id: &str,
    expected: u64,
) -> TimerRecord {
    for _ in 0..100 {
        let timer = runtime
            .recent_timers(100)
            .await
            .unwrap()
            .into_iter()
            .find(|timer| timer.id == timer_id)
            .unwrap();
        if timer.fire_count == expected {
            return timer;
        }
        tokio::task::yield_now().await;
    }
    panic!("timer {timer_id} did not reach fire_count {expected}");
}

fn assert_single_durable_timer_wake(runtime: &RuntimeHandle, timer_id: &str) -> String {
    let messages = runtime
        .storage()
        .read_recent_messages(100)
        .unwrap()
        .into_iter()
        .filter(|message| {
            matches!(&message.origin, MessageOrigin::Timer { timer_id: id } if id == timer_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(timer_id)
        .unwrap()
        .expect("timer must retain a durable pending wake");
    assert_eq!(wake.message_id, messages[0].id);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    wake.message_id
}

#[tokio::test(start_paused = true)]
async fn one_shot_timer_rechecks_wall_clock_after_long_suspend() {
    for hours in [24, 48] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap();
        let timer = runtime.schedule_timer(3_600_000, None, None).await.unwrap();
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, 0).await;

        clock.advance(std::time::Duration::from_secs(hours * 3_600));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        let fired = assert_timer_fire_count(&runtime, &timer.id, 1).await;
        assert_eq!(fired.status, TimerStatus::Completed);
        assert_single_durable_timer_wake(&runtime, &timer.id);
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);

        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, 1).await;
        assert_single_durable_timer_wake(&runtime, &timer.id);
    }
}

#[tokio::test(start_paused = true)]
async fn repeating_timer_rechecks_wall_clock_on_each_arm_without_replaying_history() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(7 * 3_600_000), None)
        .await
        .unwrap();
    let original_deadline = timer.next_fire_at.unwrap();
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 0).await;

    let mut first_wake = None;
    for (round, hours) in [24, 48].into_iter().enumerate() {
        clock.advance(std::time::Duration::from_secs(hours * 3_600));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        let fired = assert_timer_fire_count(&runtime, &timer.id, (round + 1) as u64).await;
        assert_eq!(fired.status, TimerStatus::Active);
        let next = fired.next_fire_at.unwrap();
        assert!(next > clock.now());
        assert!(next <= clock.now() + chrono::Duration::hours(7));
        assert_eq!(
            (next - original_deadline).num_milliseconds() % (7 * 3_600_000),
            0
        );
        let wake_id = assert_single_durable_timer_wake(&runtime, &timer.id);
        if let Some(first) = &first_wake {
            assert_eq!(&wake_id, first);
        } else {
            first_wake = Some(wake_id);
        }
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
        // Let the next sleep arm before separating the clocks again.
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, (round + 1) as u64).await;
    }
    runtime.cancel_timer(&timer.id).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn suppressed_timer_fire_emits_observable_audit_event() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(7 * 3_600_000), None)
        .await
        .unwrap();
    yield_timer_loop().await;

    // First fire creates the durable pending wake.
    clock.advance(std::time::Duration::from_secs(24 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 1).await;
    let wake_id = assert_single_durable_timer_wake(&runtime, &timer.id);

    // Second fire is suppressed by the one-pending-wake guard and must be
    // observable instead of silently advancing fire_count (#3607).
    clock.advance(std::time::Duration::from_secs(8 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 2).await;
    assert_eq!(
        assert_single_durable_timer_wake(&runtime, &timer.id),
        wake_id
    );

    let suppressed = runtime
        .storage()
        .read_recent_events(64)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "timer_wake_suppressed")
        .expect("suppressed timer fire must emit an audit event");
    assert_eq!(suppressed.data["timer_id"], serde_json::json!(timer.id));
    assert_eq!(suppressed.data["fire_count"], serde_json::json!(2));
    assert_eq!(
        suppressed.data["pending_message_id"],
        serde_json::json!(wake_id)
    );
    assert!(suppressed.data["pending_age_ms"].as_i64().unwrap() > 0);
    runtime.cancel_timer(&timer.id).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_sleeping_timer_cannot_fire_after_wall_clock_suspend() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(3_600_000), None)
        .await
        .unwrap();
    yield_timer_loop().await;
    runtime.cancel_timer(&timer.id).await.unwrap();
    clock.advance(std::time::Duration::from_secs(48 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    let cancelled = assert_timer_fire_count(&runtime, &timer.id, 0).await;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert!(!runtime.storage().read_recent_messages(100).unwrap().iter().any(
        |message| matches!(&message.origin, MessageOrigin::Timer { timer_id } if timer_id == &timer.id)
    ));
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
}

#[tokio::test(start_paused = true)]
async fn runtime_fires_overdue_timer_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-recover".into(),
            agent_id: "default".into(),
            created_at: now - chrono::Duration::milliseconds(10),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer recovered".into()),
            next_fire_at: Some(now - chrono::Duration::milliseconds(5)),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| events.iter().any(|event| event.kind == "timer_fired"),
        "recovered overdue timer",
    )
    .await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-recover" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test(start_paused = true)]
async fn runtime_recovers_active_timer_without_next_fire_at() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-missing-next-fire".into(),
            agent_id: "default".into(),
            created_at: now - chrono::Duration::milliseconds(20),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer fallback".into()),
            next_fire_at: None,
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer fallback done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| events.iter().any(|event| event.kind == "timer_fired"),
        "recovered timer without next_fire_at",
    )
    .await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-missing-next-fire" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test(start_paused = true)]
async fn runtime_recovers_overdue_repeating_timer_on_original_schedule() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let scheduled_fire_at = now - chrono::Duration::seconds(5);
    storage
        .append_timer(&TimerRecord {
            id: "timer-recover-repeat".into(),
            agent_id: "default".into(),
            created_at: scheduled_fire_at - chrono::Duration::seconds(1),
            duration_ms: 1_000,
            interval_ms: Some(1_000),
            repeat: true,
            status: TimerStatus::Active,
            summary: Some("repeating timer recovered".into()),
            next_fire_at: Some(scheduled_fire_at),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| {
            events.iter().any(|event| {
                event.kind == "timer_fired"
                    && event
                        .data
                        .get("timer_id")
                        .and_then(serde_json::Value::as_str)
                        == Some("timer-recover-repeat")
            })
        },
        "recovered overdue repeating timer",
    )
    .await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-recover-repeat" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Active);
    assert_eq!(timer.next_fire_at, Some(now + chrono::Duration::seconds(1)));
    runtime_task.abort();
}

#[tokio::test]
async fn schedule_timer_rejects_unrepresentable_duration() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let result = runtime.schedule_timer(u64::MAX, None, None).await;
    assert!(result.is_err());
}

#[tokio::test(start_paused = true)]
async fn timer_message_binds_the_unique_matching_wait_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let work = runtime
        .create_work_item("wait for timer".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let timer = runtime
        .schedule_timer(100, None, Some("bound timer".into()))
        .await
        .unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "waiting for bound timer".into(),
            None,
        )
        .await
        .unwrap();

    clock.advance(std::time::Duration::from_millis(100));
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    let message = runtime
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .into_iter()
        .find(|message| message.kind == MessageKind::TimerTick)
        .expect("timer tick should be queued");
    assert_eq!(message.work_item_id.as_deref(), Some(work.id.as_str()));
    assert_eq!(message.source_refs.get("timer_id"), Some(&timer.id));
    let triggered = runtime
        .storage()
        .latest_wait_conditions()
        .unwrap()
        .into_iter()
        .find(|condition| condition.id == registration.condition.id)
        .expect("timer wait should remain durable");
    assert_eq!(triggered.status, WaitConditionStatus::Triggered);
    assert_eq!(
        triggered.trigger_message_id(),
        Some(message.id.as_str()),
        "timer fire must trigger the exact wait in the enqueue transaction"
    );
}

#[tokio::test(start_paused = true)]
async fn timer_fire_fault_rolls_back_the_wait_and_wake_transaction() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let work = runtime
        .create_work_item("atomic timer wake".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let mut timer = runtime
        .schedule_timer(10_000, None, Some("atomic timer".into()))
        .await
        .unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "waiting for atomic timer".into(),
            None,
        )
        .await
        .unwrap();

    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    );
    let error = runtime.fire_timer_record(&mut timer).await.unwrap_err();
    assert_injected_transition_fault(&error);
    let persisted = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.id == timer.id)
        .unwrap();
    assert_eq!(persisted.status, TimerStatus::Active);
    assert_eq!(persisted.fire_count, 0);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert!(!runtime
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .iter()
        .any(|message| message.kind == MessageKind::TimerTick));
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .map(|condition| condition.status),
        Some(WaitConditionStatus::Active)
    );
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);

    runtime.fire_timer_record(&mut timer).await.unwrap();
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .map(|condition| condition.status),
        Some(WaitConditionStatus::Triggered)
    );
}

#[tokio::test(start_paused = true)]
async fn repeating_timer_coalesces_to_one_pending_wake() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let mut timer = runtime
        .schedule_timer(10_000, Some(10_000), Some("coalesced timer".into()))
        .await
        .unwrap();

    for _ in 0..25 {
        runtime.fire_timer_record(&mut timer).await.unwrap();
    }

    let persisted = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.id == timer.id)
        .unwrap();
    assert_eq!(persisted.fire_count, 25);
    let messages = runtime
        .storage()
        .read_recent_messages(100)
        .unwrap()
        .into_iter()
        .filter(|message| {
            matches!(
                &message.origin,
                MessageOrigin::Timer { timer_id } if timer_id == &timer.id
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, messages[0].id);
    assert_eq!(wake.fire_count, 1);
    assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_timer_drops_pending_wake_and_stale_fire_cannot_revive_it() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let mut stale_timer = runtime
        .schedule_timer(10_000, Some(10_000), Some("cancel timer".into()))
        .await
        .unwrap();
    runtime.fire_timer_record(&mut stale_timer).await.unwrap();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&stale_timer.id)
        .unwrap()
        .unwrap();

    let cancelled = runtime.cancel_timer(&stale_timer.id).await.unwrap();
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&stale_timer.id)
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Dropped
    );
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);

    runtime.fire_timer_record(&mut stale_timer).await.unwrap();
    let persisted = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.id == stale_timer.id)
        .unwrap();
    assert_eq!(persisted.status, TimerStatus::Cancelled);
    assert_eq!(persisted.fire_count, 1);
}

#[tokio::test(start_paused = true)]
async fn completed_one_shot_wait_binds_existing_pending_wake() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "timer handled",
    });
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let work = runtime
        .create_work_item("late timer wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let mut work_message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "run before late timer wait".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    bind_autonomous_work_queue_tick(&mut work_message, &work, "continue_active");
    work_message.turn_id = Some("turn-late-one-shot-wait".into());
    let work_message = runtime.enqueue(work_message).await.unwrap();
    assert!(matches!(
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        scheduler_executor::RunLoopPoll::Message(_)
    ));

    let mut timer = runtime
        .schedule_timer(10_000, None, Some("one shot".into()))
        .await
        .unwrap();
    runtime.fire_timer_record(&mut timer).await.unwrap();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();

    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "wait registered after fire".into(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        registration.condition.status,
        WaitConditionStatus::Triggered
    );
    assert_eq!(
        registration.condition.trigger_message_id(),
        Some(wake.message_id.as_str())
    );

    finish_claimed_test_run(&runtime).await;
    let terminal = terminal_transition(&work_message, Some(&work.id));
    runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: work_message.id.clone(),
                agent_id: work_message.agent_id.clone(),
                priority: work_message.priority,
                status: QueueEntryStatus::Processed,
                created_at: work_message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        200,
        |events| {
            events.iter().any(|event| {
                event.kind == "queue_entry_settled"
                    && event.data["message_id"] == wake.message_id.as_str()
            })
        },
        "late one-shot timer wait settlement",
    )
    .await;
    runtime_task.abort();

    assert_eq!(provider.call_count().await, 1);
    let condition = runtime
        .storage()
        .latest_wait_conditions()
        .unwrap()
        .into_iter()
        .find(|condition| condition.id == registration.condition.id)
        .unwrap();
    assert_eq!(condition.status, WaitConditionStatus::Resolved);
    let work = runtime
        .inner
        .runtime_db
        .work_items()
        .latest(&work.id)
        .unwrap()
        .unwrap();
    assert!(work.blocked_by.is_none());
}

#[tokio::test(start_paused = true)]
async fn recovery_normalizes_legacy_timer_backlog_idempotently() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active_timer = TimerRecord {
        id: "legacy-active-timer".into(),
        agent_id: "default".into(),
        created_at: now,
        duration_ms: 10_000,
        interval_ms: Some(10_000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("legacy active".into()),
        next_fire_at: Some(now + chrono::Duration::seconds(10)),
        last_fired_at: Some(now),
        fire_count: 3,
    };
    let cancelled_timer = TimerRecord {
        id: "legacy-cancelled-timer".into(),
        status: TimerStatus::Cancelled,
        next_fire_at: None,
        summary: Some("legacy cancelled".into()),
        ..active_timer.clone()
    };
    storage.append_timer(&active_timer).unwrap();
    storage.append_timer(&cancelled_timer).unwrap();

    let mut active_message_ids = Vec::new();
    let mut cancelled_message_ids = Vec::new();
    for (timer, ids) in [
        (&active_timer, &mut active_message_ids),
        (&cancelled_timer, &mut cancelled_message_ids),
    ] {
        for _ in 0..3 {
            let message = MessageEnvelope {
                metadata: Some(serde_json::json!({ "timer_id": timer.id })),
                ..MessageEnvelope::new(
                    "default",
                    MessageKind::TimerTick,
                    MessageOrigin::Timer {
                        timer_id: timer.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text {
                        text: "legacy timer tick".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TimerScheduler,
                    AdmissionContext::RuntimeOwned,
                )
            };
            storage.append_message(&message).unwrap();
            storage
                .append_queue_entry(&QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: "default".into(),
                    priority: Priority::Next,
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: message.created_at,
                })
                .unwrap();
            ids.push(message.id);
        }
    }

    let make_runtime = || {
        RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap()
    };
    let runtime = make_runtime();
    let active_statuses = active_message_ids
        .iter()
        .map(|message_id| {
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(message_id)
                .unwrap()
                .unwrap()
                .status
        })
        .collect::<Vec<_>>();
    assert_eq!(
        active_statuses
            .iter()
            .filter(|status| **status == QueueEntryStatus::Queued)
            .count(),
        1
    );
    assert_eq!(
        active_statuses
            .iter()
            .filter(|status| **status == QueueEntryStatus::Dropped)
            .count(),
        2
    );
    assert!(cancelled_message_ids.iter().all(|message_id| {
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(message_id)
            .unwrap()
            .unwrap()
            .status
            == QueueEntryStatus::Dropped
    }));
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&active_timer.id)
        .unwrap()
        .unwrap();
    assert!(active_message_ids.contains(&wake.message_id));
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&cancelled_timer.id)
        .unwrap()
        .is_none());
    assert_eq!(runtime.agent_state().await.unwrap().pending, 1);

    let recovered_again = make_runtime();
    let wake_again = recovered_again
        .inner
        .runtime_db
        .timers()
        .pending_wake(&active_timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake_again.message_id, wake.message_id);
    assert_eq!(recovered_again.agent_state().await.unwrap().pending, 1);
}

#[tokio::test(start_paused = true)]
async fn recovery_reactivates_incorporated_wake_for_restored_timer_message() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let timer = TimerRecord {
        id: "timer-restored".into(),
        agent_id: "default".into(),
        created_at: now,
        duration_ms: 10_000,
        interval_ms: Some(10_000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("restored timer".into()),
        next_fire_at: Some(now + chrono::Duration::seconds(10)),
        last_fired_at: Some(now),
        fire_count: 1,
    };
    storage.append_timer(&timer).unwrap();
    let message = MessageEnvelope {
        metadata: Some(serde_json::json!({ "timer_id": timer.id })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: timer.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "restored timer tick".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TimerScheduler,
            AdmissionContext::RuntimeOwned,
        )
    };
    storage.append_message(&message).unwrap();
    storage
        .append_queue_entry(&QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: "default".into(),
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        })
        .unwrap();

    let make_runtime = || {
        RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap()
    };

    // First bootstrap records the pending wake for the queued timer message.
    let runtime = make_runtime();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, message.id);

    // A claim incorporates the wake; a daemon restart then restores the claimed
    // message to the queue while the wake row for the same (timer, message)
    // pair is still stored.
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE timer_wakes SET status = 'incorporated', incorporated_at = ?2
             WHERE timer_id = ?1 AND message_id = ?3",
            rusqlite::params![timer.id, now.to_rfc3339(), message.id],
        )
        .unwrap();

    // Recovery must be idempotent instead of failing the whole bootstrap with a
    // timer_wakes primary-key conflict.
    let recovered = make_runtime();
    let wake = recovered
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, message.id);
    assert_eq!(recovered.agent_state().await.unwrap().pending, 1);
}
