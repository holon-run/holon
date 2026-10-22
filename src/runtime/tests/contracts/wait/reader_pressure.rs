use super::*;
use crate::runtime::{tasks::ReplyReconciliation, AgentMessageDeliveryService, RuntimeHandle};
use crate::types::{AgentMessageCallerContext, AgentMessagePrincipalKind, AgentMessageSendRequest};
use std::future::Future;
use std::sync::mpsc;
use std::time::Duration;

#[tokio::test]
async fn wait_for_futures_keep_preparation_off_the_callers_stack() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let preparation = runtime.prepare_wait_for_outcome(
        "default",
        None,
        WaitForWakeKind::System,
        None,
        "bounded preparation future".into(),
        None,
    );
    let registration = runtime.register_wait_for_outcome(
        "default",
        None,
        WaitForWakeKind::System,
        None,
        "bounded registration future".into(),
        None,
    );
    let identified_preparation = runtime.prepare_wait_for_outcome_with_id(
        "default",
        None,
        WaitForWakeKind::System,
        None,
        "bounded identified preparation future".into(),
        None,
        None,
    );
    // These futures are embedded in callers that also run provider callbacks.
    // Leave the default test-thread stack available for those nested calls.
    for (name, bytes) in [
        ("preparation", std::mem::size_of_val(&preparation)),
        ("registration", std::mem::size_of_val(&registration)),
        (
            "identified preparation",
            std::mem::size_of_val(&identified_preparation),
        ),
    ] {
        assert!(bytes <= 16 * 1024, "{name} future uses {bytes} stack bytes");
    }
}

async fn under_reader_pressure<T>(
    runtime: &RuntimeHandle,
    release_after: Duration,
    read: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let mut releases = Vec::new();
    let mut readers = Vec::new();
    // Occupy the runtime's four default reader slots without holding the writer.
    for _ in 0..4 {
        let db = runtime.runtime_db().clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        readers.push(std::thread::spawn(move || {
            db.with_read(|_| {
                let _ = started_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(5))?;
                Ok(())
            })
        }));
        releases.push(release_tx);
        started_rx.await.expect("reader acquired");
    }
    let release = tokio::spawn(async move {
        tokio::time::sleep(release_after).await;
        for release in releases {
            let _ = release.send(());
        }
    });
    let result = read.await;
    release.await.expect("release readers");
    for reader in readers {
        reader.join().expect("reader thread").expect("reader lease");
    }
    result
}

#[tokio::test(flavor = "current_thread")]
async fn task_read_facades_yield_until_reader_capacity_is_released() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item("reader pressure task".into(), None, None, Vec::new())
        .await
        .unwrap();
    let task = running_task("reader-pressure-task", &owner.id, harness.now());
    runtime
        .persist_task_transition(&task, "task_created")
        .await
        .unwrap();
    let release_after = Duration::from_millis(10);

    assert_eq!(
        under_reader_pressure(runtime, release_after, runtime.task_record(&task.id))
            .await
            .unwrap(),
        Some(task.clone())
    );
    assert_eq!(
        under_reader_pressure(runtime, release_after, runtime.latest_task_records())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        under_reader_pressure(
            runtime,
            release_after,
            runtime.latest_task_list_entries_for_agent("default", 1),
        )
        .await
        .unwrap()
        .len(),
        1
    );
    let output = under_reader_pressure(
        runtime,
        release_after,
        runtime.task_output(&task.id, false, 0),
    )
    .await
    .unwrap();
    assert_eq!(output.retrieval_status, TaskOutputRetrievalStatus::NotReady);
}

#[tokio::test(flavor = "current_thread")]
async fn reply_reconciliation_yields_and_reader_timeout_preserves_pending_task() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item("reader pressure reply".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut prepared = AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: "default".into(),
            content: MessageBody::Text {
                text: "reader pressure reply".into(),
            },
            client_idempotency_key: "reader-pressure-reply".into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: Some(Priority::Normal),
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: "agent:reader-pressure-peer".into(),
            caller_agent_id: Some("reader-pressure-peer".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
    )
    .unwrap();
    prepared.message.created_at = harness.now();
    prepared.message.work_item_id = Some(owner.id.clone());
    let receipt = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let mut task = running_task("reader-pressure-reply", &owner.id, harness.now());
    task.kind = TaskKind::AgentMessageWait;
    task.recovery = Some(TaskRecoverySpec::AgentMessageWait {
        summary: "reader pressure reply".into(),
        message: "await peer".into(),
        target_agent_id: "reader-pressure-peer".into(),
        authority_class: AuthorityClass::RuntimeInstruction,
        delivery_id: None,
        after_delivery_rowid: Some(0),
    });
    task.detail = Some(serde_json::json!({
        "rejoin_obligation_id": task.id,
        "rejoin_generation": 1,
        "parent_turn_id": "reader-pressure-parent",
    }));
    runtime
        .persist_task_transition(&task, "task_created")
        .await
        .unwrap();
    // Equal timestamps leave task_id order: settle the reply before this receipt fence.
    let mut pending_receipt = task.clone();
    pending_receipt.id = format!("{}-z-receipt", task.id);
    pending_receipt.detail = None;
    let Some(TaskRecoverySpec::AgentMessageWait {
        after_delivery_rowid,
        ..
    }) = pending_receipt.recovery.as_mut()
    else {
        unreachable!();
    };
    *after_delivery_rowid = None;
    runtime
        .persist_task_transition(&pending_receipt, "task_created")
        .await
        .unwrap();

    let before = harness.snapshot();
    let error = under_reader_pressure(
        runtime,
        Duration::from_millis(500),
        runtime.reconcile_agent_message_reply(&prepared.message),
    )
    .await
    .err()
    .expect("reader acquisition must time out");
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
        .is_some());
    harness.assert_unchanged(&before);

    let outcome = under_reader_pressure(
        runtime,
        Duration::from_millis(10),
        runtime.reconcile_agent_message_reply(&prepared.message),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, ReplyReconciliation::WaitingForReceipt));
    let completed = runtime.task_record(&task.id).await.unwrap().unwrap();
    assert_eq!(completed.status, TaskStatus::Completed);
    let detail = completed.detail.unwrap();
    assert_eq!(detail["message_id"], prepared.message.id);
    assert_eq!(detail["response_delivery_id"], receipt.delivery_id);
    assert_eq!(
        runtime
            .task_record(&pending_receipt.id)
            .await
            .unwrap()
            .unwrap(),
        pending_receipt
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wait_preparation_yields_and_reader_timeout_preserves_durable_state() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item("reader pressure wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let task = running_task("reader-pressure-wait", &owner.id, harness.now());
    runtime
        .persist_task_transition(&task, "task_created")
        .await
        .unwrap();
    let before = harness.snapshot();
    for wake in [WaitForWakeKind::System, WaitForWakeKind::External] {
        under_reader_pressure(
            runtime,
            Duration::from_millis(10),
            runtime.prepare_wait_for_outcome(
                "default",
                Some(owner.id.clone()),
                wake,
                None,
                "reader capacity will return".into(),
                None,
            ),
        )
        .await
        .unwrap();
        harness.assert_unchanged(&before);
    }
    let error = under_reader_pressure(
        runtime,
        Duration::from_millis(500),
        runtime.prepare_wait_for_outcome(
            "default",
            Some(owner.id.clone()),
            WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "do not settle on reader timeout".into(),
            None,
        ),
    )
    .await
    .err()
    .expect("reader acquisition must time out");
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
        .is_some());
    harness.assert_unchanged(&before);

    let result = late_task_result_message(&task.id, &owner.id);
    let terminal = terminal_task_with_result(&task, &result, harness.now());
    persist_unqueued_terminal_result(&harness, &terminal, &result).await;
    let before = harness.snapshot();
    under_reader_pressure(
        runtime,
        Duration::from_millis(10),
        runtime.prepare_wait_for_outcome(
            "default",
            Some(owner.id.clone()),
            WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "prepare the exact terminal result after capacity returns".into(),
            None,
        ),
    )
    .await
    .unwrap();
    harness.assert_unchanged(&before);
}
