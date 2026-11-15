use super::super::*;
use super::support::*;
use crate::domain::execution_protocol::{
    ConversationOutcome, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
    ExecutionOutcome, ExecutionProtocolState, ExecutionSourceIdentity, WorkItemExecutionRecord,
    WorkItemExecutionState, WorkItemOutcome,
};
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementRecord, TaskResultSettlementState,
};
use crate::types::{WaitConditionRecord, WaitConditionStatus, WorkItemPlanStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct AbortReplayProvider {
    started: tokio::sync::Notify,
    calls: AtomicUsize,
    active: Arc<AtomicUsize>,
    peak_active: AtomicUsize,
}

struct ActiveProviderCall(Arc<AtomicUsize>);

impl Drop for ActiveProviderCall {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl AgentProvider for AbortReplayProvider {
    async fn complete_turn(&self, _: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_active.fetch_max(active, Ordering::SeqCst);
        let _active = ActiveProviderCall(self.active.clone());
        self.started.notify_one();
        if call == 0 {
            return std::future::pending().await;
        }
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "fresh input completed".into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

fn operator_prompt(text: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    )
}

fn open_runtime(
    home: &std::path::Path,
    workspace: &std::path::Path,
    provider: Arc<dyn AgentProvider>,
) -> RuntimeHandle {
    let runtime = RuntimeHandle::new(
        "default",
        home.to_path_buf(),
        workspace.to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider,
        "default".into(),
        context_config(),
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    runtime
}

fn queue_status(runtime: &RuntimeHandle, source: &MessageEnvelope) -> QueueEntryStatus {
    runtime
        .storage()
        .latest_queue_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == source.id)
        .expect("original queue entry")
        .status
}

fn assert_aborted_source(runtime: &RuntimeHandle, source: &MessageEnvelope) {
    assert_eq!(queue_status(runtime, source), QueueEntryStatus::Aborted);
    let turn = runtime
        .storage()
        .read_turn_by_id(source.turn_id.as_deref().expect("original turn id"))
        .unwrap()
        .expect("original input turn");
    assert!(turn.input_message_ids.contains(&source.id));
    let terminal = turn.terminal.expect("original turn terminal");
    assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
    assert_eq!(terminal.reason.as_deref(), Some("operator_aborted"));
}

async fn wait_for_queue(
    runtime: &RuntimeHandle,
    source: &MessageEnvelope,
    status: QueueEntryStatus,
) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            if queue_status(runtime, source) == status {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original queue terminal should settle");
}

async fn start_blocked_run(
    runtime: &RuntimeHandle,
    provider: &AbortReplayProvider,
) -> (MessageEnvelope, tokio::task::JoinHandle<Result<()>>) {
    let source = runtime
        .enqueue(operator_prompt(
            "original input must not replay after abort",
        ))
        .await
        .unwrap();
    let mut running = tokio::spawn(runtime.clone().run());
    tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            _ = provider.started.notified() => {}
            result = &mut running => panic!("runtime exited before provider start: {result:?}"),
        }
    })
    .await
    .expect("original input should reach the provider");
    (source, running)
}

async fn abort_run(runtime: &RuntimeHandle, mode: CurrentRunAbortMode) -> String {
    let run_id = runtime
        .agent_state()
        .await
        .unwrap()
        .current_run_id
        .expect("original run is active");
    let outcome = runtime
        .abort_current_run(CurrentRunAbortRequest {
            run_id: Some(run_id.clone()),
            mode,
        })
        .await
        .unwrap();
    assert_eq!(outcome.run_id, run_id);
    run_id
}

async fn join_stopped_run(running: tokio::task::JoinHandle<Result<()>>) {
    tokio::time::timeout(DEADLINE, running)
        .await
        .expect("stopped run should finish")
        .unwrap()
        .unwrap();
}

// Do not let the original producer perform its normal terminal write after ACK.
// Recovery must rely on the durable abort decision, not this task's cleanup.
async fn crash_after_abort_ack(running: tokio::task::JoinHandle<Result<()>>) {
    running.abort();
    match running.await {
        Ok(result) => result.unwrap(),
        Err(error) => assert!(error.is_cancelled()),
    }
}

async fn assert_start_cannot_replay(runtime: &RuntimeHandle, source: &MessageEnvelope) {
    for _ in 0..2 {
        runtime.control(ControlAction::Start).await.unwrap();
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        assert!(
            !matches!(poll, scheduler_executor::RunLoopPoll::Message(_)),
            "Start must not claim an operator-aborted input"
        );
        assert_aborted_source(runtime, source);
    }
}

#[tokio::test]
async fn operator_abort_ack_wins_over_success_prepared_before_terminal_commit() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    provider.calls.store(1, Ordering::SeqCst);
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let checkpoint = Arc::new(TransitionTestCheckpoint::default());
    *runtime
        .inner
        .producer_terminal_commit_checkpoint
        .lock()
        .unwrap() = Some(checkpoint.clone());
    let (source, mut running) = start_blocked_run(&runtime, &provider).await;

    tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            _ = checkpoint.reached.notified() => {}
            result = &mut running => panic!("runtime exited before terminal commit: {result:?}"),
        }
    })
    .await
    .expect("successful provider response should reach the terminal commit checkpoint");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
    let original_attempt = execution_protocol(&runtime)
        .open_attempt()
        .expect("successful response has not settled its attempt")
        .clone();
    assert_eq!(
        original_attempt.source_message_id.as_deref(),
        Some(source.id.as_str())
    );

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    checkpoint.release.notify_one();
    join_stopped_run(running).await;

    assert_aborted_source(&runtime, &source);
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::Stopped);
    let terminal = state
        .last_turn_terminal
        .expect("cancelled producer terminal");
    assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
    assert_eq!(terminal.reason.as_deref(), Some("operator_aborted"));
    let execution = execution_protocol(&runtime);
    let attempt = &execution.attempts[&original_attempt.attempt_id];
    assert_eq!(attempt.state, ExecutionAttemptState::Settled);
    assert_eq!(attempt.binding, original_attempt.binding);
    assert_eq!(attempt.source, original_attempt.source);
    let outcome = &execution.outcomes[attempt
        .terminal_outcome_id
        .as_ref()
        .expect("cancelled attempt has a terminal outcome")];
    assert_eq!(outcome.attempt_id, original_attempt.attempt_id);
    assert_eq!(
        outcome.outcome,
        ExecutionOutcome::Conversation(ConversationOutcome::Aborted {
            reason: "operator_aborted".into(),
        })
    );
    assert_eq!(
        execution
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(source.id.as_str()))
            .count(),
        1,
        "losing success must not manufacture another attempt"
    );
    assert_eq!(
        execution
            .outcomes
            .values()
            .filter(|outcome| outcome.attempt_id == original_attempt.attempt_id)
            .count(),
        1,
        "losing success must not append a successful outcome"
    );
    assert!(execution.open_attempt().is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn operator_abort_stop_terminalizes_original_turn_and_queue_as_aborted() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    join_stopped_run(running).await;

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::Stopped);
    assert_eq!(state.current_run_id, None);
    let terminal = state.last_turn_terminal.expect("abort terminal in State");
    assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
    assert_eq!(terminal.reason.as_deref(), Some("operator_aborted"));
    assert_aborted_source(&runtime, &source);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    assert_eq!(
        runtime.agent_state().await.unwrap().status,
        AgentStatus::Stopped
    );
    assert_start_cannot_replay(&runtime, &source).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_abort_stop_ack_survives_crash_before_producer_settlement() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    crash_after_abort_ack(running).await;
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;

    assert_eq!(
        runtime.agent_state().await.unwrap().status,
        AgentStatus::Stopped
    );
    assert_aborted_source(&runtime, &source);
    assert_start_cannot_replay(&runtime, &source).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_aborted_input_stays_terminal_when_an_independent_wake_runs() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    join_stopped_run(running).await;
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    runtime.control(ControlAction::Start).await.unwrap();
    let disposition = runtime
        .submit_wake_hint(crate::ingress::WakeHint {
            agent_id: "default".into(),
            reason: "independent wake after operator abort".into(),
            description: None,
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: Some(MessageBody::Text {
                text: "only this independent wake may run".into(),
            }),
            content_type: None,
            correlation_id: None,
            causation_id: None,
        })
        .await
        .unwrap();
    assert_eq!(disposition, crate::ingress::WakeDisposition::Triggered);
    let wake = runtime
        .storage()
        .read_recent_messages(16)
        .unwrap()
        .into_iter()
        .find(|message| message.kind == MessageKind::SystemTick)
        .expect("independent wake should emit a new tick");
    assert_ne!(wake.id, source.id);

    let running = tokio::spawn(runtime.clone().run());
    wait_for_queue(&runtime, &wake, QueueEntryStatus::Processed).await;
    assert_aborted_source(&runtime, &source);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.peak_active.load(Ordering::SeqCst), 1);
    runtime.control(ControlAction::Stop).await.unwrap();
    join_stopped_run(running).await;
}

#[tokio::test]
async fn debug_scheduler_recovery_does_not_reactivate_an_operator_aborted_input() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    crash_after_abort_ack(running).await;
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    for _ in 0..2 {
        let report =
            scheduler_recovery_report(runtime.storage(), &runtime.inner.runtime_db, "default")
                .unwrap();
        assert!(
            report
                .candidates
                .iter()
                .filter(|candidate| candidate.message_id == source.id)
                .all(|candidate| !candidate.eligible),
            "debug recovery must not offer the canceled source for replay"
        );
        assert_eq!(
            apply_scheduler_recovery_plan_with_backup_policy(
                runtime.storage(),
                &runtime.inner.runtime_db,
                "default",
                &report,
                SchedulerRecoveryBackupPolicy::SkipApproved,
            )
            .unwrap(),
            (0, None)
        );
        assert_aborted_source(&runtime, &source);
    }
    assert_start_cannot_replay(&runtime, &source).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_abort_idle_accepts_fresh_input_without_overlapping_provider_calls() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::IdleAfterAbort).await;
    // Submit immediately after ACK, rather than waiting for the original
    // provider future to drop or the abort terminal to become observable.
    let fresh = runtime
        .enqueue(operator_prompt("independent input after idle abort"))
        .await
        .unwrap();
    wait_for_queue(&runtime, &fresh, QueueEntryStatus::Processed).await;

    assert_aborted_source(&runtime, &source);
    assert_ne!(
        runtime.agent_state().await.unwrap().status,
        AgentStatus::Stopped
    );
    assert_ne!(fresh.turn_id, source.turn_id);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.peak_active.load(Ordering::SeqCst), 1);
    let fresh_turn = runtime
        .storage()
        .read_turn_by_id(fresh.turn_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(fresh_turn.input_message_ids, vec![fresh.id]);
    assert_eq!(
        fresh_turn.terminal.unwrap().kind,
        TurnTerminalKind::Completed
    );

    runtime.control(ControlAction::Stop).await.unwrap();
    join_stopped_run(running).await;
}

#[tokio::test]
async fn operator_abort_idle_ack_survives_crash_and_allows_only_fresh_input() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let (source, running) = start_blocked_run(&runtime, &provider).await;

    abort_run(&runtime, CurrentRunAbortMode::IdleAfterAbort).await;
    crash_after_abort_ack(running).await;
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    assert_ne!(
        runtime.agent_state().await.unwrap().status,
        AgentStatus::Stopped
    );
    assert_aborted_source(&runtime, &source);
    assert_start_cannot_replay(&runtime, &source).await;

    let fresh = runtime
        .enqueue(operator_prompt("fresh input after daemon restart"))
        .await
        .unwrap();
    let running = tokio::spawn(runtime.clone().run());
    wait_for_queue(&runtime, &fresh, QueueEntryStatus::Processed).await;
    assert_aborted_source(&runtime, &source);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.peak_active.load(Ordering::SeqCst), 1);
    runtime.control(ControlAction::Stop).await.unwrap();
    join_stopped_run(running).await;
}

// Seed the persisted shape produced before operator abort gained its own
// negative replay fence. Reopen before consuming it so resident evidence comes
// from the same canonical records as a real daemon restart.
async fn historical_interrupted_input(
    reason: &str,
) -> (TempDir, TempDir, RuntimeHandle, MessageEnvelope) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = open_runtime(
        home.path(),
        workspace.path(),
        Arc::new(StubProvider::new("unused")),
    );
    let source = runtime
        .enqueue(operator_prompt("historical interrupted input"))
        .await
        .unwrap();
    let mut turn = TurnRecord::new(
        "default",
        source.turn_id.as_deref().expect("historical turn id"),
        1,
    );
    turn.input_message_ids = vec![source.id.clone()];
    turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
    turn.terminal = Some(crate::types::TurnTerminalSummary {
        kind: TurnTerminalKind::Interrupted,
        reason: Some(reason.into()),
        no_brief_reason: Some(crate::types::TurnNoBriefReason::Interrupted),
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    // Qualify the historical Turn without manufacturing an execution attempt.
    let mut mutation = runtime.begin_mutation().await.unwrap();
    let prepared = crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::turn(
        mutation.input_evidence_scope(),
        crate::ids::audit_event_id(),
        runtime.now(),
        turn,
    )
    .unwrap();
    mutation
        .commit_prepared_evidence(prepared, None)
        .await
        .unwrap();
    drop(mutation);
    let mut queue = runtime
        .storage()
        .latest_queue_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == source.id)
        .unwrap();
    queue.status = QueueEntryStatus::Interrupted;
    runtime.storage().append_queue_entry(&queue).unwrap();
    let attempt_count: u64 = runtime
        .storage()
        .runtime_db()
        .unwrap()
        .unwrap()
        .with_read(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM execution_protocol_attempts
                 WHERE agent_id = ?1 AND json_extract(payload_json, '$.source_message_id') = ?2",
                rusqlite::params![source.agent_id, source.id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        attempt_count, 0,
        "historical input has no execution attempt"
    );
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    (home, workspace, runtime, source)
}

#[tokio::test]
async fn historical_operator_aborted_interrupted_input_is_not_replayed_after_startup() {
    let (_home, _workspace, runtime, source) =
        historical_interrupted_input("operator_aborted").await;
    let original_turn_count = runtime.storage().read_recent_turns(16).unwrap().len();
    for _ in 0..2 {
        runtime.control(ControlAction::Start).await.unwrap();
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap();
        assert!(
            !matches!(poll, scheduler_executor::RunLoopPoll::Message(_)),
            "historical operator_aborted is a negative replay decision"
        );
    }
    assert_eq!(
        runtime.storage().read_recent_turns(16).unwrap().len(),
        original_turn_count
    );
    assert_ne!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
}

#[tokio::test]
async fn historical_crash_interrupted_input_still_replays_after_startup() {
    let (_home, _workspace, runtime, source) =
        historical_interrupted_input("runtime_interrupted").await;
    runtime.control(ControlAction::Start).await.unwrap();
    let poll = scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
        panic!("ordinary crash Interrupted must remain recoverable");
    };
    assert_eq!(scheduled.message.id, source.id);
    assert_eq!(
        scheduled.message.source_refs.get("replay_source_turn_id"),
        source.turn_id.as_ref()
    );
    assert_eq!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
    finish_claimed_test_run(&runtime).await;
}

#[tokio::test]
async fn operator_abort_stop_preserves_independent_work_task_wait_and_timer() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let work = runtime
        .create_work_item("independent waiting work".into(), None, None, Vec::new())
        .await
        .unwrap();
    let timer = runtime.schedule_timer(3_600_000, None, None).await.unwrap();
    runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "independent timer dependency".into(),
            None,
        )
        .await
        .unwrap();
    let task = TaskRecord {
        id: "task-independent-of-operator-abort".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: Some(work.id.clone()),
        summary: Some("independent command task".into()),
        detail: None,
        recovery: None,
    };
    persist_task_transition(&runtime, &task, "task_started")
        .await
        .unwrap();
    let (source, running) = start_blocked_run(&runtime, &provider).await;
    let work_before = runtime
        .storage()
        .read_recent_work_items(16)
        .unwrap()
        .into_iter()
        .find(|record| record.id == work.id)
        .unwrap();
    let tasks_before = runtime.storage().read_recent_tasks(16).unwrap();
    let waits_before = runtime.storage().latest_wait_conditions().unwrap();
    let timers_before = runtime.storage().latest_timer_records().unwrap();
    assert!(tasks_before
        .iter()
        .any(|record| record.id == task.id && record.status == TaskStatus::Running));
    assert!(waits_before
        .iter()
        .any(|record| record.work_item_id.as_deref() == Some(work.id.as_str())));

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    join_stopped_run(running).await;
    assert_aborted_source(&runtime, &source);
    assert_eq!(
        runtime
            .storage()
            .read_recent_work_items(16)
            .unwrap()
            .into_iter()
            .find(|record| record.id == work.id)
            .unwrap(),
        work_before
    );
    assert_eq!(
        runtime.storage().read_recent_tasks(16).unwrap(),
        tasks_before
    );
    assert_eq!(
        runtime.storage().latest_wait_conditions().unwrap(),
        waits_before
    );
    assert_eq!(
        runtime.storage().latest_timer_records().unwrap(),
        timers_before
    );
}

fn execution_protocol(runtime: &RuntimeHandle) -> ExecutionProtocolState {
    runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .expect("scheduler admitted the original source")
}

fn result_settlement(
    runtime: &RuntimeHandle,
    source: &MessageEnvelope,
) -> TaskResultSettlementRecord {
    runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&source.id)
        .unwrap()
        .expect("original task-result settlement")
}

fn wait_condition(runtime: &RuntimeHandle, id: &str) -> WaitConditionRecord {
    runtime
        .storage()
        .latest_wait_conditions()
        .unwrap()
        .into_iter()
        .find(|condition| condition.id == id)
        .expect("original wait")
}

struct IndependentAbortSnapshot {
    work: WorkItemRecord,
    task: TaskRecord,
    wait: WaitConditionRecord,
    timer: TimerRecord,
    execution: WorkItemExecutionRecord,
}

impl IndependentAbortSnapshot {
    async fn seed(runtime: &RuntimeHandle) -> Self {
        let work = runtime
            .create_work_item("independent blocked work".into(), None, None, Vec::new())
            .await
            .unwrap();
        let timer = runtime.schedule_timer(3_600_000, None, None).await.unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "independent timer wait".into(),
                None,
            )
            .await
            .unwrap();
        let work = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
        seed_live_waiting_work_execution(runtime, &work, &registration.condition.id).await;
        // A terminal task is stable across cold startup. The separate live-run
        // test above covers an unrelated Running task without crash recovery.
        let task = TaskRecord {
            id: "task-independent-terminal-result".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Completed,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: Some(work.id.clone()),
            summary: Some("independent completed task".into()),
            detail: None,
            recovery: None,
        };
        persist_task_transition(runtime, &task, "independent_task_completed")
            .await
            .unwrap();
        let execution = execution_protocol(runtime).work_items[&work.id].clone();
        Self {
            work,
            task,
            wait: wait_condition(runtime, &registration.condition.id),
            timer,
            execution,
        }
    }

    async fn assert_unchanged(&self, runtime: &RuntimeHandle) {
        assert_eq!(
            runtime.latest_work_item(&self.work.id).await.unwrap(),
            Some(self.work.clone()),
            "canceling the original input must not alter independent work"
        );
        assert_eq!(
            runtime
                .storage()
                .read_recent_tasks(16)
                .unwrap()
                .into_iter()
                .find(|task| task.id == self.task.id),
            Some(self.task.clone())
        );
        assert_eq!(wait_condition(runtime, &self.wait.id), self.wait);
        assert_eq!(
            runtime
                .storage()
                .latest_timer_records()
                .unwrap()
                .into_iter()
                .find(|timer| timer.id == self.timer.id),
            Some(self.timer.clone())
        );
        assert_eq!(
            execution_protocol(runtime).work_items.get(&self.work.id),
            Some(&self.execution)
        );
    }
}

async fn publish_abortable_task_result(
    runtime: &RuntimeHandle,
    exact_wait: bool,
) -> (MessageEnvelope, TaskRecord, Option<String>) {
    let owner = runtime
        .create_work_item(
            "task-result cancellation owner remains open".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let task = TaskRecord {
        id: "task-original-aborted-result".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: Some(owner.id.clone()),
        summary: Some("original task result".into()),
        detail: None,
        recovery: None,
    };
    persist_task_transition(runtime, &task, "original_task_started")
        .await
        .unwrap();
    let wait_id = if exact_wait {
        // A same-owner wait is still an ExactTaskRejoin. A different owner
        // grants this attempt only the exclusive exact dependency wake.
        let waiter = runtime
            .create_work_item(
                "exclusive task-result waiter remains open".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                Some(waiter.id.clone()),
                WaitForWakeKind::TaskResult,
                Some(task.id.clone()),
                "exact original task-result wait".into(),
                None,
            )
            .await
            .unwrap();
        let waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
        assert_ne!(waiter.id, owner.id);
        seed_live_waiting_work_execution(runtime, &waiter, &registration.condition.id).await;
        Some(registration.condition.id)
    } else {
        None
    };
    persist_completed_task_parent_fixture(
        runtime,
        "turn-original-aborted-task-parent",
        Some(&owner.id),
    )
    .await
    .unwrap();
    let detail = serde_json::json!({
        "rejoin_obligation_id": task.id,
        "rejoin_generation": 1,
        "parent_turn_id": "turn-original-aborted-task-parent",
        "terminal_reentry": true,
    });
    let mut source = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "only this task-result input is canceled".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    source.task_id = Some(task.id.clone());
    source.work_item_id = Some(owner.id);
    source.turn_id = Some(crate::ids::turn_id());
    source.metadata = Some(serde_json::json!({
        "task_id": task.id,
        "task_kind": task.kind.as_str(),
        "task_status": "completed",
        "task_result_id": source.id,
        "task_detail": detail,
        "work_item_id": source.work_item_id,
    }));
    let terminal_task = TaskRecord {
        status: TaskStatus::Completed,
        updated_at: Utc::now(),
        parent_message_id: Some(source.id.clone()),
        detail: Some(detail),
        ..task
    };
    runtime
        .commit_terminal_task_result(&terminal_task, "task_completed", &source)
        .await
        .unwrap();
    let source = runtime
        .storage()
        .read_message_by_id(&source.id)
        .unwrap()
        .expect("qualified terminal Task writer published the result");
    if let Some(wait_id) = &wait_id {
        let wait = wait_condition(runtime, wait_id);
        assert_eq!(wait.status, WaitConditionStatus::Triggered);
        assert_eq!(wait.trigger_message_id(), Some(source.id.as_str()));
    }
    (source, terminal_task, wait_id)
}

fn assert_task_result_cancelled(
    runtime: &RuntimeHandle,
    source: &MessageEnvelope,
    original_attempt: &ExecutionAttempt,
) {
    assert_aborted_source(runtime, source);
    let execution = execution_protocol(runtime);
    let attempt = &execution.attempts[&original_attempt.attempt_id];
    assert_eq!(attempt.state, ExecutionAttemptState::Settled);
    assert_eq!(attempt.source, original_attempt.source);
    assert_eq!(attempt.binding, original_attempt.binding);
    assert_eq!(
        attempt.source_message_id.as_deref(),
        Some(source.id.as_str())
    );
    let outcome_id = attempt
        .terminal_outcome_id
        .as_ref()
        .expect("cancellation has an exact terminal execution outcome");
    let outcome = &execution.outcomes[outcome_id];
    assert_eq!(outcome.attempt_id, original_attempt.attempt_id);
    assert_eq!(
        outcome.outcome,
        ExecutionOutcome::WorkItem(WorkItemOutcome::Aborted {
            reason: "operator_aborted".into(),
        })
    );
    assert_eq!(
        execution
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(source.id.as_str()))
            .count(),
        1,
        "operator cancellation must not manufacture a recovery attempt"
    );
    assert_eq!(
        execution
            .outcomes
            .values()
            .filter(|outcome| outcome.attempt_id == original_attempt.attempt_id)
            .count(),
        1
    );
    let settlement = result_settlement(runtime, source);
    assert_eq!(settlement.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settlement.disposition,
        Some(TaskResultSettlementDisposition::InvalidOrStale)
    );
    assert_eq!(
        settlement.activation_id.as_deref(),
        Some(original_attempt.attempt_id.as_str())
    );
    assert_eq!(settlement.message_id, source.id);
    assert_eq!(settlement.task_id, source.task_id.as_deref().unwrap());
    assert_eq!(settlement.work_item_id, source.work_item_id);
    assert!(settlement.admitted_at.is_some());
    assert!(settlement.settled_at.is_some());
    let ExecutionBinding::WorkItem { work_item_id } = &original_attempt.binding else {
        panic!("task-result fixture must consume into a WorkItem execution");
    };
    assert!(matches!(
        &execution.work_items[work_item_id].state,
        WorkItemExecutionState::Runnable {
            recovery_ref: None,
            ..
        }
    ));
}

async fn task_result_abort_fixture(exact_wait: bool, crash_after_ack: bool) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let independent = IndependentAbortSnapshot::seed(&runtime).await;
    let (source, task_before, wait_id) = publish_abortable_task_result(&runtime, exact_wait).await;
    let mut running = tokio::spawn(runtime.clone().run());
    tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            _ = provider.started.notified() => {}
            result = &mut running => panic!("runtime exited before task-result dispatch: {result:?}"),
        }
    })
    .await
    .expect("task-result source should reach the provider");
    assert_eq!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
    let execution = execution_protocol(&runtime);
    let original_attempt = execution
        .open_attempt()
        .expect("live original attempt")
        .clone();
    assert_eq!(
        original_attempt.source_message_id.as_deref(),
        Some(source.id.as_str())
    );
    let consuming_owner_id = wait_id
        .as_deref()
        .map(|id| {
            wait_condition(&runtime, id)
                .work_item_id
                .expect("exclusive task wait has a WorkItem owner")
        })
        .unwrap_or_else(|| source.work_item_id.clone().unwrap());
    if exact_wait {
        assert_ne!(
            Some(consuming_owner_id.as_str()),
            source.work_item_id.as_deref(),
            "exact-wait admission must not fall back to the task owner's rejoin"
        );
    }
    assert_eq!(
        original_attempt.binding,
        ExecutionBinding::WorkItem {
            work_item_id: consuming_owner_id.clone(),
        }
    );
    let expected_source = match &wait_id {
        Some(wait_id) => ExecutionSourceIdentity::TriggeredWait {
            wait_id: wait_id.clone(),
            trigger_message_id: source.id.clone(),
            ingress: None,
        },
        None => ExecutionSourceIdentity::TaskResult {
            task_id: task_before.id.clone(),
            result_message_id: source.id.clone(),
        },
    };
    assert_eq!(original_attempt.source.identity, expected_source);
    let admitted = result_settlement(&runtime, &source);
    assert_eq!(admitted.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(
        admitted.activation_id.as_deref(),
        Some(original_attempt.attempt_id.as_str())
    );
    let owner_before = runtime
        .latest_work_item(&consuming_owner_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner_before.state, WorkItemState::Open);
    let task_owner_before = runtime
        .latest_work_item(task_before.work_item_id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task_owner_before.state, WorkItemState::Open);
    let resolved_wait = wait_id.as_deref().map(|id| wait_condition(&runtime, id));
    if let Some(wait) = &resolved_wait {
        assert_eq!(wait.status, WaitConditionStatus::Resolved);
        assert_eq!(wait.trigger_message_id(), Some(source.id.as_str()));
    }

    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    if crash_after_ack {
        crash_after_abort_ack(running).await;
    } else {
        join_stopped_run(running).await;
        assert_task_result_cancelled(&runtime, &source, &original_attempt);
        independent.assert_unchanged(&runtime).await;
        assert_eq!(
            runtime.latest_work_item(&owner_before.id).await.unwrap(),
            Some(owner_before.clone())
        );
        assert_eq!(
            runtime
                .latest_work_item(&task_owner_before.id)
                .await
                .unwrap(),
            Some(task_owner_before.clone())
        );
        if let Some(wait) = &resolved_wait {
            assert_eq!(wait_condition(&runtime, &wait.id), *wait);
        }
    }
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    assert_task_result_cancelled(&runtime, &source, &original_attempt);
    independent.assert_unchanged(&runtime).await;
    assert_eq!(
        runtime.latest_work_item(&owner_before.id).await.unwrap(),
        Some(owner_before)
    );
    assert_eq!(
        runtime
            .latest_work_item(&task_owner_before.id)
            .await
            .unwrap(),
        Some(task_owner_before)
    );
    assert_eq!(
        runtime
            .storage()
            .read_recent_tasks(16)
            .unwrap()
            .into_iter()
            .find(|task| task.id == task_before.id),
        Some(task_before),
        "canceling delivery must not rewrite the completed source task"
    );
    if let Some(wait) = resolved_wait {
        assert_eq!(wait_condition(&runtime, &wait.id), wait);
    }
    let settled = result_settlement(&runtime, &source);
    assert_start_cannot_replay(&runtime, &source).await;
    assert_task_result_cancelled(&runtime, &source, &original_attempt);
    assert_eq!(result_settlement(&runtime, &source), settled);
    independent.assert_unchanged(&runtime).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(provider.peak_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_abort_task_result_settles_consumption_and_preserves_open_work() {
    Box::pin(task_result_abort_fixture(false, false)).await;
}

#[tokio::test]
async fn operator_abort_task_result_ack_crash_bootstrap_settles_original_attempt() {
    Box::pin(task_result_abort_fixture(false, true)).await;
}

#[tokio::test]
async fn operator_abort_exact_task_wait_settles_consumption_without_reopening_wait() {
    Box::pin(task_result_abort_fixture(true, false)).await;
}

#[tokio::test]
async fn operator_abort_exact_task_wait_ack_crash_bootstrap_does_not_replay() {
    Box::pin(task_result_abort_fixture(true, true)).await;
}

fn assert_cancelled_attempt(
    runtime: &RuntimeHandle,
    source: &MessageEnvelope,
    original: &ExecutionAttempt,
    expected_outcome: ExecutionOutcome,
) {
    assert_aborted_source(runtime, source);
    let execution = execution_protocol(runtime);
    let attempt = &execution.attempts[&original.attempt_id];
    assert_eq!(attempt.state, ExecutionAttemptState::Settled);
    assert_eq!(attempt.source, original.source);
    assert_eq!(attempt.binding, original.binding);
    assert_eq!(attempt.admitted_fences, original.admitted_fences);
    assert_eq!(
        attempt.recovery_of_attempt_id,
        original.recovery_of_attempt_id
    );
    assert_eq!(
        attempt.source_message_id.as_deref(),
        Some(source.id.as_str())
    );
    let terminal = &execution.outcomes[attempt
        .terminal_outcome_id
        .as_ref()
        .expect("original attempt has a terminal outcome")];
    assert_eq!(terminal.attempt_id, original.attempt_id);
    assert_eq!(terminal.outcome, expected_outcome);
    assert_eq!(
        execution
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(source.id.as_str()))
            .count(),
        1,
        "cancellation must not manufacture a replay or recovery attempt"
    );
    assert_eq!(
        execution
            .outcomes
            .values()
            .filter(|outcome| outcome.attempt_id == original.attempt_id)
            .count(),
        1,
        "only the exact original attempt may be settled"
    );
    assert!(execution.open_attempt().is_none());
}

async fn await_abortable_provider(
    provider: &AbortReplayProvider,
    running: &mut tokio::task::JoinHandle<Result<()>>,
) {
    tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            _ = provider.started.notified() => {}
            result = running => panic!("runtime exited before source dispatch: {result:?}"),
        }
    })
    .await
    .expect("original source should reach the blocked provider");
}

async fn exact_wake_abort_fixture(wake: WaitForWakeKind, crash_after_ack: bool) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortReplayProvider::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let independent = IndependentAbortSnapshot::seed(&runtime).await;
    let owner = runtime
        .create_work_item(
            "exact wake cancellation owner remains open".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let (mut timer, trigger_id, resource) = match wake {
        WaitForWakeKind::Timer => {
            let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
            let resource = timer.id.clone();
            (Some(timer), None, resource)
        }
        WaitForWakeKind::External => {
            let resource = "github:holon-run/holon#3905-exact-wake".to_string();
            let trigger = runtime
                .create_external_trigger(
                    "exact abort regression wake".into(),
                    "github".into(),
                    ExternalTriggerScope::Agent,
                    CallbackDeliveryMode::WakeHint,
                    Some("the exact external dependency changed".into()),
                    Some(resource.clone()),
                )
                .await
                .unwrap();
            (None, Some(trigger.external_trigger_id), resource)
        }
        _ => unreachable!("this fixture requires a real timer or callback wake"),
    };
    let registration = runtime
        .register_wait_for(
            "default",
            Some(owner.id.clone()),
            wake,
            Some(resource),
            "resume only the exact original wake".into(),
            None,
        )
        .await
        .unwrap();
    let owner = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
    seed_live_waiting_work_execution(&runtime, &owner, &registration.condition.id).await;
    let source = if let Some(timer) = timer.as_mut() {
        runtime.fire_timer_record(timer).await.unwrap();
        let pending = runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .expect("the real Timer writer published its exact wake");
        let source = runtime
            .storage()
            .read_message_by_id(&pending.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(source.kind, MessageKind::TimerTick);
        assert_eq!(
            source.origin,
            MessageOrigin::Timer {
                timer_id: timer.id.clone(),
            }
        );
        assert_eq!(source.source_refs.get("timer_id"), Some(&timer.id));
        source
    } else {
        let trigger_id = trigger_id.as_deref().unwrap();
        let delivery = runtime
            .deliver_callback(
                trigger_id,
                CallbackDeliveryPayload {
                    body: Some(MessageBody::Json {
                        value: serde_json::json!({"status": "ready"}),
                    }),
                    content_type: Some("application/json".into()),
                    correlation_id: Some("abort-exact-external-wake".into()),
                    causation_id: Some("external-dependency-changed".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(delivery.disposition, CallbackIngressDisposition::Triggered);
        let source = runtime
            .storage()
            .read_recent_messages(16)
            .unwrap()
            .into_iter()
            .find(|message| message.source_refs.get("wait_id") == Some(&registration.condition.id))
            .expect("callback ingress published the exact external wait wake");
        assert_eq!(source.kind, MessageKind::SystemTick);
        assert_eq!(
            source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata["wake_hint"]["external_trigger_id"].as_str()),
            Some(trigger_id)
        );
        source
    };
    assert_eq!(
        wait_condition(&runtime, &registration.condition.id).status,
        WaitConditionStatus::Triggered
    );
    let mut running = tokio::spawn(runtime.clone().run());
    await_abortable_provider(&provider, &mut running).await;
    assert_eq!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
    let original_attempt = execution_protocol(&runtime)
        .open_attempt()
        .expect("the real wake was admitted")
        .clone();
    assert_eq!(
        original_attempt.source.identity,
        ExecutionSourceIdentity::TriggeredWait {
            wait_id: registration.condition.id.clone(),
            trigger_message_id: source.id.clone(),
            ingress: None,
        }
    );
    assert_eq!(
        original_attempt.binding,
        ExecutionBinding::WorkItem {
            work_item_id: owner.id.clone(),
        }
    );
    let resolved_wait = wait_condition(&runtime, &registration.condition.id);
    assert_eq!(resolved_wait.status, WaitConditionStatus::Resolved);
    assert_eq!(resolved_wait.trigger_message_id(), Some(source.id.as_str()));
    let owner_before = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
    let expected_outcome = ExecutionOutcome::WorkItem(WorkItemOutcome::Aborted {
        reason: "operator_aborted".into(),
    });
    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    if crash_after_ack {
        crash_after_abort_ack(running).await;
    } else {
        join_stopped_run(running).await;
        assert_cancelled_attempt(
            &runtime,
            &source,
            &original_attempt,
            expected_outcome.clone(),
        );
        independent.assert_unchanged(&runtime).await;
        assert_eq!(wait_condition(&runtime, &resolved_wait.id), resolved_wait);
    }
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    assert_cancelled_attempt(
        &runtime,
        &source,
        &original_attempt,
        expected_outcome.clone(),
    );
    assert_eq!(wait_condition(&runtime, &resolved_wait.id), resolved_wait);
    independent.assert_unchanged(&runtime).await;
    assert_start_cannot_replay(&runtime, &source).await;
    assert_cancelled_attempt(&runtime, &source, &original_attempt, expected_outcome);
    assert_eq!(wait_condition(&runtime, &resolved_wait.id), resolved_wait);
    assert_eq!(
        runtime.latest_work_item(&owner_before.id).await.unwrap(),
        Some(owner_before)
    );
    if let Some(timer) = timer {
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(
            runtime
                .storage()
                .latest_timer_records()
                .unwrap()
                .into_iter()
                .find(|record| record.id == timer.id),
            Some(timer),
            "canceling delivery must not rewrite the fired source timer"
        );
    }
    independent.assert_unchanged(&runtime).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(provider.peak_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_abort_exact_timer_wait_preserves_independent_wait_and_timer() {
    Box::pin(exact_wake_abort_fixture(WaitForWakeKind::Timer, false)).await;
}

#[tokio::test]
async fn operator_abort_exact_timer_wait_ack_crash_bootstrap_does_not_replay() {
    Box::pin(exact_wake_abort_fixture(WaitForWakeKind::Timer, true)).await;
}

#[tokio::test]
async fn operator_abort_exact_external_wait_preserves_independent_wait_and_timer() {
    Box::pin(exact_wake_abort_fixture(WaitForWakeKind::External, false)).await;
}

#[tokio::test]
async fn operator_abort_exact_external_wait_ack_crash_bootstrap_does_not_replay() {
    Box::pin(exact_wake_abort_fixture(WaitForWakeKind::External, true)).await;
}

#[derive(Clone, Default)]
struct AbortProviderRecovery {
    calls: Arc<AtomicUsize>,
    blocked: Arc<AbortReplayProvider>,
}

#[async_trait]
impl AgentProvider for AbortProviderRecovery {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return DeferredFallbackProvider.complete_turn(request).await;
        }
        self.blocked.complete_turn(request).await
    }

    fn select_model_lineage(
        &self,
        _: &crate::config::ModelRouteRef,
    ) -> Option<Arc<dyn AgentProvider>> {
        Some(Arc::new(self.clone()))
    }
}

async fn provider_recovery_abort_fixture(crash_after_ack: bool) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbortProviderRecovery::default());
    let runtime = open_runtime(home.path(), workspace.path(), provider.clone());
    let independent = IndependentAbortSnapshot::seed(&runtime).await;
    let root = runtime
        .enqueue(operator_prompt(
            "generate a real provider fallback recovery",
        ))
        .await
        .unwrap();
    let mut running = tokio::spawn(runtime.clone().run());
    await_abortable_provider(&provider.blocked, &mut running).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let source = runtime
        .storage()
        .read_recent_messages(16)
        .unwrap()
        .into_iter()
        .find(crate::runtime::turn::TurnModelSelection::message_has_provider_recovery_provenance)
        .expect("the failing provider generated a real fallback recovery");
    assert_eq!(source.causation_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(source.source_refs.get("source_message_id"), Some(&root.id));
    assert_eq!(queue_status(&runtime, &root), QueueEntryStatus::Processed);
    assert_eq!(queue_status(&runtime, &source), QueueEntryStatus::Dequeued);
    let execution = execution_protocol(&runtime);
    let original_attempt = execution.open_attempt().unwrap().clone();
    assert_eq!(
        original_attempt.source.identity,
        ExecutionSourceIdentity::RuntimeRecovery {
            recovery_id: source.id.clone(),
        }
    );
    assert!(matches!(
        original_attempt.binding,
        ExecutionBinding::Conversation { .. }
    ));
    let predecessor = execution.attempts[original_attempt
        .recovery_of_attempt_id
        .as_ref()
        .expect("real provider recovery must name its failed predecessor")]
    .clone();
    assert_eq!(
        predecessor.source_message_id.as_deref(),
        Some(root.id.as_str())
    );
    assert!(matches!(
        predecessor.state,
        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
    ));
    let predecessor_outcome = execution.outcomes[predecessor
        .terminal_outcome_id
        .as_ref()
        .expect("the original provider failure is already terminal")]
    .clone();
    let expected_outcome = ExecutionOutcome::Conversation(ConversationOutcome::Aborted {
        reason: "operator_aborted".into(),
    });
    abort_run(&runtime, CurrentRunAbortMode::StopAfterAbort).await;
    if crash_after_ack {
        crash_after_abort_ack(running).await;
    } else {
        join_stopped_run(running).await;
        assert_cancelled_attempt(
            &runtime,
            &source,
            &original_attempt,
            expected_outcome.clone(),
        );
        independent.assert_unchanged(&runtime).await;
    }
    let (runtime, _) =
        recover_quiescent_startup_fixture(runtime, home.path(), workspace.path()).await;
    assert_cancelled_attempt(
        &runtime,
        &source,
        &original_attempt,
        expected_outcome.clone(),
    );
    independent.assert_unchanged(&runtime).await;
    assert_start_cannot_replay(&runtime, &source).await;
    assert_cancelled_attempt(&runtime, &source, &original_attempt, expected_outcome);
    let recovered = execution_protocol(&runtime);
    assert_eq!(recovered.attempts[&predecessor.attempt_id], predecessor);
    assert_eq!(
        recovered.outcomes[&predecessor_outcome.outcome_id], predecessor_outcome,
        "canceling recovery must not rewrite the original provider failure"
    );
    assert_eq!(queue_status(&runtime, &root), QueueEntryStatus::Processed);
    assert_eq!(
        recovered
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(root.id.as_str()))
            .count(),
        1,
        "bootstrap must not reactivate the root input either"
    );
    independent.assert_unchanged(&runtime).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.blocked.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.blocked.active.load(Ordering::SeqCst), 0);
    assert_eq!(provider.blocked.peak_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operator_abort_provider_generated_recovery_preserves_failed_predecessor() {
    Box::pin(provider_recovery_abort_fixture(false)).await;
}

#[tokio::test]
async fn operator_abort_provider_generated_recovery_ack_crash_bootstrap_does_not_replay() {
    Box::pin(provider_recovery_abort_fixture(true)).await;
}
