use super::*;
use crate::runtime::{PrepareWaitForOutcome, WaitForRegistrationOutcome, WaitForWakeKind};
use crate::types::AgentMessageDeliveryState;

async fn dropped_reply(harness: &LifecycleHarness) -> (WorkItemRecord, TaskRecord, String) {
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "reply owner awaiting approval".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let (task, reply_id) = reply_result(runtime, &owner).await;
    let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap();
    assert!(matches!(poll, scheduler_executor::RunLoopPoll::Idle));
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&reply_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Dropped
    );
    let delivery = runtime
        .runtime_db()
        .agent_message_deliveries()
        .latest_for_message(&reply_id)
        .unwrap()
        .unwrap();
    assert_eq!(delivery.state, AgentMessageDeliveryState::Failed);
    assert!(!delivery.retryable);
    (owner, task, reply_id)
}

#[tokio::test(start_paused = true)]
async fn late_dropped_agent_reply_uses_new_exact_recovery_once() {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let (owner, task, reply_id) = dropped_reply(&harness).await;
    let original = runtime
        .storage()
        .read_message_by_id(&reply_id)
        .unwrap()
        .unwrap();
    let queue = runtime
        .runtime_db()
        .queue_entries()
        .latest(&reply_id)
        .unwrap();
    let delivery = runtime
        .runtime_db()
        .agent_message_deliveries()
        .latest_for_message(&reply_id)
        .unwrap();
    let waiter = runtime
        .create_work_item(
            "exact reply consumer".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(waiter.id.clone()),
            WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "receive the original reply".into(),
            None,
        )
        .await
        .unwrap();
    let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        recovery_message_id,
        wait_condition_id,
        ..
    } = outcome
    else {
        panic!("terminal reply requires a distinct recovery identity: {outcome:?}");
    };
    assert_ne!(recovery_message_id, reply_id);
    let repeated = wait_for_reply(runtime, Some(&waiter.id), &task).await;
    assert!(
        matches!(repeated, WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        recovery_message_id: repeated_id, wait_condition_id: repeated_wait, ..
    } if repeated_id == recovery_message_id && repeated_wait == wait_condition_id)
    );
    let before_other = harness.snapshot();
    assert!(matches!(
        wait_for_reply(runtime, Some(&owner.id), &task).await,
        WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter { .. }
    ));
    harness.assert_unchanged(&before_other);
    seed_waiting_work_execution(
        runtime.storage(),
        &runtime.latest_work_item(&waiter.id).await.unwrap().unwrap(),
        &wait_condition_id,
    );
    let scheduled = claim_model(runtime).await;
    assert_eq!(scheduled.message.id, recovery_message_id);
    process(runtime, scheduled).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    let record = latest(runtime, result_id);
    assert_eq!(record.state, TaskResultSettlementState::Settled);
    assert_eq!(
        record.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
    assert_eq!(record.recovery_attempts, 1);
    assert_eq!(
        runtime.storage().read_message_by_id(&reply_id).unwrap(),
        Some(original)
    );
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&reply_id)
            .unwrap(),
        queue
    );
    assert_eq!(
        runtime
            .runtime_db()
            .agent_message_deliveries()
            .latest_for_message(&reply_id)
            .unwrap(),
        delivery
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].matches("repeated-reply-evidence").count(), 1);
    drop(requests);
    let consumed = harness.snapshot();
    assert!(matches!(
        wait_for_reply(runtime, Some(&waiter.id), &task).await,
        WaitForRegistrationOutcome::TaskResultAlreadyConsumed { .. }
    ));
    harness.assert_unchanged(&consumed);
}

async fn wait_for_reply(
    runtime: &RuntimeHandle,
    owner: Option<&str>,
    task: &TaskRecord,
) -> WaitForRegistrationOutcome {
    runtime
        .register_wait_for_outcome(
            "default",
            owner.map(str::to_owned),
            WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "receive exact reply".into(),
            None,
        )
        .await
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn late_dropped_reply_exact_wait_survives_restart_for_work_and_agent_scope() {
    for agent_scope in [false, true] {
        let provider = Arc::new(FailureProvider::default());
        let mut harness = LifecycleHarness::with_provider(provider.clone());
        let (_, task, reply_id) = dropped_reply(&harness).await;
        let waiter = if agent_scope {
            None
        } else {
            Some(
                harness
                    .runtime()
                    .create_work_item(
                        "restart exact consumer".into(),
                        Some(WorkItemPlanStatus::Ready),
                        None,
                        Vec::new(),
                    )
                    .await
                    .unwrap(),
            )
        };
        let outcome = wait_for_reply(
            harness.runtime(),
            waiter.as_ref().map(|item| item.id.as_str()),
            &task,
        )
        .await;
        let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
            recovery_message_id,
            wait_condition_id,
            ..
        } = outcome
        else {
            panic!("expected exact recovery: {outcome:?}");
        };
        if let Some(waiter) = &waiter {
            seed_waiting_work_execution(
                harness.runtime().storage(),
                &harness
                    .runtime()
                    .latest_work_item(&waiter.id)
                    .await
                    .unwrap()
                    .unwrap(),
                &wait_condition_id,
            );
        }
        let original_queue = harness
            .runtime()
            .runtime_db()
            .queue_entries()
            .latest(&reply_id)
            .unwrap();
        harness.restart();
        let scheduled = Box::pin(claim_model(harness.runtime())).await;
        assert_eq!(scheduled.message.id, recovery_message_id);
        process(harness.runtime(), scheduled).await;
        let result = latest(
            harness.runtime(),
            task.parent_message_id.as_deref().unwrap(),
        );
        assert_eq!(
            result.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(result.recovery_attempts, 1);
        assert_eq!(
            harness
                .runtime()
                .runtime_db()
                .queue_entries()
                .latest(&reply_id)
                .unwrap(),
            original_queue
        );
        assert_eq!(provider.requests.lock().await.len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn terminal_reply_without_canonical_drop_evidence_is_unavailable() {
    for status in [
        QueueEntryStatus::Dropped,
        QueueEntryStatus::Aborted,
        QueueEntryStatus::Quarantined,
    ] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let owner = runtime
            .create_work_item(
                "unproven reply".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let (task, reply_id) = reply_result(runtime, &owner).await;
        let reply = runtime
            .storage()
            .read_message_by_id(&reply_id)
            .unwrap()
            .unwrap();
        remove_source(runtime, &reply, status).await;
        let before = harness.snapshot();
        assert!(matches!(
            wait_for_reply(runtime, Some(&owner.id), &task).await,
            WaitForRegistrationOutcome::TaskResultUnavailable { .. }
        ));
        harness.assert_unchanged(&before);
    }
}

fn damage_source(runtime: &RuntimeHandle, reply_id: &str, damage: &str) {
    runtime
        .runtime_db()
        .transaction(|tx| {
            let sql = match damage {
                "delivery" => {
                    "UPDATE agent_message_deliveries
                SET payload_json = json_set(payload_json, '$.retryable', json('true'))
                WHERE message_id = ?1"
                }
                "queue" => {
                    "UPDATE queue_entries
                SET payload_json = json_set(payload_json, '$.updated_at', '2026-10-29T00:00:00Z')
                WHERE message_id = ?1"
                }
                "message" => {
                    "UPDATE messages
                SET payload_json = json_set(payload_json, '$.metadata', json('{}'))
                WHERE message_id = ?1"
                }
                "missing_message" => "DELETE FROM messages WHERE message_id = ?1",
                "missing_queue" => "DELETE FROM queue_entries WHERE message_id = ?1",
                "rejection" => {
                    "DELETE FROM audit_events
                WHERE kind = 'scheduler_authority_input_rejected'
                  AND json_extract(data_json, '$.data.message_id') = ?1"
                }
                _ => unreachable!(),
            };
            assert!(tx.execute(sql, [reply_id])? > 0);
            Ok(())
        })
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn dropped_reply_missing_or_revoked_source_evidence_is_unavailable() {
    for damage in [
        "delivery",
        "message",
        "missing_message",
        "missing_queue",
        "rejection",
    ] {
        let harness = LifecycleHarness::new();
        let (_, task, reply_id) = dropped_reply(&harness).await;
        damage_source(harness.runtime(), &reply_id, damage);
        let before = harness.snapshot();
        assert!(
            matches!(
                wait_for_reply(harness.runtime(), None, &task).await,
                WaitForRegistrationOutcome::TaskResultUnavailable { .. }
            ),
            "{damage}"
        );
        harness.assert_unchanged(&before);
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_reply_prepared_source_changes_are_atomic_conflicts() {
    for damage in [
        "delivery",
        "queue",
        "message",
        "missing_message",
        "missing_queue",
        "rejection",
    ] {
        let harness = LifecycleHarness::new();
        let (_, task, reply_id) = dropped_reply(&harness).await;
        let runtime = harness.runtime();
        let prepared = runtime
            .prepare_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::TaskResult,
                Some(task.id),
                "prepare exact reply".into(),
                None,
            )
            .await
            .unwrap();
        let PrepareWaitForOutcome::Prepared(prepared) = prepared else {
            panic!("expected prepared dropped reply");
        };
        damage_source(runtime, &reply_id, damage);
        let before = harness.snapshot();
        let error = runtime
            .runtime_db()
            .transitions()
            .commit_wait_with_execution_protocol_and_task_expectation(
                &prepared.command,
                &prepared.execution_protocol,
                prepared.expected_task.as_ref(),
            )
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
                .unwrap()
                .domain(),
            "task_result_obligation",
            "{damage}"
        );
        harness.assert_unchanged(&before);
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_reply_wait_rolls_back_each_precommit_fault() {
    for fault in crate::runtime::tests::support::PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let (_, task, _) = dropped_reply(&harness).await;
        let before = harness.snapshot();
        harness.arm_fault(fault);
        let error = harness
            .runtime()
            .register_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::TaskResult,
                Some(task.id.clone()),
                "fault exact reply".into(),
                None,
            )
            .await
            .unwrap_err();
        crate::runtime::tests::support::assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);
        assert!(matches!(
            wait_for_reply(harness.runtime(), None, &task).await,
            WaitForRegistrationOutcome::TaskResultRecoveryQueued { .. }
        ));
    }
}
