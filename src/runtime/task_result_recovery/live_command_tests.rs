use super::*;
use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
use crate::runtime::waiting::{WaitForRegistrationOutcome, WaitForWakeKind};
use crate::runtime_db::task_result_settlement::TaskResultObligation;
use crate::types::CommandTaskSpec;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::Notify;

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

async fn completed_command(
    runtime: &RuntimeHandle,
    owner: &WorkItemRecord,
    evidence: &str,
) -> (TaskRecord, MessageEnvelope) {
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
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
    assert_eq!(created.work_item_id.as_deref(), Some(owner.id.as_str()));
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
    // Lose only the original queue delivery. No turn has admitted its content.
    remove_source(runtime, &result, QueueEntryStatus::Processed).await;
    (task, result)
}

async fn exact_wait(
    runtime: &RuntimeHandle,
    owner: &str,
    task: &str,
) -> WaitForRegistrationOutcome {
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(owner.into()),
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
        let owner = runtime.latest_work_item(owner).await.unwrap().unwrap();
        seed_waiting_work_execution(runtime.storage(), &owner, wait_condition_id);
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

async fn assert_first_admission_order(scanner_first: bool) {
    let provider = Arc::new(FailureProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = ready_owner(runtime).await;
    let (task, result) = completed_command(runtime, &owner, "first-admission-evidence").await;
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
    } = exact_wait(runtime, &owner.id, &task.id).await
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
            && wait.work_item_id.as_deref() == Some(owner.id.as_str())
            && wait.trigger_message_id() == Some(recovery_message_id.as_str())
    }));
    let scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(scheduled.message.id, recovery_message_id);
    assert_eq!(scheduled.message.source_refs, source.source_refs);
    assert_eq!(latest(runtime, &result.id).recovery_attempts, 0);
    process(runtime, scheduled).await;
    let delivered = latest(runtime, &result.id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(
        delivered.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(delivered.recovery_attempts, 1);
    assert_eq!(delivered.work_item_id, Some(owner.id));
    assert_eq!(runtime.task_record(&task.id).await.unwrap().unwrap(), task);
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("first-admission-evidence"));
}

#[tokio::test(start_paused = true)]
async fn real_command_first_exact_wait_then_scanner_has_one_admission() {
    Box::pin(assert_first_admission_order(false)).await;
}

#[tokio::test(start_paused = true)]
async fn real_command_first_scanner_then_exact_wait_reuses_one_admission() {
    Box::pin(assert_first_admission_order(true)).await;
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
