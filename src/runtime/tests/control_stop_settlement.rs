use super::tests::support::*;
use super::*;
use crate::runtime_db::control_transition::{
    ControlCancellationKind, ControlObligationState, ControlStoppedRun,
};

struct BlockingStopProvider {
    started: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl AgentProvider for BlockingStopProvider {
    async fn complete_turn(&self, _: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.started.notify_one();
        std::future::pending().await
    }
}

fn prompt() -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "wait for lifecycle Stop".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    )
}
fn runtime_fixture(
    home: &std::path::Path,
    workspace: &std::path::Path,
    started: Arc<tokio::sync::Notify>,
) -> RuntimeHandle {
    let runtime = RuntimeHandle::new(
        "default",
        home.to_path_buf(),
        workspace.to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(BlockingStopProvider { started }),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime
        .runtime_db()
        .agent_identities()
        .upsert(&crate::types::AgentIdentityRecord::new(
            "default",
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))
        .unwrap();
    runtime
}
fn proof<'a>(
    runtime: &RuntimeHandle,
    source: &'a MessageEnvelope,
    lease: &'a control_cancellation::ControlRunLease,
) -> ControlStoppedRun<'a> {
    ControlStoppedRun {
        agent_id: &source.agent_id,
        identity_incarnation: runtime.inner.identity_incarnation,
        owner_epoch: runtime.inner.mutation_owner.owner_epoch(),
        run_id: lease.run_id(),
        runtime_lease_id: lease.lease_id(),
        activation_id: lease.activation_id(),
        source,
    }
}

#[tokio::test]
async fn blocking_provider_stop_keeps_original_canonical_terminal_and_joins_run() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime_fixture(home.path(), workspace.path(), started.clone());
    let source = runtime.enqueue(prompt()).await.unwrap();
    let source = runtime
        .storage()
        .read_message_by_id(&source.id)
        .unwrap()
        .unwrap();
    let mut running = tokio::spawn(runtime.clone().run());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = started.notified() => {},
            result = &mut running => panic!("run exited before provider start: {result:?}"),
        }
    })
    .await
    .unwrap();
    let run_id = runtime.agent_state().await.unwrap().current_run_id.unwrap();
    let lease = runtime
        .inner
        .control_cancellations
        .run_for(&run_id)
        .unwrap();
    let (key, invocation) =
        runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::RuntimeInstruction);
    runtime
        .control_command_admitted(
            key.clone(),
            invocation.clone(),
            runtime.execution_admission_lease().unwrap(),
        )
        .await
        .unwrap();
    let receipt = runtime
        .runtime_db()
        .control_receipt(&key, &invocation, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.outcome.aborted_run_id.as_deref(),
        Some(run_id.as_str())
    );
    let queue = runtime
        .storage()
        .latest_queue_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == source.id)
        .unwrap();
    assert_eq!(queue.status, QueueEntryStatus::Aborted);
    let turns = runtime.storage().read_recent_turns(16).unwrap();
    let original_turn = turns
        .iter()
        .find(|turn| turn.run_id.as_deref() == Some(run_id.as_str()))
        .unwrap();
    assert_eq!(
        original_turn.terminal.as_ref().unwrap().kind,
        TurnTerminalKind::Interrupted
    );
    assert_eq!(
        original_turn.terminal.as_ref().unwrap().reason.as_deref(),
        Some("agent_stopped")
    );
    assert!(runtime
        .runtime_db()
        .committed_control_stop_for_run(&proof(&runtime, &source, &lease))
        .unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.inner.control_cancellations.drain(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(lease.is_complete());
    assert_eq!(
        runtime
            .runtime_db()
            .control_receipt(&key, &invocation, None)
            .unwrap()
            .unwrap(),
        receipt
    );
    assert_eq!(runtime.storage().read_recent_turns(16).unwrap(), turns);
    assert_eq!(
        runtime
            .storage()
            .latest_queue_entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id == source.id)
            .unwrap(),
        queue
    );
    assert!(runtime
        .runtime_db()
        .pending_control_obligations(&key.agent_id, key.identity_incarnation)
        .unwrap()
        .is_empty());
    assert!(runtime
        .agent_state()
        .await
        .unwrap()
        .last_runtime_failure
        .is_none());
    assert!(!runtime
        .storage()
        .read_recent_events(128)
        .unwrap()
        .iter()
        .any(|event| event.kind == "agent_runtime_loop_failed" || event.kind == "runtime_error"));
}

#[tokio::test]
async fn claim_only_stop_has_original_attempt_proof_without_started_turn() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = runtime_fixture(
        home.path(),
        workspace.path(),
        Arc::new(tokio::sync::Notify::new()),
    );
    runtime.enqueue(prompt()).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("input must be claimed")
    };
    let run_id = scheduled.running_state.current_run_id.as_deref().unwrap();
    let lease = runtime.inner.control_cancellations.run_for(run_id).unwrap();
    assert!(runtime.storage().read_recent_turns(16).unwrap().is_empty());
    let (key, invocation) =
        runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::RuntimeInstruction);
    runtime
        .control_command_admitted(
            key.clone(),
            invocation,
            runtime.execution_admission_lease().unwrap(),
        )
        .await
        .unwrap();
    assert!(runtime
        .runtime_db()
        .committed_control_stop_for_run(&proof(&runtime, &scheduled.message, &lease))
        .unwrap());
    let (_, target) = runtime
        .runtime_db()
        .pending_control_obligations(&key.agent_id, key.identity_incarnation)
        .unwrap()
        .into_iter()
        .find(|(_, target)| target.target_kind == ControlCancellationKind::Run)
        .unwrap();
    assert_eq!(
        runtime
            .runtime_db()
            .control_obligation(&key, &target)
            .unwrap()
            .unwrap()
            .state,
        ControlObligationState::Pending
    );
    drop(scheduled);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.inner.control_cancellations.drain(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(lease.is_complete());
    assert_eq!(
        runtime
            .runtime_db()
            .control_obligation(&key, &target)
            .unwrap()
            .unwrap()
            .state,
        ControlObligationState::Completed
    );
}

#[tokio::test]
async fn arbitrary_aborted_queue_is_not_an_original_control_stop_proof() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = runtime_fixture(
        home.path(),
        workspace.path(),
        Arc::new(tokio::sync::Notify::new()),
    );
    runtime.enqueue(prompt()).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("input must be claimed")
    };
    let lease = runtime
        .inner
        .control_cancellations
        .run_for(scheduled.running_state.current_run_id.as_deref().unwrap())
        .unwrap();
    let mut queue = runtime
        .storage()
        .latest_queue_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == scheduled.message.id)
        .unwrap();
    queue.status = QueueEntryStatus::Aborted;
    queue.updated_at = Utc::now();
    runtime.runtime_db().queue_entries().upsert(&queue).unwrap();
    assert!(!runtime
        .runtime_db()
        .committed_control_stop_for_run(&proof(&runtime, &scheduled.message, &lease))
        .unwrap());
    assert_eq!(
        runtime
            .storage()
            .latest_queue_entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id == scheduled.message.id)
            .unwrap(),
        queue
    );
    drop(scheduled);
}

async fn deleting_service_shutdown_preserves_original_admission(fault: bool) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime_fixture(home.path(), workspace.path(), started.clone());
    let source = runtime.enqueue(prompt()).await.unwrap();
    let mut running = tokio::spawn(runtime.clone().run());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = started.notified() => {},
            result = &mut running => panic!("run exited before provider start: {result:?}"),
        }
    })
    .await
    .unwrap();
    let agent_id = runtime.instance_agent_id();
    let before = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)
        .unwrap()
        .unwrap();
    let attempt = before
        .attempts
        .values()
        .find(|attempt| attempt.source_message_id.as_deref() == Some(source.id.as_str()))
        .unwrap()
        .clone();
    assert!(before.outcomes.is_empty());
    let mut identity = runtime
        .runtime_db()
        .agent_identities()
        .latest(&agent_id)
        .unwrap()
        .unwrap();
    identity.status = crate::types::AgentRegistryStatus::Deleting;
    runtime
        .runtime_db()
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    if fault {
        runtime.inject_next_transition_fault(
            crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
        );
    }
    let shutdown = runtime.request_service_shutdown().await;
    if fault {
        assert!(shutdown.is_err());
    } else {
        shutdown.unwrap();
    }
    runtime.reconcile_posture_observation().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    runtime.join_idle_owned_tasks().await.unwrap();
    runtime.inner.control_cancellations.drain().await.unwrap();
    let after = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.attempts.len(),
        before.attempts.len(),
        "shutdown cannot admit a replacement attempt"
    );
    let settled = &after.attempts[&attempt.attempt_id];
    assert_ne!(
        settled.state,
        crate::domain::execution_protocol::ExecutionAttemptState::Open
    );
    assert!(
        settled.terminal_outcome_id.as_ref().is_some_and(|id| after
            .outcomes
            .get(id)
            .is_some_and(|outcome| outcome.attempt_id == attempt.attempt_id)),
        "original admitted cleanup must create and qualify its first Outcome"
    );
    let outcome_id = settled.terminal_outcome_id.as_ref().unwrap();
    assert!(runtime.runtime_db().connection().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='outcome' AND record_id=?3 AND availability='present')",
        rusqlite::params![agent_id, runtime.inner.identity_incarnation, outcome_id], |row| row.get::<_, bool>(0)).unwrap());
    assert!(runtime
        .storage()
        .read_recent_turns(16)
        .unwrap()
        .iter()
        .any(|turn| turn.terminal.is_some() && turn.input_message_ids.contains(&source.id)));
    assert_eq!(
        runtime
            .runtime_db()
            .agent_identities()
            .latest(&agent_id)
            .unwrap()
            .unwrap()
            .status,
        crate::types::AgentRegistryStatus::Deleting
    );
    assert!(runtime.inner.mutation_owner.capture().unwrap().is_some());
    assert!(
        runtime.enqueue(prompt()).await.is_err(),
        "retirement keeps new admission closed"
    );
}

#[tokio::test]
async fn deleting_service_shutdown_settles_original_provider_attempt_and_outcome() {
    deleting_service_shutdown_preserves_original_admission(false).await;
}

#[tokio::test]
async fn deleting_service_shutdown_known_publication_fault_preserves_original_cleanup() {
    deleting_service_shutdown_preserves_original_admission(true).await;
}
