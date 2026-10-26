use super::super::{
    mutation_owner::MutationOwnerError,
    scheduler_executor::{SchedulerDecisionExecutor, StaleRunProjectionBoundary},
};
use super::*;
use crate::runtime::tests::support::{context_config, CountingProvider};
use tempfile::{tempdir, TempDir};

async fn stale_runtime() -> (RuntimeHandle, TempDir, TempDir) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_owned(),
        workspace.path().to_owned(),
        "http://127.0.0.1:7878".into(),
        Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "unused",
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut agent = runtime.begin_mutation().await.unwrap();
        agent.state.status = AgentStatus::AwakeRunning;
        agent.state.current_run_id = Some("stale-run".into());
        agent.persist_state(&runtime.inner.storage).await.unwrap();
    }
    (runtime, data, workspace)
}

#[tokio::test]
async fn publication_failure_recovers_without_rewriting_or_repeating_audit() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let (_births, current_session) = runtime.begin_task_birth_session().await.unwrap();
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("committed but publication"));
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let committed = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(committed.status, AgentStatus::AwakeIdle);
    assert_eq!(
        runtime
            .inner
            .agent
            .lock()
            .await
            .state
            .current_run_id
            .as_deref(),
        Some("stale-run")
    );
    let events = runtime.storage().read_recent_events(100).unwrap();
    runtime.reconcile_posture_observation().await.unwrap();
    let published = runtime.inner.mutation_owner.capture().unwrap().unwrap();
    assert_eq!(published.state.as_ref(), &committed);
    assert_eq!(runtime.inner.agent.lock().await.state, committed);
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
    let current = runtime.capture_run_once(&current_session).unwrap();
    assert_eq!(
        current
            .events()
            .filter(|event| event.kind == "scheduler_stale_run_projection_recovered")
            .count(),
        1
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        committed
    );
}

#[tokio::test]
async fn unknown_operation_stays_fenced_and_does_not_replay_against_latest_state() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    {
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation
            .begin("unknown-original-operation".into())
            .unwrap();
    }
    let before = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    let events = runtime.storage().read_recent_events(100).unwrap();
    let error = SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("outcome is unknown"));
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_request_during_sql_wait_keeps_owned_commit_and_publication() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let blocker = rusqlite::Connection::open(runtime.inner.runtime_db.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            SchedulerDecisionExecutor::new(&runtime)
                .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                runtime.inner.mutation_owner.capture(),
                Err(MutationOwnerError::NeedsReconcile(_))
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker must close its gate before entering blocked SQL");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    blocker.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(Duration::from_secs(5), runtime.join_idle_owned_tasks())
        .await
        .unwrap()
        .unwrap();
    let observation = runtime.inner.mutation_owner.capture().unwrap().unwrap();
    let durable = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(observation.state.as_ref(), &durable);
    assert_eq!(durable.status, AgentStatus::AwakeIdle);
    assert_eq!(runtime.inner.agent.lock().await.state, durable);
    assert_eq!(
        runtime
            .storage()
            .read_recent_events(100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "scheduler_stale_run_projection_recovered")
            .count(),
        1
    );
}

#[tokio::test]
async fn concurrent_stale_recovery_requests_commit_only_once() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let baseline_generation = runtime
        .inner
        .mutation_owner
        .capture()
        .unwrap()
        .unwrap()
        .token
        .generation;
    let executor = SchedulerDecisionExecutor::new(&runtime);
    let (first, second) = tokio::join!(
        executor.reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll),
        executor.reconcile_stale_run_projection(StaleRunProjectionBoundary::WakeHintSubmission)
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(
        runtime
            .inner
            .mutation_owner
            .capture()
            .unwrap()
            .unwrap()
            .token
            .generation,
        baseline_generation + 1
    );
    assert_eq!(
        runtime
            .storage()
            .read_recent_events(100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "scheduler_stale_run_projection_recovered")
            .count(),
        1
    );
}

#[tokio::test]
async fn runtime_failure_cleanup_cannot_overwrite_a_committed_unpublished_posture() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    let committed = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    runtime.record_runtime_loop_failure(&error).await;
    let cleaned = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(cleaned.status, committed.status);
    assert_eq!(cleaned.current_run_id, committed.current_run_id);
    assert!(cleaned.last_runtime_failure.is_some());
    assert_eq!(runtime.inner.agent.lock().await.state, cleaned);
    assert_eq!(
        runtime
            .inner
            .mutation_owner
            .capture()
            .unwrap()
            .unwrap()
            .state
            .as_ref(),
        &cleaned
    );
}

#[tokio::test]
async fn mutation_drain_closes_registration_and_waits_for_every_task_after_a_failure() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    runtime
        .spawn_owned_mutation(async {
            panic!("injected mutation worker failure");
        })
        .unwrap();
    let (release, pending) = tokio::sync::oneshot::channel();
    runtime
        .spawn_owned_mutation(async move {
            pending.await.unwrap();
        })
        .unwrap();
    let mut draining = Box::pin(runtime.join_owned_mutations());
    assert!(futures_util::poll!(&mut draining).is_pending());
    let mut concurrent_drain = Box::pin(runtime.join_owned_mutations());
    assert!(futures_util::poll!(&mut concurrent_drain).is_pending());
    assert!(runtime
        .spawn_owned_mutation(async {
            panic!("must never be spawned");
        })
        .is_err());
    release.send(()).unwrap();
    assert!(draining.await.is_err());
    concurrent_drain.await.unwrap();
    assert!(runtime
        .inner
        .mutation_tasks
        .lock()
        .unwrap()
        .handles
        .is_empty());
}

#[tokio::test]
async fn cancelled_drainer_keeps_pending_handles_for_the_next_drainer() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let (release, pending) = tokio::sync::oneshot::channel();
    runtime
        .spawn_owned_mutation(async move {
            pending.await.unwrap();
        })
        .unwrap();
    let drainer = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.join_owned_mutations().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let moved = {
                let tasks = runtime.inner.mutation_tasks.lock().unwrap();
                tasks.closed && tasks.handles.is_empty()
            };
            if moved {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drainer.abort();
    assert!(drainer.await.unwrap_err().is_cancelled());
    assert!(!runtime.inner.mutation_tasks.lock().unwrap().all_finished());
    let mut next = Box::pin(runtime.join_owned_mutations());
    assert!(futures_util::poll!(&mut next).is_pending());
    release.send(()).unwrap();
    next.await.unwrap();
}

#[tokio::test]
async fn legacy_shutdown_cannot_overwrite_a_committed_unpublished_posture() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    let committed = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(committed.status, AgentStatus::AwakeIdle);
    runtime.request_service_shutdown().await.unwrap();
    let cleaned = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(cleaned.status, committed.status);
    assert_eq!(cleaned.current_run_id, committed.current_run_id);
    assert_eq!(runtime.inner.agent.lock().await.state, cleaned);
    assert_eq!(
        runtime
            .inner
            .mutation_owner
            .capture()
            .unwrap()
            .unwrap()
            .state
            .as_ref(),
        &cleaned
    );
}

#[tokio::test]
async fn legacy_force_write_cannot_bypass_an_unknown_posture_operation() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    {
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation
            .begin("unknown-before-legacy-force-write".into())
            .unwrap();
    }
    let before = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    {
        let mut agent = runtime.inner.agent.lock().await;
        agent.state.status = AgentStatus::Stopped;
        agent.state.pending = 42;
        let error = agent.persist_state(&runtime.inner.storage).unwrap_err();
        assert!(error.downcast_ref::<MutationOwnerError>().is_some());
        assert_eq!(agent.state, agent.last_persisted_state);
    }
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        before
    );
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
}

#[tokio::test]
async fn bootstrap_restores_known_posture_before_legacy_force_writes() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    runtime
        .inner
        .shutdown_requested
        .store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), runtime.clone().run())
        .await
        .unwrap()
        .unwrap();
    assert!(runtime.inner.mutation_owner.capture().unwrap().is_some());
    let persisted = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, AgentStatus::AwakeIdle);
    assert_eq!(runtime.inner.agent.lock().await.state, persisted);
}

#[tokio::test]
async fn transient_abort_intent_is_private_until_explicit_install() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let token = CancellationToken::new();
    let reason = Arc::new(StdMutex::new("running".to_owned()));
    {
        let mut agent = runtime.inner.agent.lock().await;
        agent.current_run_abort = Some(CurrentRunAbortHandle {
            run_id: "stale-run".into(),
            token: token.clone(),
            reason: reason.clone(),
        });
    }
    {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        mutation.defer_abort_current_run("operator_aborted");
        assert!(!token.is_cancelled());
        assert_eq!(*reason.lock().unwrap(), "running");
    }
    assert!(!token.is_cancelled());
    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation.defer_abort_current_run("operator_aborted");
    mutation
        .persist_state(&runtime.inner.storage)
        .await
        .unwrap();
    assert!(token.is_cancelled());
    assert_eq!(*reason.lock().unwrap(), "operator_aborted");
}

#[tokio::test]
async fn whole_row_no_change_does_not_borrow_an_external_ack_or_install_candidate() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let mut mutation = runtime.begin_mutation().await.unwrap();
    let before = mutation.state.clone();
    let mut externally_persisted = before.clone();
    externally_persisted.total_input_tokens += 17;
    runtime
        .storage()
        .write_agent_for_fixture(&externally_persisted)
        .unwrap();
    mutation.state = externally_persisted.clone();
    mutation.queue.clear();
    let error = mutation
        .persist_state(&runtime.inner.storage)
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .is_some());
    assert_eq!(runtime.inner.agent.lock().await.state, before);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        externally_persisted
    );
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    drop(mutation);
    assert!(runtime
        .refresh_enqueue_agent_state_baseline("default")
        .await
        .unwrap());
    assert_eq!(runtime.inner.agent.lock().await.state, externally_persisted);
}

#[tokio::test]
async fn candidate_identity_change_is_rejected_before_sql_and_keeps_gate_open() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let before = runtime.inner.agent.lock().await.state.clone();
    let events = runtime.storage().read_recent_events(100).unwrap();
    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation.state.id = "other-agent".into();
    let error = mutation.persist_state(runtime.storage()).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("cannot write agent state for `other-agent`"));
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    assert_eq!(runtime.inner.agent.lock().await.state, before);
    assert!(runtime
        .runtime_db()
        .agent_states()
        .latest("other-agent")
        .unwrap()
        .is_none());
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
}

#[tokio::test]
async fn cold_metadata_group_recovers_without_inventing_a_child_state_or_repeating_sql() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let child = runtime
        .inner
        .standalone_related_mutation_owners
        .current_owner("cold-child", 1)
        .unwrap();
    let mut mutation = runtime
        .begin_mutation_with_owners(vec![runtime.inner.mutation_owner.clone(), child.clone()])
        .await
        .unwrap();
    mutation.state.total_output_tokens += 23;
    let expected = mutation.state.clone();
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = mutation.persist_state(runtime.storage()).await.unwrap_err();
    assert!(
        matches!(
            runtime.inner.mutation_owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ),
        "expected a committed publication fault, got {error:#}"
    );
    assert!(matches!(
        child.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let child_recovery = mutation
        .reservations
        .iter()
        .find(|scope| scope.agent_id() == "cold-child")
        .unwrap()
        .recovery()
        .unwrap()
        .unwrap();
    assert!(
        matches!(child_recovery.known, Some(KnownCommit::Metadata(ref facts)) if facts.state.is_none())
    );
    drop(mutation);
    let events = runtime.storage().read_recent_events(100).unwrap();
    let recovered = runtime.begin_mutation().await.unwrap();
    assert_eq!(recovered.state, expected);
    drop(recovered);
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    assert!(child.capture().unwrap().is_none());
    assert!(runtime
        .runtime_db()
        .agent_states()
        .latest("cold-child")
        .unwrap()
        .is_none());
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
}

#[tokio::test]
async fn readonly_group_member_with_orphan_state_rejects_before_primary_commit() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let before = runtime.agent_state().await.unwrap();
    let child = runtime
        .inner
        .standalone_related_mutation_owners
        .current_owner("cold-child", 1)
        .unwrap();
    let orphan = AgentState::new("cold-child");
    runtime
        .runtime_db()
        .agent_states()
        .upsert_for_fixture(&orphan)
        .unwrap();
    let mut mutation = runtime
        .begin_mutation_with_owners(vec![runtime.inner.mutation_owner.clone(), child.clone()])
        .await
        .unwrap();
    mutation.state.total_output_tokens += 23;
    let error = mutation.persist_state(runtime.storage()).await.unwrap_err();
    assert!(format!("{error:#}").contains("State without a durable identity"));
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap(),
        Some(before)
    );
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("cold-child")
            .unwrap(),
        Some(orphan)
    );
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    assert!(child.capture().unwrap().is_none());
}

#[tokio::test]
async fn unknown_group_keeps_parent_and_absent_child_fenced() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let child = runtime
        .inner
        .standalone_related_mutation_owners
        .current_owner("cold-child", 1)
        .unwrap();
    let mut mutation = runtime
        .begin_mutation_with_owners(vec![runtime.inner.mutation_owner.clone(), child.clone()])
        .await
        .unwrap();
    let before = mutation.state.clone();
    let error = mutation
        .commit_transition("unknown-group".into(), |_scope| {
            Err(anyhow!("lost acknowledgement"))
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lost acknowledgement"));
    assert!(!mutation.reconcile_known_commit().await.unwrap());
    drop(mutation);
    assert!(runtime
        .reconcile_posture_observation()
        .await
        .unwrap_err()
        .to_string()
        .contains("outcome is unknown"));
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        child.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_eq!(runtime.inner.agent.lock().await.state, before);
}

#[tokio::test]
async fn definitely_rolled_back_group_opens_all_scopes_without_installing_candidate() {
    let (runtime, _data, _workspace) = stale_runtime().await;
    let child = runtime
        .inner
        .standalone_related_mutation_owners
        .current_owner("cold-child", 1)
        .unwrap();
    let mut mutation = runtime
        .begin_mutation_with_owners(vec![runtime.inner.mutation_owner.clone(), child.clone()])
        .await
        .unwrap();
    let before = mutation.state.clone();
    mutation.state.total_output_tokens += 29;
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCommit);
    mutation.persist_state(runtime.storage()).await.unwrap_err();
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    assert!(child.capture().unwrap().is_none());
    assert_eq!(runtime.inner.agent.lock().await.state, before);
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn acknowledged_input_projection_error_keeps_known_state_and_original_effects() {
    use super::super::resident_input_evidence::{
        CommittedEvidenceRemoval, InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope,
    };
    let (runtime, _data, _workspace) = stale_runtime().await;
    let mut mutation = runtime.begin_mutation().await.unwrap();
    assert!(mutation.input_evidence.is_some());
    let previous = mutation.last_persisted_state.clone();
    let mut next = previous.clone();
    next.total_output_tokens += 11;
    let event = AuditEvent::legacy("input_projection_error_fixture", serde_json::json!({}));
    let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
        agent_id: next.id.clone(),
        expected_incarnation: Some(runtime.inner.identity_incarnation),
        agent_state: crate::runtime_db::transitions::AgentStateMutation {
            expected: Some(Box::new(previous)),
            record: Box::new(next.clone()),
        },
        audit_events: vec![event.clone()],
        fault: None,
    };
    let error = mutation
        .commit_transition(event.id.clone(), |scope| {
            let mut committed = runtime
                .inner
                .runtime_db
                .transitions_in_scope(scope)
                .commit_agent_posture(&command)?;
            committed
                .effects
                .input_evidence_delta
                .removals
                .push(CommittedEvidenceRemoval {
                    scope: InputEvidenceScope {
                        agent_id: "foreign-input".into(),
                        identity_incarnation: 1,
                    },
                    key: InputEvidenceKey::new(InputEvidenceFamily::Message, "never-installed"),
                    revision: 1,
                    previous_content_digest: None,
                });
            Ok(committed)
        })
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("committed; input publication requires preparation"));
    let recovery = mutation.reservations[mutation.primary_index]
        .recovery()
        .unwrap()
        .unwrap();
    let Some(KnownCommit::State {
        state,
        input_evidence: None,
        runtime: Some(facts),
        ..
    }) = recovery.known
    else {
        panic!("an acknowledged write must retain Known facts even when input projection fails");
    };
    assert_eq!(state.as_ref(), &next);
    assert_eq!(facts.effects.audit_events[0].id, event.id);
    drop(mutation);
    let events = runtime.storage().read_recent_events(100).unwrap();
    runtime.reconcile_posture_observation().await.unwrap();
    let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
    assert_eq!(root.state.as_ref(), &next);
    assert!(
        root.input_evidence.is_none(),
        "failed derived data remains explicitly unprepared"
    );
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap(),
        Some(next)
    );
}
