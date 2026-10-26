use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::runtime_db::task_result_settlement::{
    charge_recovery_admission_tx, TaskResultSettlementState,
};
use crate::runtime_db::TaskResultSettlementRecord;
use crate::types::{
    TaskKind, TaskRecord, TaskStatus, WaitConditionKind, WaitConditionRecord, WaitConditionStatus,
    WakeSource, WorkItemRecord,
};

fn pending_command_result(
    runtime: &RuntimeHandle,
    task_id: &str,
    now: DateTime<Utc>,
) -> TaskResultSettlementRecord {
    let mut signal = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "secret command output must not appear in diagnostics".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    signal.task_id = Some(task_id.into());
    let task = TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: Some(signal.id.clone()),
        work_item_id: None,
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": task_id, "rejoin_generation": 1, "parent_turn_id": "parent"
        })),
        recovery: None,
    };
    runtime.runtime_db().tasks().upsert(&task).unwrap();
    runtime.storage().append_message(&signal).unwrap();
    runtime
        .runtime_db()
        .task_result_settlements()
        .ensure_pending(&task, &signal, now)
        .unwrap()
        .unwrap()
}

fn exhaust(runtime: &RuntimeHandle, record: &TaskResultSettlementRecord) {
    let mut now = record.next_recheck_at.unwrap();
    for attempt in 1..=5 {
        runtime
            .runtime_db()
            .transaction(|tx| {
                assert!(charge_recovery_admission_tx(
                    tx,
                    &record.result_identity,
                    &format!("root-{attempt}"),
                    now,
                )?);
                Ok(())
            })
            .unwrap();
        now = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap()
            .next_recheck_at
            .unwrap_or(now);
    }
}

#[tokio::test(start_paused = true)]
async fn paused_scanner_reports_once_without_queueing_or_exposing_result_content() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let record = pending_command_result(
        &runtime,
        "paused-task",
        harness.now() - chrono::Duration::seconds(2000),
    );
    exhaust(&runtime, &record);
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let briefs = runtime.storage().read_recent_briefs(20).unwrap();
    assert_eq!(briefs.len(), 1);
    assert!(briefs[0].text.contains(&record.result_identity));
    assert!(briefs[0].text.contains("--rearm-task-result"));
    assert!(!briefs[0].text.contains("secret command output"));
    assert!(briefs[0].turn_id.is_none());
    assert!(outstanding_recovery_message_id(
        &runtime,
        "default",
        None,
        &recovery_owner_key(&record),
    )
    .unwrap()
    .is_none());
    let mut paused = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(paused.state, TaskResultSettlementState::PersistedPending);
    assert!(paused.recovery_diagnostic_at.is_some());
    // Simulate a crash after the Brief/event committed, before its report marker.
    paused.recovery_diagnostic_at = None;
    runtime
        .runtime_db()
        .connection()
        .unwrap()
        .execute(
            "UPDATE task_result_settlements SET recovery_diagnostic_at = NULL, payload_json = ?2
         WHERE result_identity = ?1",
            rusqlite::params![
                paused.result_identity,
                serde_json::to_string(&paused).unwrap()
            ],
        )
        .unwrap();
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(runtime.storage().read_recent_briefs(20).unwrap().len(), 1);
    let report =
        scheduler_recovery_report(runtime.storage(), runtime.runtime_db(), "default").unwrap();
    assert_eq!(report.pending_task_results.len(), 1);
    assert_eq!(report.pending_task_results[0].recovery_attempts, 5);
    assert!(report.pending_task_results[0].recovery_paused_at.is_some());
    assert!(report.pending_task_results[0].next_recheck_at.is_none());
}

#[tokio::test(start_paused = true)]
async fn paused_diagnostic_preserves_deferred_reason_with_legacy_budget_default() {
    for reason in [
        Some("provider_dispatch_rejected"),
        Some("recovery_budget_exhausted"),
        None,
    ] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let record = pending_command_result(
            &runtime,
            "paused-task",
            harness.now() - chrono::Duration::seconds(2000),
        );
        exhaust(&runtime, &record);
        let mut paused = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap();
        paused.deferred_reason = reason.map(str::to_owned);
        runtime
            .runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE task_result_settlements SET payload_json = ?2 WHERE result_identity = ?1",
                rusqlite::params![
                    paused.result_identity,
                    serde_json::to_string(&paused).unwrap()
                ],
            )
            .unwrap();

        runtime
            .publish_paused_task_result_diagnostics("default")
            .await
            .unwrap();
        let briefs = runtime.storage().read_recent_briefs(20).unwrap();
        assert_eq!(briefs.len(), 1);
        assert!(briefs[0].text.contains(&format!(
            "reason={},",
            reason.unwrap_or("recovery_budget_exhausted")
        )));
        assert!(!briefs[0].text.contains("secret command output"));
    }
}

#[tokio::test(start_paused = true)]
async fn paused_diagnostics_drain_beyond_scanner_batch_without_a_timer() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    for index in 0..=RESULT_RECHECK_LIMIT {
        let record = pending_command_result(
            runtime,
            &format!("paused-{index}"),
            harness.now() - chrono::Duration::seconds(2000),
        );
        exhaust(runtime, &record);
    }
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(
        runtime.storage().read_recent_briefs(20).unwrap().len(),
        RESULT_RECHECK_LIMIT + 1,
    );
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .unreported_paused("default", 1)
        .unwrap()
        .is_empty());
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .next_recheck_at("default")
        .unwrap()
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn paused_result_does_not_block_same_owner_new_results_or_charge_scanner_probes() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let now = harness.now() - chrono::Duration::seconds(2000);
    let paused = pending_command_result(&runtime, "paused-task", now);
    exhaust(&runtime, &paused);
    let fresh = pending_command_result(&runtime, "fresh-task", now);
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let fresh = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&fresh.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.recovery_attempts, 0);
    assert!(outstanding_recovery_message_id(
        &runtime,
        "default",
        None,
        &recovery_owner_key(&fresh),
    )
    .unwrap()
    .is_some());
}

#[tokio::test(start_paused = true)]
async fn scanner_defers_obligation_claimed_by_another_waiter_without_charging() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let record = pending_command_result(
        runtime,
        "claimed-task",
        harness.now() - chrono::Duration::seconds(2000),
    );
    let waiter = WorkItemRecord::new("default", "claim task result", WorkItemState::Open);
    runtime.storage().append_work_item(&waiter).unwrap();
    runtime
        .storage()
        .append_wait_condition(&WaitConditionRecord {
            id: "other-owner-wait".into(),
            agent_id: "default".into(),
            work_item_id: Some(waiter.id),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some(record.task_id.clone()),
            waiting_for: "task result".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: record.task_id.clone(),
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
        })
        .unwrap();

    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let deferred = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        deferred.deferred_reason.as_deref(),
        Some("claimed_by_other_waiter"),
    );
    assert_eq!(deferred.recovery_attempts, 0);
    assert_eq!(
        deferred.next_recheck_at,
        Some(harness.now() + chrono::Duration::seconds(RESULT_RECHECK_SECONDS)),
    );
    assert!(outstanding_recovery_message_id(
        runtime,
        "default",
        None,
        &recovery_owner_key(&record),
    )
    .unwrap()
    .is_none());
}

#[tokio::test(start_paused = true)]
async fn scanner_uses_obligation_classification_instead_of_queue_progress() {
    for (status, should_recover) in [
        (QueueEntryStatus::Processed, true),
        (QueueEntryStatus::Quarantined, false),
    ] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let record = pending_command_result(
            runtime,
            "queue-progress-task",
            harness.now() - chrono::Duration::seconds(2000),
        );
        runtime
            .runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: record.message_id.clone(),
                agent_id: "default".into(),
                priority: Priority::Next,
                status,
                created_at: record.created_at,
                updated_at: harness.now(),
            })
            .unwrap();

        assert_eq!(
            runtime.emit_due_task_result_recovery().await.unwrap(),
            should_recover,
        );
        let deferred = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(deferred.recovery_attempts, 0);
        if !should_recover {
            assert_eq!(
                deferred.deferred_reason.as_deref(),
                Some("invalid_or_stale_obligation"),
            );
        }
        assert_eq!(
            deferred.next_recheck_at,
            Some(harness.now() + chrono::Duration::seconds(RESULT_RECHECK_SECONDS)),
        );
    }
}
