use super::*;
use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
use crate::runtime::tests::support::{
    assert_injected_transition_fault, CountingProvider, LifecycleHarness, POST_COMMIT_FAULTS,
    PRE_COMMIT_FAULTS,
};
use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
use crate::types::{TaskKind, TaskRecord, TaskStatus};

fn pending_result(
    runtime: &RuntimeHandle,
    now: DateTime<Utc>,
    task_id: &str,
    owner: Option<&str>,
) -> MessageEnvelope {
    let mut result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "completed command".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    result.task_id = Some(task_id.into());
    result.work_item_id = owner.map(str::to_owned);
    let task = TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: Some(result.id.clone()),
        work_item_id: owner.map(str::to_owned),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": task_id,
            "rejoin_generation": 1,
            "parent_turn_id": "parent",
            "terminal_reentry": true
        })),
        recovery: None,
    };
    result.metadata = Some(serde_json::json!({
        "task_id": task.id,
        "task_kind": "command_task",
        "task_status": "completed",
        "task_detail": task.detail,
    }));
    runtime.runtime_db().tasks().upsert(&task).unwrap();
    runtime.storage().append_message(&result).unwrap();
    runtime
        .runtime_db()
        .task_result_settlements()
        .ensure_pending(&task, &result, now)
        .unwrap()
        .unwrap();
    result
}

async fn queued_recovery(runtime: &RuntimeHandle, now: DateTime<Utc>) -> (String, String) {
    queued_recovery_for_owner(runtime, now, None).await
}

async fn queued_recovery_for_owner(
    runtime: &RuntimeHandle,
    now: DateTime<Utc>,
    owner: Option<&str>,
) -> (String, String) {
    let result = pending_result(runtime, now, "stale-source-task", owner);
    let record = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&result.id)
        .unwrap()
        .unwrap();
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let recovery_id =
        outstanding_recovery_message_id(runtime, "default", owner, &recovery_owner_key(&record))
            .unwrap()
            .unwrap();
    (result.id, recovery_id)
}

pub(in crate::runtime) async fn settled_recovery(harness: &LifecycleHarness) -> (String, String) {
    let runtime = harness.runtime();
    let (result_id, recovery_id) =
        queued_recovery(runtime, harness.now() - chrono::Duration::seconds(60)).await;
    let result = runtime
        .storage()
        .read_message_by_id(&result_id)
        .unwrap()
        .unwrap();
    runtime.enqueue(result).await.unwrap();
    process_next(runtime).await;
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&result_id)
            .unwrap()
            .unwrap()
            .state,
        TaskResultSettlementState::Settled,
    );
    (result_id, recovery_id)
}

async fn process_next(runtime: &RuntimeHandle) {
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    else {
        return;
    };
    let message = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: message.id,
                agent_id: message.agent_id,
                priority: message.priority,
                status: QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn recovery_settled_before_claim_consumes_source_without_creating_turn() {
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "delivered",
    });
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let (result_id, recovery_id) =
        queued_recovery(runtime, harness.now() - chrono::Duration::seconds(60)).await;
    let result = runtime
        .storage()
        .read_message_by_id(&result_id)
        .unwrap()
        .unwrap();
    runtime.enqueue(result).await.unwrap();
    process_next(runtime).await;
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&result_id)
            .unwrap()
            .unwrap()
            .state,
        TaskResultSettlementState::Settled,
    );
    let turns = runtime.storage().read_recent_turns(20).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(*provider.calls.lock().await, 1);

    process_next(runtime).await;
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&recovery_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Processed,
    );
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap(), turns);
    assert_eq!(*provider.calls.lock().await, 1);
    assert!(runtime
        .storage()
        .read_recent_events(100)
        .unwrap()
        .iter()
        .any(|event| event.kind == "task_result_recovery_source_consumed"
            && event.data["message_id"] == recovery_id));
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_precommit_faults_replay_without_losing_source_or_creating_turn() {
    for fault in PRE_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        let (_, recovery_id) = settled_recovery(&harness).await;
        let before = harness.snapshot();
        harness.arm_fault(fault);
        let error = SchedulerDecisionExecutor::new(harness.runtime())
            .poll()
            .await
            .err()
            .expect("injected no-op transaction must roll back");
        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        harness.restart();
        process_next(harness.runtime()).await;
        assert_eq!(
            harness
                .runtime()
                .storage()
                .read_recent_turns(20)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            harness
                .runtime()
                .runtime_db()
                .queue_entries()
                .latest(&recovery_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Processed,
        );
    }
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_postcommit_faults_and_competing_polls_consume_once() {
    for (fault, _) in POST_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        let (_, recovery_id) = settled_recovery(&harness).await;
        harness.arm_fault(fault);
        let first = SchedulerDecisionExecutor::new(harness.runtime());
        let second = SchedulerDecisionExecutor::new(harness.runtime());
        let (a, b) = tokio::join!(Box::pin(first.poll()), Box::pin(second.poll()));
        assert!(!matches!(a.unwrap(), RunLoopPoll::Message(_)));
        assert!(!matches!(b.unwrap(), RunLoopPoll::Message(_)));
        harness.restart();
        process_next(harness.runtime()).await;
        assert_eq!(
            harness
                .runtime()
                .storage()
                .read_recent_turns(20)
                .unwrap()
                .len(),
            1
        );
        let events = harness.runtime().storage().read_recent_events(200).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.kind == "task_result_recovery_source_consumed"
                        && event.data["message_id"] == recovery_id
                })
                .count(),
            1,
        );
    }
}

fn planned_source_consumption(
    runtime: &RuntimeHandle,
    recovery_id: &str,
) -> crate::runtime_db::transitions::QueueTransitionCommand {
    use crate::runtime_db::transitions::{QueueMutation, QueueOperation, QueueTransitionCommand};
    let source = runtime
        .storage()
        .read_message_by_id(recovery_id)
        .unwrap()
        .unwrap();
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .settled_recovery_source(&source)
        .unwrap());
    let expected = runtime
        .runtime_db()
        .queue_entries()
        .latest(recovery_id)
        .unwrap()
        .unwrap();
    let mut record = expected.clone();
    record.status = QueueEntryStatus::Processed;
    record.updated_at = runtime.now();
    QueueTransitionCommand {
        agent_id: source.agent_id.clone(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::ConsumeSettledTaskResultRecovery {
            expected,
            record,
            source: Box::new(source),
        },
        scheduler_claim_work_item: None,
        agent_state: None,
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: vec![AuditEvent::legacy(
            "planned_source_consumption",
            serde_json::json!({}),
        )],
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    }
}

fn replace_terminal_task_fixture(runtime: &RuntimeHandle, task: &TaskRecord) {
    // The public repository correctly forbids terminal rewrites. Inject an
    // adversarial durable replacement only in this harness's temporary DB.
    runtime
        .runtime_db()
        .transaction(|tx| {
            assert_eq!(
                tx.execute(
                    "UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
                    rusqlite::params![serde_json::to_string(task)?, task.id],
                )?,
                1,
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_rechecks_new_owner_results_inside_queue_transaction() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, recovery_id) = settled_recovery(&harness).await;
    let command = planned_source_consumption(runtime, &recovery_id);
    // A new result appears after planning but before the queue CAS.
    let pending = pending_result(runtime, harness.now(), "new-owner-result", None);
    let before = harness.snapshot();
    assert!(
        !runtime
            .runtime_db()
            .transitions()
            .commit_queue(&command)
            .unwrap()
            .applied
    );
    harness.assert_unchanged(&before);
    process_next(runtime).await;
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&pending.id)
            .unwrap()
            .unwrap()
            .state,
        TaskResultSettlementState::Settled,
    );
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_requires_durable_source_identity_and_excludes_exact_waits() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, recovery_id) = settled_recovery(&harness).await;
    let source = runtime
        .storage()
        .read_message_by_id(&recovery_id)
        .unwrap()
        .unwrap();
    let settlements = runtime.runtime_db().task_result_settlements();
    assert!(settlements.settled_recovery_source(&source).unwrap());
    for key in [
        "task_result_identity",
        "task_result_parent_turn_id",
        "task_result_rejoin_generation",
        "task_result_message_id",
        "task_result_owner_key",
    ] {
        let mut altered = source.clone();
        altered.source_refs.insert(key.into(), "mismatched".into());
        assert!(
            !settlements.settled_recovery_source(&altered).unwrap(),
            "{key}"
        );
    }
    let mut exact = source.clone();
    exact
        .source_refs
        .insert("wait_id".into(), "independent-waiter".into());
    assert!(!settlements.settled_recovery_source(&exact).unwrap());
    let mut foreign = source.clone();
    foreign.work_item_id = Some("other-owner".into());
    assert!(!settlements.settled_recovery_source(&foreign).unwrap());
    let mut untrusted = source;
    untrusted.authority_class = AuthorityClass::ExternalEvidence;
    assert!(!settlements.settled_recovery_source(&untrusted).unwrap());
}

#[tokio::test(start_paused = true)]
async fn pending_recovery_retains_model_delivery_instead_of_system_only_consumption() {
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "recovered",
    });
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let (result_id, _) =
        queued_recovery(runtime, harness.now() - chrono::Duration::seconds(60)).await;
    process_next(runtime).await;
    assert_eq!(*provider.calls.lock().await, 1);
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap().len(), 1);
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&result_id)
            .unwrap()
            .unwrap()
            .state,
        TaskResultSettlementState::Settled,
    );
    assert!(!runtime
        .storage()
        .read_recent_events(100)
        .unwrap()
        .iter()
        .any(|event| event.kind == "task_result_recovery_source_consumed"));
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_does_not_complete_or_admit_its_work_item_owner() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work = runtime
        .create_work_item("owner remains open".into(), None, None, Vec::new())
        .await
        .unwrap();
    let (result_id, recovery_id) = queued_recovery_for_owner(
        runtime,
        harness.now() - chrono::Duration::seconds(60),
        Some(&work.id),
    )
    .await;
    let result = runtime
        .storage()
        .read_message_by_id(&result_id)
        .unwrap()
        .unwrap();
    runtime.enqueue(result).await.unwrap();
    process_next(runtime).await;
    let before = harness.snapshot();
    process_next(runtime).await;
    let after = harness.snapshot();
    assert_eq!(after.work_items, before.work_items);
    assert_eq!(after.execution_protocol, before.execution_protocol);
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap().len(), 1);
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&recovery_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Processed,
    );
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_retains_independent_wait_reconciliation_and_generation_fences() {
    use crate::types::{WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource};
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, recovery_id) = settled_recovery(&harness).await;
    let source = runtime
        .storage()
        .read_message_by_id(&recovery_id)
        .unwrap()
        .unwrap();
    let command = planned_source_consumption(runtime, &recovery_id);
    let mut wait = WaitConditionRecord {
        id: "independent-wait".into(),
        agent_id: "default".into(),
        work_item_id: Some("another-waiter-owner".into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: None,
        subject_ref: None,
        waiting_for: "stale-source-task".into(),
        wake_sources: vec![WakeSource::TaskResult {
            task_id: "stale-source-task".into(),
        }],
        continuation: None,
        created_at: harness.now(),
        updated_at: harness.now(),
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    runtime
        .runtime_db()
        .wait_conditions()
        .upsert(&wait)
        .unwrap();
    assert!(!runtime
        .runtime_db()
        .task_result_settlements()
        .settled_recovery_source(&source)
        .unwrap());
    let before = harness.snapshot();
    assert!(
        !runtime
            .runtime_db()
            .transitions()
            .commit_queue(&command)
            .unwrap()
            .applied
    );
    harness.assert_unchanged(&before);
    wait.status = WaitConditionStatus::Cancelled;
    wait.cancelled_at = Some(harness.now());
    runtime
        .runtime_db()
        .wait_conditions()
        .upsert(&wait)
        .unwrap();
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .settled_recovery_source(&source)
        .unwrap());

    // Replace the durable generation after planning, before the queue CAS.
    let mut task = runtime
        .storage()
        .latest_task_record("stale-source-task")
        .unwrap()
        .unwrap();
    task.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    replace_terminal_task_fixture(runtime, &task);
    assert!(!runtime
        .runtime_db()
        .task_result_settlements()
        .settled_recovery_source(&source)
        .unwrap());
    let before = harness.snapshot();
    assert!(
        !runtime
            .runtime_db()
            .transitions()
            .commit_queue(&command)
            .unwrap()
            .applied
    );
    harness.assert_unchanged(&before);
    process_next(runtime).await;
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap().len(), 2);
    assert!(!runtime
        .storage()
        .read_recent_events(100)
        .unwrap()
        .iter()
        .any(|event| event.kind == "task_result_recovery_source_consumed"));
}

#[tokio::test(start_paused = true)]
async fn settled_recovery_rechecks_durable_task_owner_before_queue_commit() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, recovery_id) = settled_recovery(&harness).await;
    let command = planned_source_consumption(runtime, &recovery_id);
    let mut task = runtime
        .storage()
        .latest_task_record("stale-source-task")
        .unwrap()
        .unwrap();
    task.work_item_id = Some("replacement-owner".into());
    replace_terminal_task_fixture(runtime, &task);
    let before = harness.snapshot();
    assert!(
        !runtime
            .runtime_db()
            .transitions()
            .commit_queue(&command)
            .unwrap()
            .applied
    );
    harness.assert_unchanged(&before);
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&recovery_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued,
    );
}

#[tokio::test(start_paused = true)]
async fn stopped_agent_retains_settled_recovery_source_without_any_consumption() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, recovery_id) = settled_recovery(&harness).await;
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::Stopped;
        guard.persist_state(&runtime.inner.storage).unwrap();
    }
    let before = harness.snapshot();
    assert!(matches!(
        SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Stopped(..),
    ));
    harness.assert_unchanged(&before);
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&recovery_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued,
    );
}
