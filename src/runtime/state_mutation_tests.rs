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
        let mut agent = runtime.inner.agent.lock().await;
        agent.state.status = AgentStatus::AwakeRunning;
        agent.state.current_run_id = Some("stale-run".into());
        agent.persist_state(&runtime.inner.storage).unwrap();
    }
    (runtime, data, workspace)
}

#[tokio::test]
async fn publication_failure_recovers_without_rewriting_or_repeating_audit() {
    let (runtime, _data, _workspace) = stale_runtime().await;
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
        1
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
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    runtime.reconcile_posture_observation().await.unwrap();
    assert_eq!(runtime.inner.agent.lock().await.state, committed);
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
