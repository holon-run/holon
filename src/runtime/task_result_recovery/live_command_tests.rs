use super::*;
use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
use crate::runtime::waiting::{WaitForRegistrationOutcome, WaitForWakeKind};
use crate::runtime_db::task_result_settlement::TaskResultObligation;
use crate::types::CommandTaskSpec;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::Notify;

#[cfg(feature = "recovery-acceptance")]
#[path = "acceptance_control_tests.rs"]
mod acceptance_control_tests;

async fn ready_owner(runtime: &RuntimeHandle) -> WorkItemRecord {
    runtime
        .create_work_item(
            "real command recovery owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn real_command_lifecycle_repeated_daemon_startup_retains_only_exact_promises() {
    for scanner_first in [true, false] {
        for revoke in [
            None,
            Some("source_identity"),
            Some("wait_ref"),
            Some("source_origin"),
        ] {
            Box::pin(assert_first_admission_for_owner(
                scanner_first,
                FirstRecovery::RepeatedDaemonStartupBeforeAdmission,
                revoke,
                true,
            ))
            .await;
        }
    }
}

async fn completed_command(
    runtime: &RuntimeHandle,
    owner: &WorkItemRecord,
    evidence: &str,
) -> (TaskRecord, MessageEnvelope) {
    completed_command_for_owner(runtime, Some(owner), evidence).await
}

async fn completed_command_for_owner(
    runtime: &RuntimeHandle,
    owner: Option<&WorkItemRecord>,
    evidence: &str,
) -> (TaskRecord, MessageEnvelope) {
    completed_command_with_delivery(runtime, owner, evidence, false).await
}

async fn completed_command_with_delivery(
    runtime: &RuntimeHandle,
    owner: Option<&WorkItemRecord>,
    evidence: &str,
    keep_original_delivery: bool,
) -> (TaskRecord, MessageEnvelope) {
    if let Some(owner) = owner {
        runtime.pick_work_item(owner.id.clone()).await.unwrap();
    }
    let created = runtime
        .schedule_command_task(
            "real recovery command".into(),
            CommandTaskSpec {
                cmd: format!("printf '{evidence}'"),
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 0,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::RuntimeInstruction,
        )
        .await
        .unwrap();
    assert_eq!(
        created.work_item_id.as_deref(),
        owner.map(|owner| owner.id.as_str())
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let task = loop {
        let task = runtime.task_record(&created.id).await.unwrap().unwrap();
        if task.status == TaskStatus::Completed {
            break task;
        }
        assert!(
            !matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled),
            "real command failed: {task:?}"
        );
        assert!(std::time::Instant::now() < deadline, "{task:?}");
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    let result = runtime
        .storage()
        .read_message_by_id(task.parent_message_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let record = latest(runtime, &result.id);
    assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
    assert_eq!(record.recovery_attempts, 0);
    assert_eq!(
        record.parent_turn_id,
        task.rejoin_fence().unwrap().parent_turn_id
    );
    assert_eq!(record.task_id, created.id);
    if !keep_original_delivery {
        // Lose only the original queue delivery. No turn has admitted its content.
        remove_source(runtime, &result, QueueEntryStatus::Processed).await;
    }
    (task, result)
}

async fn exact_wait(
    runtime: &RuntimeHandle,
    owner: &str,
    task: &str,
) -> WaitForRegistrationOutcome {
    exact_wait_for_owner(runtime, Some(owner), task).await
}

async fn exact_wait_for_owner(
    runtime: &RuntimeHandle,
    owner: Option<&str>,
    task: &str,
) -> WaitForRegistrationOutcome {
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            owner.map(str::to_string),
            WaitForWakeKind::TaskResult,
            Some(task.into()),
            "recover the real command result".into(),
            None,
        )
        .await
        .unwrap();
    if let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        wait_condition_id, ..
    } = &outcome
    {
        // Registration is outside a provider turn; initialize the execution
        // wait normally persisted by that turn's WaitFor settlement.
        if let Some(owner) = owner {
            let owner = runtime.latest_work_item(owner).await.unwrap().unwrap();
            seed_waiting_work_execution(runtime.storage(), &owner, wait_condition_id);
        }
    }
    outcome
}

fn recovery_sources(runtime: &RuntimeHandle) -> Vec<MessageEnvelope> {
    runtime
        .runtime_db()
        .messages()
        .all(Some("default"))
        .unwrap()
        .into_iter()
        .filter(|message| {
            matches!(&message.origin, MessageOrigin::System { subsystem }
                if subsystem == "task_result_recovery")
        })
        .collect()
}

#[derive(Clone, Copy)]
enum FirstRecovery {
    Success,
    ProviderFailure,
    RestartBeforeAdmission,
    DaemonStartupBeforeAdmission,
    RepeatedDaemonStartupBeforeAdmission,
}

async fn revoke_promise(
    runtime: &RuntimeHandle,
    owner_id: Option<&str>,
    wait_id: &str,
    source: &MessageEnvelope,
    revoke: &str,
) {
    match revoke {
        "replacement" => {
            runtime
                .register_wait_for_outcome(
                    "default",
                    owner_id.map(str::to_string),
                    WaitForWakeKind::OperatorInput,
                    None,
                    "replace scanner-first promise".into(),
                    None,
                )
                .await
                .unwrap();
        }
        "stop" => runtime
            .control(crate::types::ControlAction::Stop)
            .await
            .unwrap(),
        "owner_closed" => {
            runtime
                .complete_work_item(owner_id.expect("WorkItem owner").to_string(), Vec::new())
                .await
                .unwrap();
        }
        "cancelled" | "expired" => {
            let mut wait = runtime
                .runtime_db()
                .wait_conditions()
                .snapshot_for_agent("default")
                .unwrap()
                .into_iter()
                .find(|wait| wait.id == wait_id)
                .unwrap();
            if revoke == "cancelled" {
                wait.cancelled_at = Some(runtime.now());
            } else {
                wait.expires_at = Some(runtime.now() - chrono::Duration::seconds(1));
            }
            // Terminal waits are immutable through the public repository.
            // Fault-inject a revoked snapshot to exercise replay fail-closed.
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE wait_conditions SET payload_json = ?2, expires_at = ?3
                         WHERE wait_condition_id = ?1",
                        rusqlite::params![
                            wait.id,
                            serde_json::to_string(&wait)?,
                            wait.expires_at.map(|expires| expires.to_rfc3339()),
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        "source_identity" | "wait_ref" | "source_origin" => {
            let mut forged = source.clone();
            if revoke == "source_origin" {
                forged.origin = MessageOrigin::System {
                    subsystem: "ordinary_followup".into(),
                };
            } else {
                let key = if revoke == "source_identity" {
                    "task_result_identity"
                } else {
                    "wait_id"
                };
                forged.source_refs.insert(key.into(), "wrong".into());
            }
            runtime.runtime_db().messages().upsert(&forged).unwrap();
        }
        _ => panic!("unknown promise revocation: {revoke}"),
    }
}

async fn assert_first_admission_order(
    scanner_first: bool,
    recovery: FirstRecovery,
    revoke: Option<&str>,
) {
    Box::pin(assert_first_admission_for_owner(
        scanner_first,
        recovery,
        revoke,
        false,
    ))
    .await;
}

async fn assert_first_admission_for_owner(
    scanner_first: bool,
    recovery: FirstRecovery,
    revoke: Option<&str>,
    lifecycle: bool,
) {
    let provider = Arc::new(FailureProvider::default());
    if matches!(recovery, FirstRecovery::ProviderFailure) {
        provider.failures.store(1, Ordering::SeqCst);
    }
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = if lifecycle {
        None
    } else {
        Some(ready_owner(runtime).await)
    };
    let owner_id = owner.as_ref().map(|owner| owner.id.as_str());
    let (task, result) =
        completed_command_for_owner(runtime, owner.as_ref(), "first-admission-evidence").await;
    harness.advance(Duration::from_secs(30)).await;
    let scanner_source = if scanner_first {
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        Some(recovery_sources(runtime).remove(0).id)
    } else {
        None
    };
    let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        recovery_message_id,
        wait_condition_id,
        ..
    } = exact_wait_for_owner(runtime, owner_id, &task.id).await
    else {
        panic!("the exact waiter must join the recovery source");
    };
    if let Some(scanner_source) = scanner_source {
        assert_eq!(recovery_message_id, scanner_source);
    }
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(recovery_sources(runtime).len(), 1);
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    let source = runtime
        .storage()
        .read_message_by_id(&recovery_message_id)
        .unwrap()
        .unwrap();
    // Persisted messages are append-only, so a source the scanner queued first
    // carries the wait ref only when this registration created it. The durable
    // join is the wait condition below, which must name this source either way.
    if !scanner_first {
        assert_eq!(source.source_refs.get("wait_id"), Some(&wait_condition_id));
    }
    let waits = runtime
        .runtime_db()
        .wait_conditions()
        .snapshot_for_agent("default")
        .unwrap();
    assert!(waits.iter().any(|wait| {
        wait.id == wait_condition_id
            && wait.work_item_id.as_deref() == owner_id
            && wait.trigger_message_id() == Some(recovery_message_id.as_str())
    }));
    let mut scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(scheduled.message.id, recovery_message_id);
    assert_eq!(scheduled.message.source_refs, source.source_refs);
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    let restarts = match recovery {
        FirstRecovery::RepeatedDaemonStartupBeforeAdmission => 2,
        FirstRecovery::RestartBeforeAdmission | FirstRecovery::DaemonStartupBeforeAdmission => 1,
        _ => 0,
    };
    for restart in 0..restarts {
        let runtime = harness.runtime();
        let revoke = revoke.filter(|_| restart + 1 == restarts);
        let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = scheduled
            .dispatch_plan
            .execution_admission_provenance
            .as_ref()
        else {
            panic!("the initial exact claim must be canonical");
        };
        let prior_activation_id = activation_id.clone();
        Box::pin(
            runtime.begin_interactive_turn_with_provenance(
                Some(&source),
                None,
                None,
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .clone(),
            ),
        )
        .await
        .unwrap();
        let daemon_startup = matches!(
            recovery,
            FirstRecovery::DaemonStartupBeforeAdmission
                | FirstRecovery::RepeatedDaemonStartupBeforeAdmission
        );
        if daemon_startup {
            if let Some(revoke) = revoke {
                revoke_promise(runtime, owner_id, &wait_condition_id, &source, revoke).await;
            }
            let report = runtime
                .runtime_db()
                .recover_interrupted_runtime_state_at_startup()
                .unwrap();
            assert_eq!(report.interrupted_execution_attempts, 1);
        }
        harness.restart();
        let runtime = harness.runtime();
        if daemon_startup {
            if let Some(revoke) = revoke {
                assert_eq!(
                    runtime
                        .runtime_db()
                        .queue_entries()
                        .latest(&source.id)
                        .unwrap()
                        .unwrap()
                        .status,
                    QueueEntryStatus::Quarantined,
                    "{revoke}: startup must reject an unproven exact source"
                );
                assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
                assert!(provider.requests.lock().await.is_empty());
                return;
            }
            assert_eq!(
                runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(&source.id)
                    .unwrap()
                    .unwrap()
                    .status,
                QueueEntryStatus::Interrupted,
                "daemon startup must retain the exact promise before runtime bootstrap"
            );
        }
        assert_eq!(
            Box::pin(runtime.recover_scheduler_bootstrap_claims())
                .await
                .unwrap(),
            usize::from(!daemon_startup)
        );
        let replay_wait = runtime
            .runtime_db()
            .task_result_settlements()
            .exact_waiter_recovery_replay_wait(&source, runtime.now())
            .unwrap()
            .expect("scanner-first restart retains its exact promise");
        assert_eq!(replay_wait.id, wait_condition_id);
        if let Some(revoke) = revoke {
            revoke_promise(runtime, owner_id, &wait_condition_id, &source, revoke).await;
            assert!(
                runtime
                    .runtime_db()
                    .task_result_settlements()
                    .exact_waiter_recovery_replay_wait(&source, runtime.now())
                    .unwrap()
                    .is_none(),
                "{revoke}: revoked scanner-first replay promise"
            );
            assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
            assert!(provider.requests.lock().await.is_empty());
            return;
        }
        scheduled = Box::pin(claim_model(runtime)).await;
        assert_eq!(scheduled.message.id, source.id);
        for (key, value) in &source.source_refs {
            assert_eq!(scheduled.message.source_refs.get(key), Some(value));
        }
        let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = scheduled
            .dispatch_plan
            .execution_admission_provenance
            .as_ref()
        else {
            panic!("the replayed exact claim must be canonical");
        };
        let execution = runtime
            .runtime_db()
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .unwrap();
        let predecessor = &execution.attempts[&prior_activation_id];
        let replay = &execution.attempts[activation_id];
        assert_eq!(
            predecessor.state,
            crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
        );
        assert_ne!(activation_id, &prior_activation_id);
        assert_eq!(
            replay.recovery_of_attempt_id.as_deref(),
            Some(prior_activation_id.as_str())
        );
        assert_eq!(replay.binding, predecessor.binding);
        assert_eq!(replay.source, predecessor.source);
        assert_eq!(
            replay.admitted_fences.source_revision,
            predecessor.admitted_fences.source_revision
        );
    }
    let runtime = harness.runtime();
    process(runtime, scheduled).await;
    let expected_attempts = if matches!(recovery, FirstRecovery::ProviderFailure) {
        discard_provider_handoffs(runtime).await;
        let failed = latest(runtime, &result.id);
        assert_eq!(failed.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(failed.recovery_attempts, 1);
        harness.advance(Duration::from_secs(180)).await;
        let promise = runtime
            .runtime_db()
            .task_result_settlements()
            .exact_waiter_recovery_continuation(&failed, harness.now())
            .unwrap()
            .expect("scanner-first failure retains its exact promise");
        assert_eq!(promise.id, wait_condition_id);
        if let Some(revoke) = revoke {
            revoke_promise(runtime, owner_id, &wait_condition_id, &source, revoke).await;
            assert!(
                runtime
                    .runtime_db()
                    .task_result_settlements()
                    .exact_waiter_recovery_continuation(
                        &latest(runtime, &result.id),
                        harness.now(),
                    )
                    .unwrap()
                    .is_none(),
                "{revoke}: revoked scanner-first continuation promise"
            );
            assert_eq!(latest(runtime, &result.id).recovery_attempts, 1);
            assert_eq!(provider.requests.lock().await.len(), 1);
            return;
        }
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        let successor = recovery_sources(runtime)
            .into_iter()
            .find(|source| source.id != recovery_message_id)
            .expect("the scanner queues a fresh exact successor");
        let retry_wait = runtime
            .runtime_db()
            .task_result_settlements()
            .exact_waiter_recovery_wait(&successor)
            .unwrap()
            .expect("the successor retains an exact wait, not an ordinary owner wake");
        assert_ne!(retry_wait.id, wait_condition_id);
        assert_eq!(retry_wait.work_item_id.as_deref(), owner_id);
        let retry = Box::pin(claim_model(runtime)).await;
        assert_eq!(retry.message.id, successor.id);
        process(runtime, retry).await;
        2
    } else {
        1
    };
    let delivered = latest(runtime, &result.id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(
        delivered.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(delivered.recovery_attempts, expected_attempts);
    assert_eq!(delivered.work_item_id.as_deref(), owner_id);
    assert_eq!(runtime.task_record(&task.id).await.unwrap().unwrap(), task);
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), expected_attempts as usize);
    assert!(requests
        .iter()
        .all(|request| request.contains("first-admission-evidence")));
}

#[tokio::test(start_paused = true)]
async fn real_command_first_exact_wait_then_scanner_has_one_admission() {
    Box::pin(assert_first_admission_order(
        false,
        FirstRecovery::Success,
        None,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn real_command_first_scanner_then_exact_wait_reuses_one_admission() {
    Box::pin(assert_first_admission_order(
        true,
        FirstRecovery::Success,
        None,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn real_command_scanner_first_provider_failure_retains_exact_promise() {
    Box::pin(assert_first_admission_order(
        true,
        FirstRecovery::ProviderFailure,
        None,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn real_command_scanner_first_claim_restart_retains_exact_promise() {
    Box::pin(assert_first_admission_order(
        true,
        FirstRecovery::RestartBeforeAdmission,
        None,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn real_command_daemon_startup_retains_exact_promise_in_both_orders() {
    for scanner_first in [true, false] {
        Box::pin(assert_first_admission_order(
            scanner_first,
            FirstRecovery::DaemonStartupBeforeAdmission,
            None,
        ))
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_daemon_startup_rejects_unproven_exact_sources() {
    for revoke in ["source_identity", "wait_ref", "source_origin"] {
        Box::pin(assert_first_admission_order(
            true,
            FirstRecovery::DaemonStartupBeforeAdmission,
            Some(revoke),
        ))
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_lifecycle_exact_promise_survives_admission_failure_and_restart() {
    for scanner_first in [true, false] {
        for recovery in [
            FirstRecovery::Success,
            FirstRecovery::ProviderFailure,
            FirstRecovery::RestartBeforeAdmission,
            FirstRecovery::DaemonStartupBeforeAdmission,
        ] {
            Box::pin(assert_first_admission_for_owner(
                scanner_first,
                recovery,
                None,
                true,
            ))
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_lifecycle_daemon_startup_rejects_unproven_exact_sources() {
    for revoke in ["source_identity", "wait_ref", "source_origin"] {
        Box::pin(assert_first_admission_for_owner(
            true,
            FirstRecovery::DaemonStartupBeforeAdmission,
            Some(revoke),
            true,
        ))
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_lifecycle_resolved_promise_keeps_revocation_fences() {
    for recovery in [
        FirstRecovery::ProviderFailure,
        FirstRecovery::RestartBeforeAdmission,
    ] {
        for revoke in [
            "replacement",
            "stop",
            "cancelled",
            "expired",
            "source_identity",
            "wait_ref",
        ] {
            Box::pin(assert_first_admission_for_owner(
                true,
                recovery,
                Some(revoke),
                true,
            ))
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_scanner_first_resolved_promise_keeps_revocation_fences() {
    for recovery in [
        FirstRecovery::ProviderFailure,
        FirstRecovery::RestartBeforeAdmission,
    ] {
        for revoke in [
            "replacement",
            "stop",
            "owner_closed",
            "cancelled",
            "expired",
            "source_identity",
            "wait_ref",
        ] {
            Box::pin(assert_first_admission_order(true, recovery, Some(revoke))).await;
        }
    }
}

#[derive(Default)]
struct GatedProvider {
    entered: Notify,
    release: Notify,
    requests: AtomicUsize,
}

#[async_trait::async_trait]
impl AgentProvider for GatedProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        crate::provider::StubProvider::new("delivered")
            .complete_turn(request)
            .await
    }
}

#[derive(Default)]
struct FifthRootBarrierProvider {
    failure: FailureProvider,
    entered: Notify,
    requests: AtomicUsize,
}

#[async_trait::async_trait]
impl AgentProvider for FifthRootBarrierProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 4 {
            self.entered.notify_one();
            return std::future::pending().await;
        }
        self.failure.complete_turn(request).await
    }
}

#[tokio::test(start_paused = true)]
async fn real_command_fifth_charged_root_survives_daemon_startup_without_new_budget() {
    let provider = Arc::new(FifthRootBarrierProvider::default());
    provider.failure.failures.store(4, Ordering::SeqCst);
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    // Initial snapshots use wall time, while startup replay uses the test clock.
    // Start just before wall time; the five retries advance beyond it for replay.
    let offset = Utc::now()
        .signed_duration_since(harness.now())
        .to_std()
        .unwrap_or_default();
    harness
        .advance(offset.saturating_sub(Duration::from_secs(60)))
        .await;
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (_, result) = completed_command(runtime, &owner, "fifth-root-startup").await;
    let mut roots = BTreeSet::new();
    for attempt in 1..=4 {
        harness
            .advance(Duration::from_secs(if attempt == 1 { 30 } else { 600 }))
            .await;
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        let scheduled = Box::pin(claim_model(runtime)).await;
        process(runtime, scheduled).await;
        let record = latest(runtime, &result.id);
        assert_recovery_snapshot(runtime, &record);
        assert_eq!(record.recovery_attempts, attempt);
        assert!(roots.insert(record.last_recovery_root_attempt_id.unwrap()));
        discard_provider_handoffs(runtime).await;
    }
    harness.advance(Duration::from_secs(600)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let scheduled = Box::pin(claim_model(runtime)).await;
    let source = scheduled.message.clone();
    assert!(source.source_refs.contains_key("task_result_owner_key"));
    assert!(!source.source_refs.contains_key("wait_id"));
    let mut processing = process(runtime, scheduled);
    tokio::select! {
        _ = &mut processing => panic!("fifth root must reach the provider barrier"),
        _ = provider.entered.notified() => {}
    }
    let before = latest(runtime, &result.id);
    assert_recovery_snapshot(runtime, &before);
    assert_eq!(before.recovery_attempts, 5);
    assert!(before.recovery_paused_at.is_some());
    assert!(before.next_recheck_at.is_none());
    assert!(roots.insert(before.last_recovery_root_attempt_id.clone().unwrap()));
    assert_eq!(roots.len(), 5);
    assert_eq!(provider.requests.load(Ordering::SeqCst), 5);
    // Dropping the in-flight provider future simulates the process ending at
    // this exact durable boundary, then runs the real daemon startup repair.
    drop(processing);
    let report = runtime
        .runtime_db()
        .recover_interrupted_runtime_state_at_startup()
        .unwrap();
    assert_eq!(report.interrupted_execution_attempts, 1);
    harness.restart();
    let runtime = harness.runtime();
    Box::pin(runtime.recover_scheduler_bootstrap_claims())
        .await
        .unwrap();
    let interrupted = latest(runtime, &result.id);
    assert_eq!(interrupted.state, TaskResultSettlementState::CallerAdmitted);
    let interrupted_execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let interrupted_attempt =
        &interrupted_execution.attempts[before.activation_id.as_deref().unwrap()];
    assert_eq!(
        interrupted_attempt.state,
        crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
    );
    let interrupted_turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&source.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interrupted,
        "{interrupted_attempt:?}\n{interrupted_turn:?}"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .interrupted_admitted_result_anchor("default", &result.id, runtime.now())
            .unwrap(),
        Some(source.id.clone()),
        "daemon startup must retain the interrupted canonical result snapshot: \
         {interrupted:?}\n{interrupted_attempt:?}\n{interrupted_turn:?}"
    );
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .recovery_root_is_charged(
            &latest(runtime, &result.id),
            before.last_recovery_root_attempt_id.as_deref().unwrap(),
        )
        .unwrap());
    let scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(scheduled.message.id, source.id);
    assert_eq!(
        runtime.storage().read_message_by_id(&source.id).unwrap(),
        Some(source.clone())
    );
    let mut replay_refs = source.source_refs.clone();
    replay_refs.insert("replay_source_turn_id".into(), interrupted_turn.turn_id);
    replay_refs.insert(
        "replay_reason".into(),
        "interrupted_queue_claim_reentry".into(),
    );
    assert_eq!(scheduled.message.source_refs, replay_refs);
    process(runtime, scheduled).await;
    let delivered = latest(runtime, &result.id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.recovery_attempts, 5);
    assert_eq!(delivered.recovery_epoch, before.recovery_epoch);
    assert_eq!(
        delivered.last_recovery_root_attempt_id,
        before.last_recovery_root_attempt_id
    );
    assert_eq!(delivered.recovery_paused_at, before.recovery_paused_at);
    assert_ne!(delivered.activation_id, before.activation_id);
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.attempts[delivered.activation_id.as_deref().unwrap()].recovery_of_attempt_id,
        before.activation_id
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 6);
}

#[tokio::test(start_paused = true)]
async fn real_command_live_provider_owner_excludes_scanner_and_second_exact_admission() {
    let provider = Arc::new(GatedProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) = completed_command(runtime, &owner, "live-owner-evidence").await;
    assert!(matches!(
        exact_wait(runtime, &owner.id, &task.id).await,
        WaitForRegistrationOutcome::TaskResultRecoveryQueued { .. }
    ));
    let scheduled = Box::pin(claim_model(runtime)).await;
    tokio::join!(process(runtime, scheduled), async {
        provider.entered.notified().await;
        let admitted = latest(runtime, &result.id);
        assert_eq!(admitted.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(admitted.recovery_attempts, 1);
        assert_eq!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .classify_obligation(&admitted, None, Some(&owner.id))
                .unwrap(),
            TaskResultObligation::InFlight {
                activation_id: admitted.activation_id.clone().unwrap(),
            }
        );
        let execution = runtime
            .runtime_db()
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .unwrap();
        assert_eq!(
            Some(execution.open_attempt().unwrap().attempt_id.as_str()),
            admitted.activation_id.as_deref()
        );
        harness.advance(Duration::from_secs(600)).await;
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        assert!(matches!(
            exact_wait(runtime, &owner.id, &task.id).await,
            WaitForRegistrationOutcome::TaskResultAlreadyAdmitted { activation_id, .. }
                if Some(activation_id.as_str()) == admitted.activation_id.as_deref()
        ));
        assert_eq!(recovery_sources(runtime).len(), 1);
        assert_eq!(latest(runtime, &result.id).recovery_attempts, 1);
        assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
        provider.release.notify_one();
    });
    let delivered = latest(runtime, &result.id);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.recovery_attempts, 1);
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    assert!(matches!(
        exact_wait(runtime, &owner.id, &task.id).await,
        WaitForRegistrationOutcome::TaskResultAlreadyConsumed { .. }
    ));
}

async fn exhaust_real_command(
    harness: &LifecycleHarness,
    provider: &FailureProvider,
    result: &MessageEnvelope,
) -> TaskResultSettlementRecord {
    let runtime = harness.runtime();
    provider.failures.store(5, Ordering::SeqCst);
    let mut roots = BTreeSet::new();
    for attempt in 1..=5 {
        harness
            .advance(Duration::from_secs(if attempt == 1 { 30 } else { 600 }))
            .await;
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        assert_eq!(latest(runtime, &result.id).recovery_attempts, attempt - 1);
        let scheduled = Box::pin(claim_model(runtime)).await;
        process(runtime, scheduled).await;
        let record = latest(runtime, &result.id);
        assert_recovery_snapshot(runtime, &record);
        assert_eq!(record.recovery_attempts, attempt);
        assert!(roots.insert(record.last_recovery_root_attempt_id.unwrap()));
        discard_provider_handoffs(runtime).await;
    }
    let paused = latest(runtime, &result.id);
    assert!(paused.recovery_paused_at.is_some());
    assert!(paused.next_recheck_at.is_none());
    assert_eq!(roots.len(), 5);
    assert_eq!(provider.requests.lock().await.len(), 5);
    paused
}

#[tokio::test(start_paused = true)]
async fn real_command_explicit_sixth_recovery_source_cannot_charge_or_call_provider() {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) = completed_command(runtime, &owner, "five-roots-evidence").await;
    let paused = Box::pin(exhaust_real_command(&harness, &provider, &result)).await;
    harness.advance(Duration::from_secs(600)).await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert!(matches!(
        exact_wait(runtime, &owner.id, &task.id).await,
        WaitForRegistrationOutcome::TaskResultUnavailable { reason, .. }
            if reason == "recovery_paused_requires_operator"
    ));
    // An explicitly submitted, correctly fenced sixth wake must fail closed too,
    // not merely disappear because the scanner stopped producing notifications.
    let (sixth, existing) = runtime
        .task_result_repair_source(&paused, Some(&owner.id))
        .unwrap();
    assert!(existing.is_none());
    let sixth_id = sixth.id.clone();
    assert!(!recovery_sources(runtime)
        .iter()
        .any(|source| source.id == sixth_id));
    runtime.enqueue(sixth).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("explicit sixth wake must reach the scheduler");
    };
    assert_eq!(scheduled.message.id, sixth_id);
    // A scheduler candidate is not a charged model admission. Exercise the
    // snapshot admission fence instead of asserting the earlier classification.
    process(runtime, scheduled).await;
    let after = latest(runtime, &result.id);
    assert_eq!(after.recovery_attempts, 5);
    assert_eq!(
        after.last_recovery_root_attempt_id,
        paused.last_recovery_root_attempt_id
    );
    assert_eq!(after.activation_id, paused.activation_id);
    assert_eq!(after.recovery_paused_at, paused.recovery_paused_at);
    assert_eq!(after.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(provider.requests.lock().await.len(), 5);
    // The paused settlement fails closed: the explicit sixth wake must not
    // persist a canonical recovery snapshot for itself.
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert!(turn
        .idle_input_batch
        .as_ref()
        .is_none_or(|batch| batch.items.iter().all(|item| item.message_id != sixth_id)));
}

#[tokio::test(start_paused = true)]
async fn real_command_paused_obligation_does_not_starve_same_owner_fresh_result() {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (_, exhausted) = completed_command(runtime, &owner, "exhausted-real-evidence").await;
    let paused = Box::pin(exhaust_real_command(&harness, &provider, &exhausted)).await;
    let (_, fresh) = completed_command(runtime, &owner, "fresh-real-evidence").await;
    harness.advance(Duration::from_secs(30)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let scheduled = Box::pin(claim_model(runtime)).await;
    let source_id = scheduled.message.id.clone();
    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = scheduled
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
    else {
        panic!("fresh result must have a real canonical recovery root");
    };
    let candidates = runtime
        .runtime_db()
        .task_result_settlements()
        .recovery_snapshot_candidates_for_owner(
            "default",
            Some(&owner.id),
            activation_id,
            runtime.now(),
            1,
        )
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].message_id, fresh.id);
    process(runtime, scheduled).await;
    let delivered = latest(runtime, &fresh.id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.recovery_attempts, 1);
    let after = latest(runtime, &exhausted.id);
    assert_eq!(after.recovery_attempts, 5);
    assert_eq!(after.recovery_paused_at, paused.recovery_paused_at);
    assert_eq!(after.activation_id, paused.activation_id);
    assert_eq!(after.state, TaskResultSettlementState::CallerAdmitted);
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 6);
    assert!(requests[5].contains("fresh-real-evidence"));
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert_eq!(batch.anchor_message_id, source_id);
    assert!(batch.items.iter().any(|item| item.message_id == source_id));
    assert!(!batch
        .items
        .iter()
        .any(|item| item.message_id == exhausted.id));
}
