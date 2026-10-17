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
    harness
        .runtime()
        .storage()
        .append_queue_entry(&QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status,
            created_at: message.created_at,
            updated_at: harness.now(),
        })
        .unwrap();
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
            let commit = harness
                .runtime()
                .inner
                .runtime_db
                .transitions()
                .commit_wait_with_execution_protocol_and_task_expectation(
                    &prepared.command,
                    &prepared.execution_protocol,
                    prepared.expected_task.as_ref(),
                )
                .unwrap();
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
