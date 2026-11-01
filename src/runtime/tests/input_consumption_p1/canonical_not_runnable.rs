//! Canonical input retention while the WorkItem execution is temporarily not
//! runnable (#3773): task results settle reducer-only with a pending
//! obligation, other validated runtime inputs defer in place instead of being
//! dropped, and later legal inputs keep advancing.
use super::*;
use crate::types::QueueEntryStatus;

async fn advance_past_wall_time(harness: &mut LifecycleHarness) {
    // Turn creation still uses wall time; keep test-clock terminals after it.
    let offset = Utc::now()
        .signed_duration_since(harness.now())
        .to_std()
        .unwrap();
    harness
        .advance(offset + std::time::Duration::from_secs(60))
        .await;
}

async fn publish_owner_terminal_result(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: &str,
) -> String {
    let agent_id = runtime.agent_id().await.unwrap();
    let detail = serde_json::json!({
        "rejoin_obligation_id": task_id,
        "rejoin_generation": 1,
        "parent_turn_id": "issue-3773-parent",
    });
    let mut message = MessageEnvelope::new(
        &agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "owner task result body".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.turn_id = Some(crate::ids::turn_id());
    message.work_item_id = Some(owner_id.into());
    message.metadata = Some(serde_json::json!({
        "task_id": task_id,
        "task_kind": "command_task",
        "task_status": "completed",
        "task_result_id": message.id,
        "task_detail": detail,
        "work_item_id": owner_id,
    }));
    let task = TaskRecord {
        id: task_id.into(),
        agent_id,
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: Some(owner_id.into()),
        summary: None,
        detail: Some(detail),
        recovery: None,
    };
    runtime
        .commit_terminal_task_result(&task, "task_completed", &message)
        .await
        .unwrap();
    message.id
}

fn queue_status(runtime: &RuntimeHandle, message_id: &str) -> QueueEntryStatus {
    runtime
        .runtime_db()
        .queue_entries()
        .latest(message_id)
        .unwrap()
        .unwrap_or_else(|| panic!("queue entry for {message_id} is missing"))
        .status
}

#[tokio::test(start_paused = true)]
async fn issue_3773_task_result_settles_pending_when_execution_not_runnable() {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    advance_past_wall_time(&mut harness).await;
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "issue 3773 owner result retention".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let message_id = publish_owner_terminal_result(runtime, &owner.id, "issue-3773-result").await;
    let owner_latest = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
    seed_live_paused_work_execution(runtime, &owner_latest, "issue-3773 lane paused").await;

    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => panic!("expected reducer-only claim, got idle"),
        _ => panic!("expected reducer-only claim, got a non-message poll"),
    };
    assert_eq!(scheduled.message.id, message_id);
    assert!(!scheduled.scheduler_decision.model_reentry);
    process_scheduled(runtime, scheduled).await;

    // The durable input is admitted reducer-only, not dropped or quarantined,
    // and its obligation stays pending for the next legal activation.
    assert_eq!(
        queue_status(runtime, &message_id),
        QueueEntryStatus::Processed
    );
    let settlement = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&message_id)
        .unwrap()
        .expect("terminal result keeps a durable settlement");
    assert_eq!(
        settlement.state,
        crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending
    );
    assert!(provider.requests.lock().await.is_empty());

    // Repeated polls stay stable: no requeue, no drop, no quarantine.
    for _ in 0..4 {
        assert!(matches!(
            scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await
                .unwrap(),
            scheduler_executor::RunLoopPoll::Idle
        ));
    }
    assert_eq!(
        queue_status(runtime, &message_id),
        QueueEntryStatus::Processed
    );
}

async fn bound_internal_followup(runtime: &RuntimeHandle, work_item_id: &str) -> String {
    let agent_id = runtime.agent_id().await.unwrap();
    let message = MessageEnvelope::new(
        &agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "runtime_followup".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "continue the interrupted objective".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    let mut message = message;
    message.work_item_id = Some(work_item_id.into());
    runtime.enqueue(message).await.unwrap().id
}

async fn work_queue_tick(runtime: &RuntimeHandle, work_item_id: &str) -> String {
    let agent_id = runtime.agent_id().await.unwrap();
    let work_item = runtime
        .inner
        .storage
        .latest_work_item(work_item_id)
        .unwrap()
        .unwrap();
    let mut tick = MessageEnvelope::new(
        &agent_id,
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "issue 3773 later legal input".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    bind_autonomous_work_queue_tick(&mut tick, &work_item, "queued_available");
    runtime.enqueue(tick).await.unwrap().id
}

#[tokio::test(start_paused = true)]
async fn issue_3773_internal_followup_defers_without_drop_or_starvation() {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    advance_past_wall_time(&mut harness).await;
    let runtime = harness.runtime();
    let blocked = runtime
        .create_work_item(
            "issue 3773 deferred followup owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let runnable = runtime
        .create_work_item(
            "issue 3773 later legal input owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(blocked.id.clone()).await.unwrap();
    let followup_id = bound_internal_followup(runtime, &blocked.id).await;
    let tick_id = work_queue_tick(runtime, &runnable.id).await;
    let blocked_latest = runtime
        .latest_work_item(&blocked.id)
        .await
        .unwrap()
        .unwrap();
    seed_live_paused_work_execution(runtime, &blocked_latest, "issue-3773 lane paused").await;
    let runnable_latest = runtime
        .latest_work_item(&runnable.id)
        .await
        .unwrap()
        .unwrap();
    seed_live_runnable_work_execution(runtime, &runnable_latest).await;

    // The deferred followup is skipped within the same poll and the later
    // legal input still advances.
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => {
            panic!("expected the later legal input to claim, got idle")
        }
        _ => panic!("expected the later legal input to claim, got a non-message poll"),
    };
    assert_eq!(scheduled.message.id, tick_id);
    assert_eq!(
        queue_status(runtime, &followup_id),
        QueueEntryStatus::Queued
    );

    // Repeated polls never escalate the deferral to quarantine.
    for _ in 0..4 {
        assert!(matches!(
            scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await
                .unwrap(),
            scheduler_executor::RunLoopPoll::Idle | scheduler_executor::RunLoopPoll::Message(_)
        ));
        assert_eq!(
            queue_status(runtime, &followup_id),
            QueueEntryStatus::Queued
        );
    }
    assert_ne!(
        queue_status(runtime, &followup_id),
        QueueEntryStatus::Quarantined
    );
    assert_ne!(
        queue_status(runtime, &followup_id),
        QueueEntryStatus::Dropped
    );
}

#[tokio::test(start_paused = true)]
async fn issue_3773_deferred_followup_survives_restart_and_claims_when_runnable() {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    advance_past_wall_time(&mut harness).await;
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "issue 3773 restart retention owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let followup_id = bound_internal_followup(runtime, &owner.id).await;
    let owner_latest = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
    seed_live_paused_work_execution(runtime, &owner_latest, "issue-3773 lane paused").await;
    assert!(matches!(
        scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap(),
        scheduler_executor::RunLoopPoll::Idle
    ));

    harness.restart();
    let runtime = harness.runtime();
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    assert_eq!(
        queue_status(runtime, &followup_id),
        QueueEntryStatus::Queued
    );

    // Once the canonical execution is runnable again the retained input
    // claims through its original path.
    let owner_latest = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
    seed_live_runnable_work_execution(runtime, &owner_latest).await;
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => {
            panic!("expected the retained followup to claim, got idle")
        }
        _ => panic!("expected the retained followup to claim, got a non-message poll"),
    };
    assert_eq!(scheduled.message.id, followup_id);
    assert!(scheduled.scheduler_decision.model_reentry);
}
