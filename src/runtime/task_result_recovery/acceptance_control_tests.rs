use super::*;
use crate::runtime::recovery_acceptance;
use serde_json::{json, Value};

fn control(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
    request: &str,
    hold: bool,
    inject: bool,
    fence: Value,
) {
    gated_control(runtime, task, request, hold, false, inject, fence);
}

fn gated_control(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
    request: &str,
    hold_scanner: bool,
    hold_task_result: bool,
    inject: bool,
    fence: Value,
) {
    let directory = recovery_acceptance::directory(runtime);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("control.json"),
        serde_json::to_vec(&json!({
            "protocol": 1,
            "request_id": request,
            "agent_id": task.agent_id,
            "task_id": task.id,
            "work_item_id": task.work_item_id,
            "hold_scanner": hold_scanner,
            "hold_task_result": hold_task_result,
            "inject_sixth": inject,
            "fence": fence,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn event(runtime: &RuntimeHandle, request: &str, name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            recovery_acceptance::directory(runtime).join(format!("{request}.{name}.json")),
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_original_gate_allows_real_scanner_claim_before_original_result() {
    assert_original_gate_scanner_claim(false).await;
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_triggered_exact_wait_preserves_original_delivery_fence() {
    assert_original_gate_scanner_claim(true).await;
}

async fn assert_original_gate_scanner_claim(original_wait: bool) {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) =
        completed_command_with_delivery(runtime, Some(&owner), "held-original", true).await;
    if original_wait {
        let WaitForRegistrationOutcome::TaskResultQueued {
            result_message_id,
            wait_condition_id,
            ..
        } = exact_wait(runtime, &owner.id, &task.id).await
        else {
            panic!("the original queued result must trigger its exact waiter");
        };
        assert_eq!(result_message_id, result.id);
        let owner = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
        seed_live_waiting_work_execution(runtime, &owner, &wait_condition_id).await;
    }
    gated_control(runtime, &task, "held", false, true, false, Value::Null);
    assert!(matches!(
        SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Idle
    ));
    let held = event(runtime, "held", "task-result-held");
    assert_eq!(held["source_message_id"], result.id);
    assert_eq!(held["queue_entry"]["status"], "queued");
    assert_eq!(held["recovery_attempts"], 0);
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    assert!(provider.requests.lock().await.is_empty());

    harness.advance(Duration::from_secs(30)).await;
    if original_wait {
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        assert!(recovery_sources(runtime).is_empty());
        gated_control(
            runtime,
            &task,
            "held",
            false,
            false,
            false,
            held["fence"].clone(),
        );
        let scheduled = Box::pin(claim_model(runtime)).await;
        assert_eq!(scheduled.message.id, result.id);
        process(runtime, scheduled).await;
        assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
        assert_eq!(provider.requests.lock().await.len(), 1);
        return;
    }
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let enqueued = event(runtime, "held", "scanner-enqueued");
    let scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(enqueued["source_message_id"], scheduled.message.id);
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&result.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    process(runtime, scheduled).await;
    let admitted = latest(runtime, &result.id);
    assert_eq!(admitted.recovery_attempts, 1);
    assert!(admitted.last_recovery_root_attempt_id.is_some());
    assert_eq!(provider.requests.lock().await.len(), 1);

    gated_control(
        runtime,
        &task,
        "held",
        false,
        false,
        false,
        held["fence"].clone(),
    );
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(
        event(runtime, "held", "task-result-released")["fence"],
        held["fence"]
    );
    if let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    {
        assert!(!scheduled.scheduler_decision.model_reentry);
        process(runtime, scheduled).await;
    }
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 1);
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_ack_matches_real_cli_version() {
    use clap::CommandFactory;

    let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, _) = completed_command_with_delivery(runtime, Some(&owner), "ack-build", true).await;
    gated_control(runtime, &task, "build", false, true, false, Value::Null);
    let _ = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap();
    assert_eq!(
        event(runtime, "build", "task-result-held")["build"],
        crate::cli::Cli::command().render_long_version().trim()
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_original_gate_cannot_park_a_foreign_result() {
    let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (first, _) = completed_command(runtime, &owner, "gate-target").await;
    let (_, other) =
        completed_command_with_delivery(runtime, Some(&owner), "not-gate-target", true).await;
    gated_control(runtime, &first, "foreign", false, true, false, Value::Null);
    let scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(scheduled.message.id, other.id);
    assert!(!recovery_acceptance::directory(runtime)
        .join("foreign.task-result-held.json")
        .exists());
    process(runtime, scheduled).await;
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_prearmed_scanner_ack_precedes_due_without_charging() {
    let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) =
        completed_command_with_delivery(runtime, Some(&owner), "early-scanner-ack", true).await;
    let before = latest(runtime, &result.id);
    assert!(before.next_recheck_at.unwrap() > runtime.now());
    control(runtime, &task, "early", true, false, Value::Null);

    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let held = event(runtime, "early", "scanner-held");
    assert_eq!(held["task_id"], task.id);
    assert_eq!(held["fence"]["result_message_id"], result.id);
    assert_eq!(
        held["fence"],
        event(runtime, "early", "target-observed")["fence"]
    );
    assert_eq!(held["recovery_attempts"], 0);
    assert!(held["last_recovery_root_attempt_id"].is_null());
    assert!(recovery_sources(runtime).is_empty());
    let after = latest(runtime, &result.id);
    assert_eq!(after.next_recheck_at, before.next_recheck_at);
    assert_eq!(after.recovery_epoch, before.recovery_epoch);
    assert_eq!(after.recovery_attempts, 0);
    assert!(after.last_recovery_root_attempt_id.is_none());
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&result.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_controls_real_first_source_order_without_blocking_exact_wait() {
    assert!(env!("HOLON_VERSION").contains("[recovery-acceptance-test]"));
    for scanner_first in [false, true] {
        let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
        let runtime = harness.runtime();
        let owner = ready_owner(runtime).await;
        let (task, result) = completed_command(runtime, &owner, "ordered-control").await;
        control(
            runtime,
            &task,
            "ordered",
            !scanner_first,
            false,
            Value::Null,
        );
        harness.advance(Duration::from_secs(30)).await;
        assert_eq!(
            runtime.emit_due_task_result_recovery().await.unwrap(),
            scanner_first
        );
        let observed = event(runtime, "ordered", "target-observed");
        assert_eq!(observed["fence"]["result_message_id"], result.id);
        assert_eq!(observed["task_id"], task.id);
        assert_eq!(observed["work_item_id"], owner.id);
        let first = event(
            runtime,
            "ordered",
            if scanner_first {
                "scanner-enqueued"
            } else {
                "scanner-held"
            },
        );
        assert_eq!(first["recovery_attempts"], 0);
        assert_eq!(recovery_sources(runtime).len(), usize::from(scanner_first));
        let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
            recovery_message_id,
            wait_condition_id,
            ..
        } = exact_wait(runtime, &owner.id, &task.id).await
        else {
            panic!("the exact wait must register even while the scanner is held");
        };
        let source = runtime
            .storage()
            .read_message_by_id(&recovery_message_id)
            .unwrap()
            .unwrap();
        if scanner_first {
            assert_eq!(first["source_message_id"], recovery_message_id);
            assert!(!source.source_refs.contains_key("wait_id"));
        } else {
            assert_eq!(source.source_refs.get("wait_id"), Some(&wait_condition_id));
            control(
                runtime,
                &task,
                "ordered",
                false,
                false,
                observed["fence"].clone(),
            );
        }
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        assert_eq!(recovery_sources(runtime).len(), 1);
        assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_sixth_source_uses_real_queue_and_reports_actual_budget_rejection() {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) = completed_command(runtime, &owner, "sixth-control").await;
    let paused = Box::pin(exhaust_real_command(&harness, &provider, &result)).await;
    control(runtime, &task, "sixth", false, false, Value::Null);
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let observed = event(runtime, "sixth", "target-observed");
    control(
        runtime,
        &task,
        "sixth",
        false,
        true,
        observed["fence"].clone(),
    );
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let enqueued = event(runtime, "sixth", "sixth-enqueued");
    assert_eq!(
        enqueued["source"]["origin"]["subsystem"],
        "task_result_recovery"
    );
    assert_eq!(enqueued["source"]["authority_class"], "runtime_instruction");
    assert_eq!(
        enqueued["source"]["source_refs"]["task_result_identity"],
        paused.result_identity
    );
    assert_eq!(
        enqueued["source"]["source_refs"]["task_result_message_id"],
        result.id
    );
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("the explicit source must enter normal canonical queue claim");
    };
    assert_eq!(enqueued["source_message_id"], scheduled.message.id);
    process(runtime, scheduled).await;
    let rejected = event(runtime, "sixth", "budget-rejected");
    assert_eq!(rejected["source_message_id"], enqueued["source_message_id"]);
    assert_eq!(rejected["rejection_stage"], "scheduler_canonical_claim");
    assert_eq!(rejected["reason"], "recovery_budget_exhausted");
    assert_eq!(rejected["recovery_attempts"], 5);
    assert_eq!(rejected["fence"], enqueued["fence"]);
    let after = latest(runtime, &result.id);
    assert_eq!(after.recovery_attempts, 5);
    assert_eq!(after.recovery_epoch, paused.recovery_epoch);
    assert_eq!(
        after.last_recovery_root_attempt_id,
        paused.last_recovery_root_attempt_id
    );
    assert_eq!(after.recovery_paused_at, paused.recovery_paused_at);
    assert_eq!(provider.requests.lock().await.len(), 5);
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(
        recovery_sources(runtime)
            .iter()
            .filter(|source| source.id == enqueued["source_message_id"])
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_stale_identity_cannot_hold_or_inject_another_result() {
    let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) = completed_command(runtime, &owner, "stale-control").await;
    control(runtime, &task, "stale", true, false, Value::Null);
    recovery_acceptance::service(runtime).await.unwrap();
    let mut fence = event(runtime, "stale", "target-observed")["fence"].clone();
    fence["result_identity"] = "foreign-result".into();
    control(runtime, &task, "stale", true, true, fence);
    harness.advance(Duration::from_secs(30)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(recovery_sources(runtime).len(), 1);
    assert!(!recovery_acceptance::directory(runtime)
        .join("stale.sixth-enqueued.json")
        .exists());
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
}

#[tokio::test(start_paused = true)]
async fn recovery_acceptance_prearmed_task_gate_cannot_rebind_its_observed_result() {
    let harness = LifecycleHarness::with_provider(Arc::new(FailureProvider::default()));
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (first_task, _) = completed_command(runtime, &owner, "first-bound").await;
    control(runtime, &first_task, "bound", true, false, Value::Null);
    recovery_acceptance::service(runtime).await.unwrap();
    let (second_task, _) = completed_command(runtime, &owner, "second-bound").await;
    control(runtime, &second_task, "bound", true, false, Value::Null);
    let (control, enqueued) = recovery_acceptance::service(runtime).await.unwrap();
    assert!(control.is_none());
    assert!(!enqueued);
    assert_eq!(
        event(runtime, "bound", "target-observed")["task_id"],
        first_task.id
    );
}
