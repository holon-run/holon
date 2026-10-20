use super::*;
use crate::runtime::tests::support::{seed_bound_work_item, LifecycleHarness};
use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
use crate::runtime_db::{TaskResultSettlementDisposition, TaskResultSettlementRecord};
use crate::types::{
    LiveInputBatchRecord, LiveInputSafePoint, TaskKind, TaskRecord, TaskStatus, TurnRecord,
    TurnTerminalKind, TurnTerminalSummary, WorkItemState,
};

fn lifecycle_result(
    runtime: &RuntimeHandle,
    task_id: &str,
    now: DateTime<Utc>,
) -> TaskResultSettlementRecord {
    result_for_owner(
        runtime,
        task_id,
        None,
        TaskKind::ChildAgentTask,
        TaskStatus::Cancelled,
        now,
    )
}

fn result_for_owner(
    runtime: &RuntimeHandle,
    task_id: &str,
    work_item_id: Option<&str>,
    kind: TaskKind,
    status: TaskStatus,
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
            text: "anchored task result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    signal.task_id = Some(task_id.into());
    signal.work_item_id = work_item_id.map(str::to_owned);
    let task = TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind,
        status,
        created_at: now,
        updated_at: now,
        parent_message_id: Some(signal.id.clone()),
        work_item_id: work_item_id.map(str::to_owned),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": task_id,
            "rejoin_generation": 1,
            "parent_turn_id": "turn-anchor"
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

fn anchor_turn(runtime: &RuntimeHandle, turn_id: &str, terminal: bool, now: DateTime<Utc>) {
    let mut turn = TurnRecord::new("default", turn_id, 1);
    if terminal {
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: now,
            duration_ms: 10,
        });
    }
    runtime.runtime_db().turn_records().upsert(&turn).unwrap();
}

fn bind_open_attempt_to_anchor(
    runtime: &RuntimeHandle,
    turn_id: &str,
    activation_id: &str,
    now: DateTime<Utc>,
) {
    let attempt = serde_json::json!({
        "attempt_id": activation_id,
        "agent_id": "default",
        "source_message_id": null,
        "source": {
            "identity": {"kind": "queue_message", "message_id": "message-anchor-source"},
            "generation": 1
        },
        "binding": {"kind": "agent_lifecycle", "agent_id": "default"},
        "provenance": {
            "origin": "system",
            "trust": "runtime_instruction",
            "priority": "next"
        },
        "admitted_fences": {
            "source_revision": 1,
            "agent_control_revision": 1,
            "host_registry_revision": 1
        },
        "state": "open",
        "admitted_at": now.to_rfc3339()
    });
    runtime
        .runtime_db()
        .transaction(|tx| {
            tx.execute(
                "INSERT INTO execution_protocol_attempts (
                   agent_id, attempt_id, lifecycle_state, source_identity_json,
                   source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
                 ) VALUES (?1, ?2, 'open', ?3, 1, NULL, NULL, ?4)",
                rusqlite::params![
                    "default",
                    activation_id,
                    serde_json::to_string(&attempt["source"]["identity"]).unwrap(),
                    serde_json::to_string(&attempt).unwrap()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let mut turn = TurnRecord::new("default", turn_id, 1);
    turn.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: now,
        duration_ms: 10,
    });
    turn.live_input_batches.push(LiveInputBatchRecord {
        activation_id: activation_id.to_owned(),
        safe_point: LiveInputSafePoint::new(activation_id, 1),
        cutoff: now,
        max_items: 8,
        max_bytes: 4096,
        rendered_bytes: 0,
        result_identities: Vec::new(),
        ordinary_message_ids: Vec::new(),
        items: Vec::new(),
    });
    runtime.runtime_db().turn_records().upsert(&turn).unwrap();
}

fn latest_record(runtime: &RuntimeHandle, message_id: &str) -> TaskResultSettlementRecord {
    runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(message_id)
        .unwrap()
        .unwrap()
}

#[test]
fn anchor_liveness_tracks_turn_and_bound_attempt_state() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let now = harness.now();
    let settlements = runtime.runtime_db().task_result_settlements();

    // A missing anchor record is not proof of death: turnless result fences
    // synthesize turn ids that never persist one.
    assert!(settlements
        .anchor_turn_live("default", "turn-missing")
        .unwrap());

    anchor_turn(&runtime, "turn-open", false, now);
    assert!(settlements
        .anchor_turn_live("default", "turn-open")
        .unwrap());

    anchor_turn(&runtime, "turn-terminal", true, now);
    assert!(!settlements
        .anchor_turn_live("default", "turn-terminal")
        .unwrap());

    // A bound open attempt keeps even a terminal anchor live.
    bind_open_attempt_to_anchor(&runtime, "turn-terminal-bound", "attempt-anchor-open", now);
    assert!(settlements
        .anchor_turn_live("default", "turn-terminal-bound")
        .unwrap());
}

#[tokio::test(start_paused = true)]
async fn lifecycle_zombie_settlement_converges_on_terminal_anchor() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let record = lifecycle_result(
        &runtime,
        "zombie-child-task",
        harness.now() - chrono::Duration::seconds(2000),
    );
    anchor_turn(&runtime, "turn-anchor", true, harness.now());
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .exact_waiter_recovery_continuation(&record, harness.now())
        .unwrap()
        .is_none());

    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());

    let settled = latest_record(&runtime, &record.message_id);
    assert_eq!(settled.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settled.disposition,
        Some(TaskResultSettlementDisposition::InvalidOrStale)
    );
    assert!(settled.settled_at.is_some());
    assert!(settled.next_recheck_at.is_none());
    assert!(outstanding_recovery_message_id(
        &runtime,
        "default",
        None,
        &recovery_owner_key(&record)
    )
    .unwrap()
    .is_none());

    // The loop terminates: no due record remains on the next scan.
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .due_deferred("default", harness.now(), 8)
        .unwrap()
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn nonterminal_anchor_turn_keeps_settlement_recoverable() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let record = lifecycle_result(
        &runtime,
        "live-anchor-task",
        harness.now() - chrono::Duration::seconds(2000),
    );
    anchor_turn(&runtime, "turn-anchor", false, harness.now());

    assert!(runtime.emit_due_task_result_recovery().await.unwrap());

    let deferred = latest_record(&runtime, &record.message_id);
    assert_eq!(deferred.state, TaskResultSettlementState::PersistedPending);
    assert!(deferred.disposition.is_none());
    assert!(deferred.next_recheck_at.is_some());
    assert!(outstanding_recovery_message_id(
        &runtime,
        "default",
        None,
        &recovery_owner_key(&record)
    )
    .unwrap()
    .is_some());
}

#[tokio::test(start_paused = true)]
async fn open_attempt_bound_to_terminal_anchor_keeps_settlement_recoverable() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let record = lifecycle_result(
        &runtime,
        "replaying-anchor-task",
        harness.now() - chrono::Duration::seconds(2000),
    );
    bind_open_attempt_to_anchor(
        &runtime,
        "turn-anchor",
        "attempt-anchor-open",
        harness.now(),
    );

    assert!(runtime.emit_due_task_result_recovery().await.unwrap());

    let deferred = latest_record(&runtime, &record.message_id);
    assert_eq!(deferred.state, TaskResultSettlementState::PersistedPending);
    assert!(deferred.disposition.is_none());
    assert!(deferred.next_recheck_at.is_some());
}

#[tokio::test(start_paused = true)]
async fn work_item_owner_closure_keeps_owner_disposition_path() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Completed, None, None).await;
    let record = result_for_owner(
        &runtime,
        "closed-owner-task",
        Some(&work_item_id),
        TaskKind::ChildAgentTask,
        TaskStatus::Cancelled,
        harness.now() - chrono::Duration::seconds(2000),
    );
    anchor_turn(&runtime, "turn-anchor", true, harness.now());

    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());

    let settled = latest_record(&runtime, &record.message_id);
    assert_eq!(settled.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settled.disposition,
        Some(TaskResultSettlementDisposition::OwnerClosed)
    );
    assert!(settled.next_recheck_at.is_none());
}

#[tokio::test(start_paused = true)]
async fn reply_anchored_lifecycle_settlement_defers_to_reply_channel() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    // Backdate creation so the settlement is already due for the scan.
    let now = harness.now() - chrono::Duration::seconds(2000);
    let reply = MessageEnvelope::new(
        "default",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "agent_message".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "peer reply".into(),
        },
    );
    runtime.storage().append_message(&reply).unwrap();

    let mut signal = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "reply-wait-task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "reply result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    signal.task_id = Some("reply-wait-task".into());
    signal.metadata = Some(serde_json::json!({
        "task_id": "reply-wait-task",
        "task_kind": "agent_message_wait",
        "task_status": "completed",
        "task_detail": {
            "message_id": reply.id,
            "reply_content_source": "original_message"
        }
    }));
    let task = TaskRecord {
        id: "reply-wait-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: Some(signal.id.clone()),
        work_item_id: None,
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": "reply-wait-task",
            "rejoin_generation": 1,
            "parent_turn_id": "turn-anchor"
        })),
        recovery: None,
    };
    runtime.runtime_db().tasks().upsert(&task).unwrap();
    runtime.storage().append_message(&signal).unwrap();
    let record = runtime
        .runtime_db()
        .task_result_settlements()
        .ensure_pending(&task, &signal, now)
        .unwrap()
        .unwrap();
    anchor_turn(&runtime, "turn-anchor", true, harness.now());

    // The reply channel still owns first delivery: the terminal-anchor
    // convergence must not terminate this obligation.
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let deferred = latest_record(&runtime, &record.message_id);
    assert_eq!(deferred.state, TaskResultSettlementState::PersistedPending);
    assert!(deferred.disposition.is_none());
    assert_eq!(
        deferred.deferred_reason.as_deref(),
        Some("awaiting_original_agent_reply_admission")
    );
    assert!(deferred.next_recheck_at.is_some());
}
