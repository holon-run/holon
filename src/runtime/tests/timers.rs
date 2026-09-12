use super::super::*;
use super::support::*;

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
