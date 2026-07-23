use super::super::support::*;
use crate::types::{AdmissionContext, MessageDeliverySurface, MessageEnvelope, QueueEntryRecord};

#[tokio::test]
async fn legacy_work_queue_settlement_does_not_require_scheduler_partition() {
    let harness = LifecycleHarness::new();
    let message = harness
        .runtime()
        .enqueue(
            MessageEnvelope::new(
                "default",
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "work_queue".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "legacy work queue tick".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::RuntimeSystem,
                AdmissionContext::RuntimeOwned,
            ),
        )
        .await
        .unwrap();
    let mut processed = harness
        .snapshot()
        .queue_entries
        .into_iter()
        .find(|entry| entry.message_id == message.id)
        .unwrap();
    processed.status = QueueEntryStatus::Processed;
    processed.updated_at = Utc::now();

    assert!(harness
        .runtime()
        .commit_queue_settlement(processed.clone(), Vec::new(), true)
        .await
        .unwrap());
    assert_eq!(
        harness
            .snapshot()
            .queue_entries
            .into_iter()
            .find(|entry| entry.message_id == message.id)
            .unwrap(),
        processed
    );
}

#[tokio::test]
async fn queue_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-fault-contract".into(),
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        harness
            .runtime()
            .storage()
            .append_queue_entry(&queued)
            .unwrap();
        let before = harness.snapshot();
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .commit_queue_settlement(
                processed.clone(),
                vec![AuditEvent::legacy(
                    "queue_fault_contract",
                    serde_json::json!({}),
                )],
                true,
            )
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        assert!(harness
            .runtime()
            .commit_queue_settlement(
                processed.clone(),
                vec![AuditEvent::legacy(
                    "queue_fault_contract",
                    serde_json::json!({}),
                )],
                true,
            )
            .await
            .unwrap());
        let after = harness.snapshot();
        assert_eq!(after.queue_entries, vec![processed]);
        assert_eq!(after.audit_events.len(), before.audit_events.len() + 1);
    }
}

#[tokio::test]
async fn queue_post_commit_fault_keeps_terminal_settlement_after_restart() {
    for (fault, expected_effect) in POST_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        let now = harness.now();
        let queued = QueueEntryRecord {
            message_id: format!("message-post-commit-{expected_effect}"),
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        harness
            .runtime()
            .storage()
            .append_queue_entry(&queued)
            .unwrap();
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        harness.arm_fault(fault);

        assert!(harness
            .runtime()
            .commit_queue_settlement(
                processed.clone(),
                vec![AuditEvent::legacy(
                    "queue_post_commit_contract",
                    serde_json::json!({}),
                )],
                true,
            )
            .await
            .unwrap());

        harness.assert_post_commit_warning(expected_effect);
        assert_eq!(harness.snapshot().queue_entries, vec![processed.clone()]);
        let committed = harness.snapshot();

        harness.restart();

        assert_eq!(harness.snapshot(), committed);
        assert_eq!(harness.snapshot().queue_entries, vec![processed]);
    }
}

#[tokio::test]
async fn queue_replays_unprocessed_message_once_after_restart() {
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "replayed once",
    });
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let message = harness
        .runtime()
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "recover me".into(),
            },
        ))
        .await
        .unwrap();

    harness.restart();
    let runtime = harness.runtime().clone();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        200,
        |events| {
            events.iter().any(|event| {
                event.kind == "queue_entry_settled"
                    && event.data["message_id"] == message.id.as_str()
                    && event.data["status"] == "processed"
            })
        },
        "replayed queue settlement",
    )
    .await;
    runtime_task.abort();

    assert!(harness
        .snapshot()
        .briefs
        .iter()
        .any(|brief| brief.text.contains("replayed once")));
    assert_eq!(
        harness
            .snapshot()
            .queue_entries
            .iter()
            .find(|entry| entry.message_id == message.id)
            .unwrap()
            .status,
        QueueEntryStatus::Processed
    );
    assert_eq!(
        harness
            .snapshot()
            .audit_events
            .iter()
            .filter(|event| {
                event.kind == "queue_entry_settled"
                    && event.data["message_id"] == message.id.as_str()
            })
            .count(),
        1
    );

    let settled = harness.snapshot();
    let settled_briefs = settled.briefs.len();
    let settled_events = settled.audit_events.len();
    assert_eq!(provider.call_count().await, 1);
    harness.restart();
    let runtime = harness.runtime().clone();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        settled_events + 10,
        |events| {
            events.iter().skip(settled_events).any(|event| {
                event.kind == "scheduler_posture_decision"
                    && event.data["boundary"] == "run_loop_idle"
            })
        },
        "second restart idle checkpoint",
    )
    .await;
    runtime_task.abort();

    let restarted = harness.snapshot();
    assert_eq!(provider.call_count().await, 1);
    assert_eq!(restarted.briefs.len(), settled_briefs);
    assert_eq!(
        restarted
            .audit_events
            .iter()
            .filter(|event| {
                event.kind == "queue_entry_settled"
                    && event.data["message_id"] == message.id.as_str()
            })
            .count(),
        1
    );
}
