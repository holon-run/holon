use super::*;
use crate::runtime_db::{
    task_result_settlement::{TaskResultObligation, TaskResultSettlementState},
    TaskResultSettlementDisposition,
};

mod exact_repair;

async fn pending_result(
    harness: &LifecycleHarness,
    status: QueueEntryStatus,
) -> (WorkItemRecord, TaskRecord, MessageEnvelope) {
    let owner = harness
        .runtime()
        .create_work_item("pending result repair".into(), None, None, Vec::new())
        .await
        .unwrap();
    let running = running_task("task-obligation-repair", &owner.id, harness.now());
    let message = late_task_result_message(&running.id, &owner.id);
    let terminal = terminal_task_with_result(&running, &message, harness.now());
    persist_unqueued_terminal_result(harness, &terminal, &message).await;
    persist_native_queue_fixture(
        harness.runtime(),
        QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status,
            created_at: message.created_at,
            updated_at: harness.now(),
        },
    )
    .await;
    (owner, terminal, message)
}

#[tokio::test]
async fn queue_progress_repairs_one_canonical_wake_without_replaying_result() {
    for status in [
        QueueEntryStatus::Dequeued,
        QueueEntryStatus::Interjected,
        QueueEntryStatus::Processed,
    ] {
        for delivery in ["silent", "final"] {
            let harness = LifecycleHarness::new();
            let (owner, task, original) = pending_result(&harness, status.clone()).await;
            let original_queue = harness
                .runtime()
                .inner
                .runtime_db
                .queue_entries()
                .latest(&original.id)
                .unwrap();
            let input = serde_json::json!({
                "wake": "task_result",
                "resource": task.id,
                "work_item_id": owner.id,
                "reason": "recover this pending obligation",
                "delivery": delivery,
            });
            let before = harness.snapshot();
            let result = crate::tool::tools::wait_for::execute(
                harness.runtime(),
                "default",
                &AuthorityClass::OperatorInstruction,
                &input,
                &crate::tool::spec::ToolExecutionContext::default(),
            )
            .await
            .unwrap();
            harness.assert_unchanged(&before);
            let result = if delivery == "final" {
                assert_eq!(
                    result.envelope.result.as_ref().unwrap()["disposition"],
                    "awaiting_final_report"
                );
                crate::tool::tools::wait_for::prepare_settlement(
                    harness.runtime(),
                    "default",
                    &AuthorityClass::OperatorInstruction,
                    crate::tool::tools::wait_for::parse_wait_for_args(&input).unwrap(),
                )
                .await
                .unwrap()
            } else {
                result
            };
            harness.assert_unchanged(&before);
            let prepared = result.prepared_wait_for.as_ref().unwrap();
            let mut mutation = harness.runtime().begin_mutation().await.unwrap();
            let commit = mutation
                .commit_wait_transition(
                    crate::ids::audit_event_id(),
                    prepared.command.clone(),
                    prepared.execution_protocol.clone(),
                    prepared.expected_task.clone(),
                )
                .await
                .unwrap();
            drop(mutation);
            harness.runtime().apply_transition_commit(commit).await;
            let data = result.envelope.result.as_ref().unwrap();
            assert_eq!(data["continuation"], "yield_and_reenter");
            assert_eq!(data["result_message_id"], original.id);
            let recovery_id = data["recovery_message_id"].as_str().unwrap();
            assert_ne!(recovery_id, original.id);
            assert_eq!(
                harness
                    .runtime()
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&original.id)
                    .unwrap(),
                original_queue,
            );
            let stored = harness
                .runtime()
                .storage()
                .read_message_by_id(&original.id)
                .unwrap()
                .unwrap();
            assert_eq!(stored.turn_id, original.turn_id);
            let ledger = harness
                .runtime()
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(&original.id)
                .unwrap()
                .unwrap();
            assert_eq!(ledger.state, TaskResultSettlementState::PersistedPending);
            assert!(ledger.disposition.is_none());
            assert_eq!(ledger.work_item_id.as_deref(), Some(owner.id.as_str()));
            let again = harness
                .runtime()
                .register_wait_for_outcome(
                    "default",
                    Some(owner.id.clone()),
                    WaitForWakeKind::TaskResult,
                    Some(task.id.clone()),
                    "reuse canonical recovery".into(),
                    None,
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    &again,
                    WaitForRegistrationOutcome::TaskResultRecoveryQueued { recovery_message_id, .. }
                        if recovery_message_id == recovery_id
                ),
                "repeated exact repair must reuse its wake: {again:?}"
            );
            assert_eq!(
                harness
                    .runtime()
                    .storage()
                    .latest_task_record(&task.id)
                    .unwrap()
                    .unwrap()
                    .work_item_id,
                Some(owner.id),
            );
            let recoveries = harness
                .runtime()
                .inner
                .runtime_db
                .messages()
                .all(Some("default"))
                .unwrap()
                .into_iter()
                .filter(|message| {
                    matches!(&message.origin,
                    MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
                })
                .count();
            assert_eq!(recoveries, 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn wait_snapshot_keeps_submillisecond_precision_and_rejects_stale_payloads() {
    for concurrent_mutation in [false, true] {
        let harness = LifecycleHarness::new();
        let (owner, task, _) = pending_result(&harness, QueueEntryStatus::Queued).await;
        harness
            .advance(std::time::Duration::from_nanos(123_456))
            .await;
        harness
            .runtime()
            .register_wait_for(
                "default",
                Some(owner.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#3572".into()),
                "independent business blocker".into(),
                None,
            )
            .await
            .unwrap();
        let repository = harness.runtime().inner.runtime_db.wait_conditions();
        let mut wait = repository
            .snapshot_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|wait| wait.work_item_id.as_deref() == Some(owner.id.as_str()))
            .unwrap();
        assert_ne!(wait.updated_at.timestamp_subsec_nanos() % 1_000_000, 0);
        let raw_wait = wait.clone();
        let canonical_wait = repository
            .latest_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == wait.id)
            .unwrap();
        let input = serde_json::json!({
            "wake": "task_result",
            "resource": task.id,
            "work_item_id": owner.id,
            "reason": "late exact result",
            "delivery": "silent",
        });
        let result = crate::tool::tools::wait_for::prepare_settlement(
            harness.runtime(),
            "default",
            &AuthorityClass::OperatorInstruction,
            crate::tool::tools::wait_for::parse_wait_for_args(&input).unwrap(),
        )
        .await
        .unwrap();
        let prepared = result.prepared_wait_for.as_ref().unwrap();
        assert_eq!(
            prepared
                .command
                .task_result_admission
                .as_ref()
                .unwrap()
                .obligation
                .wait_witness
                .live_waits,
            vec![canonical_wait],
        );
        assert_eq!(
            repository
                .snapshot_for_agent("default")
                .unwrap()
                .into_iter()
                .find(|candidate| candidate.id == raw_wait.id)
                .unwrap(),
            raw_wait,
        );
        if concurrent_mutation {
            wait.updated_at += chrono::Duration::nanoseconds(1);
            wait.status = WaitConditionStatus::Cancelled;
            wait.cancelled_at = Some(wait.updated_at);
            repository.upsert(&wait).unwrap();
        }
        let before = harness.snapshot();
        let commit = harness
            .runtime()
            .inner
            .runtime_db
            .transitions()
            .commit_wait_with_execution_protocol_and_task_expectation(
                &prepared.command,
                &prepared.execution_protocol,
                prepared.expected_task.as_ref(),
            );
        if concurrent_mutation {
            commit.expect_err("concurrent wait cancellation must invalidate the prepared snapshot");
            harness.assert_unchanged(&before);
        } else {
            assert!(commit.is_ok(), "{commit:?}");
        }
    }
}

#[tokio::test]
async fn true_settlement_discloses_its_actual_disposition() {
    let harness = LifecycleHarness::new();
    let (owner, task, original) = pending_result(&harness, QueueEntryStatus::Processed).await;
    harness
        .runtime()
        .inner
        .runtime_db
        .task_result_settlements()
        .settle_owner_unavailable(
            &original.id,
            TaskResultSettlementDisposition::OwnerClosed,
            harness.now(),
        )
        .unwrap();
    let before = harness.snapshot();
    let outcome = harness
        .runtime()
        .register_wait_for_outcome(
            "default",
            Some(owner.id),
            WaitForWakeKind::TaskResult,
            Some(task.id),
            "observe actual settlement".into(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
            settlement_disposition: TaskResultSettlementDisposition::OwnerClosed,
            ..
        }
    ));
    harness.assert_unchanged(&before);
}

#[tokio::test]
async fn classifier_does_not_infer_delivery_from_queue_progress() {
    let harness = LifecycleHarness::new();
    let (owner, _, original) = pending_result(&harness, QueueEntryStatus::Processed).await;
    let repository = harness.runtime().inner.runtime_db.task_result_settlements();
    let ledger = repository
        .latest_for_message(&original.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        repository
            .classify_obligation(&ledger, None, Some(&owner.id))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
}
