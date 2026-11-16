use super::*;
use crate::runtime::mutation_owner::{
    MutationOwner, MutationOwnerError, MutationReservation, RecoveryPhase, WitnessUnknownResolution,
};
use crate::runtime::tests::support::{persist_task_transition, LifecycleHarness};
use crate::types::{
    AgentListObservationStatus, TaskKind, TaskRecord, TaskStatus, WorkItemPlanStatus,
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;

async fn state_response(app: &axum::Router, agent_id: &str) -> Result<(StatusCode, Value)> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/agents/{agent_id}/state"))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

fn assert_reconciliation_pending((status, body): &(StatusCode, Value)) {
    assert_eq!(*status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "agent_reconciliation_pending");
    assert_eq!(body["domain"], "runtime");
    assert_eq!(body["retryable"], true);
    assert!(body.get("observation").is_none(), "{body}");
    assert!(body.get("agent").is_none(), "{body}");
}

fn assert_historical_state((status, body): &(StatusCode, Value), before: &Value) {
    assert_eq!(*status, StatusCode::OK, "{body}");
    let mut observation = before["observation"].clone();
    observation["status"] = "reconciliation_pending".into();
    assert_eq!(body["observation"], observation);
    for field in ["agent", "tasks", "waits", "timers", "work_items"] {
        assert_eq!(body[field], before[field], "{field}");
    }
}

#[tokio::test]
async fn state_http_rejects_identity_change_and_rename_commit_windows() -> Result<()> {
    for rename in [false, true] {
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let run_loop = runtime.pause_run_loop_for_monitor_test().await;
        let refresh = runtime.hold_resource_refresh_for_test().await?;
        let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
        let agent_id = runtime.instance_agent_id();
        assert_eq!(state_response(&app, agent_id).await?.0, StatusCode::OK);
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        if rename {
            reservation.begin_identity_rename(
                "state-identity-rename-window".into(),
                root.identity.as_ref().unwrap().clone(),
            )?;
        } else {
            MutationReservation::begin_all_identity_change(
                std::slice::from_mut(&mut reservation),
                "state-identity-change-window".into(),
            )?;
        }
        let response =
            tokio::time::timeout(Duration::from_secs(2), state_response(&app, agent_id)).await;
        reservation.committed_prepared_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            root.tasks.clone(),
            root.scheduling.clone(),
            root.queue_posture,
            root.scheduler_events,
        )?;
        drop(reservation);
        assert_reconciliation_pending(&response??);
        let mut recovery = runtime.inner.mutation_owner.reserve().await?;
        let record = recovery.recovery()?.unwrap();
        assert_eq!(record.phase, RecoveryPhase::NeedsReconcile);
        assert!(record.known.is_some());
        let response =
            tokio::time::timeout(Duration::from_secs(2), state_response(&app, agent_id)).await??;
        assert_reconciliation_pending(&response);
        MutationReservation::reconcile_all(
            std::slice::from_mut(&mut recovery),
            vec![record.known],
        )?;
        drop(recovery);
        drop((refresh, run_loop));
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn state_http_observes_known_commit_awaiting_reconciliation_without_repair() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let agent_id = runtime.instance_agent_id();
    let (status, before) = state_response(&app, agent_id).await?;
    assert_eq!(status, StatusCode::OK);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let key = {
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        let key = reservation.begin("state-known-interrupted-publication".into())?;
        reservation.committed_prepared_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            root.tasks.clone(),
            root.scheduling.clone(),
            root.queue_posture,
            root.scheduler_events,
        )?;
        // Reads keep the preceding complete cut without acknowledging or
        // repairing this acknowledged but not yet published write.
        key
    };
    let mut recovery = runtime.inner.mutation_owner.reserve().await?;
    let record = recovery.recovery()?.unwrap();
    assert_eq!(record.key, key);
    assert_eq!(record.phase, RecoveryPhase::NeedsReconcile);
    assert!(record.known.is_some());
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    let frontier = runtime
        .inner
        .scheduler_audit_frontier
        .fetch_add(1, Ordering::AcqRel);
    for _ in 0..2 {
        let response =
            tokio::time::timeout(Duration::from_secs(2), state_response(&app, agent_id)).await??;
        assert_historical_state(&response, &before);
        let (observed, _) = runtime
            .inner
            .mutation_owner
            .capture_state_observation()?
            .unwrap();
        assert!(Arc::ptr_eq(&root, &observed));
        assert!(matches!(
            runtime.inner.mutation_owner.capture_with_token(),
            Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
        ));
    }
    runtime
        .inner
        .scheduler_audit_frontier
        .store(frontier, Ordering::Release);
    assert_eq!(
        recovery.recovery()?.unwrap().phase,
        RecoveryPhase::NeedsReconcile,
        "a State read must not reconcile the owner"
    );
    MutationReservation::reconcile_all(std::slice::from_mut(&mut recovery), vec![record.known])?;
    drop(recovery);
    assert_eq!(state_response(&app, agent_id).await?.0, StatusCode::OK);
    drop((refresh, run_loop));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn state_http_observes_unknown_original_commit_outcome_without_resolving_it() -> Result<()> {
    use crate::runtime::mutation_witness::MutationWitness;
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    use crate::runtime_db::input_mutation_receipts::{
        InputMutationFamily, OriginalInputMutationAttempt, PreparedInputMutation,
    };

    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let agent_id = runtime.instance_agent_id();
    let (status, before) = state_response(&app, agent_id).await?;
    assert_eq!(status, StatusCode::OK);
    let owner = runtime.inner.mutation_owner.clone();
    let token = owner.capture_with_token()?.0;
    let witness = MutationWitness::input_mutation(
        "state-unknown-original-outcome".into(),
        OriginalInputMutationAttempt::new(PreparedInputMutation::new(
            InputMutationFamily::Queue,
            "state-unknown-original-outcome".into(),
            vec![InputEvidenceScope {
                agent_id: agent_id.into(),
                identity_incarnation: runtime.inner.identity_incarnation,
            }],
            "a".repeat(64),
            runtime.now(),
        )?),
        Vec::new(),
    )?;
    let key = {
        let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
        MutationReservation::begin_all_with_witness(
            &mut reservations,
            "state-unknown-original-outcome".into(),
            witness.clone(),
        )?
        .remove(0)
    };
    let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
    let record = reservations[0].recovery()?.unwrap();
    assert_eq!(record.key, key);
    assert_eq!(record.phase, RecoveryPhase::NeedsReconcile);
    assert!(record.known.is_none() && record.committed_state.is_none());
    assert!(!witness.original_attempt_finished());
    let response =
        tokio::time::timeout(Duration::from_secs(2), state_response(&app, agent_id)).await??;
    assert_historical_state(&response, &before);
    assert!(!response
        .1
        .to_string()
        .contains("state-unknown-original-outcome"));
    assert!(!witness.original_attempt_finished());
    assert!(reservations[0].recovery()?.unwrap().known.is_none());
    assert!(matches!(
        owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));

    // Only the original producer can establish that no write occurred.
    witness.finish_original_attempt();
    assert_eq!(
        MutationReservation::resolve_input_unknown(&mut reservations, &key, &witness, None)?,
        WitnessUnknownResolution::NotCommitted
    );
    drop(reservations);
    assert_eq!(owner.capture_with_token()?.0, token);
    assert_eq!(state_response(&app, agent_id).await?.0, StatusCode::OK);
    drop((refresh, run_loop));
    host.shutdown().await?;
    Ok(())
}

async fn add_state_cut_records(
    runtime: &RuntimeHandle,
    suffix: &str,
) -> Result<(String, String, String)> {
    let work = runtime
        .create_work_item(
            format!("State observation cut {suffix}"),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    let task_id = format!("state-observation-task-{suffix}");
    persist_task_transition(
        runtime,
        &TaskRecord {
            id: task_id.clone(),
            agent_id: runtime.instance_agent_id().into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: runtime.now(),
            updated_at: runtime.now(),
            parent_message_id: None,
            work_item_id: Some(work.id.clone()),
            summary: Some(format!("State observation task {suffix}")),
            detail: None,
            recovery: None,
        },
        "state_observation_task_fixture",
    )
    .await?;
    let wait = runtime
        .register_wait_for(
            runtime.instance_agent_id(),
            Some(work.id),
            WaitForWakeKind::External,
            Some(format!("state-observation-external-{suffix}")),
            format!("State observation wait {suffix}"),
            None,
        )
        .await?;
    let timer = runtime
        .schedule_timer(
            3_600_000,
            None,
            Some(format!("State observation timer {suffix}")),
        )
        .await?;
    Ok((task_id, wait.condition.id, timer.id))
}

#[tokio::test]
async fn state_projection_keeps_task_wait_and_timer_records_on_one_published_cut() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (task_a, wait_a, timer_a) = add_state_cut_records(runtime, "first").await?;
    let generation = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .token
        .generation;
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    let key = reservation.begin("state-material-open-commit-window".into())?;
    let first = runtime.operator_state_read_projection(10, 10).await?;
    let observation = first.observation.as_ref().unwrap();
    assert_eq!(observation.status, AgentListObservationStatus::Committing);
    assert_eq!(observation.publication_generation, generation);
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    reservation.definitely_not_committed()?;
    drop(reservation);
    let first_records = serde_json::to_value((&first.tasks, &first.waits, &first.timers))?;
    assert_eq!(first.tasks.len(), 1);
    assert_eq!(first.tasks[0].id, task_a);
    assert_eq!(first.waits.len(), 1);
    assert_eq!(first.waits[0].id, wait_a);
    assert_eq!(first.timers.len(), 1);
    assert_eq!(first.timers[0].id, timer_a);

    let (task_b, wait_b, timer_b) = add_state_cut_records(runtime, "second").await?;
    let second = runtime.operator_state_read_projection(10, 10).await?;
    assert!(
        second.observation.as_ref().unwrap().publication_generation > generation,
        "later domain publication must advance the State generation"
    );
    assert_eq!(second.tasks.len(), 2);
    assert!(second.tasks.iter().any(|task| task.id == task_a));
    assert!(second.tasks.iter().any(|task| task.id == task_b));
    assert_eq!(second.agent.active_task_count, 2);
    assert_eq!(second.waits.len(), 2);
    assert!(second.waits.iter().any(|wait| wait.id == wait_a));
    assert!(second.waits.iter().any(|wait| wait.id == wait_b));
    assert_eq!(second.timers.len(), 2);
    assert!(second.timers.iter().any(|timer| timer.id == timer_a));
    assert!(second.timers.iter().any(|timer| timer.id == timer_b));
    assert_eq!(second.active_timer_count, 2);
    assert_eq!(
        serde_json::to_value((&first.tasks, &first.waits, &first.timers))?,
        first_records,
        "later domain publication must not splice into an older State view"
    );
    Ok(())
}

#[tokio::test]
async fn state_wire_material_releases_after_last_wire_slice_and_detached_timer_loan() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    runtime
        .schedule_timer(
            3_600_000,
            None,
            Some("State observation last holder".into()),
        )
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    let generation = root.token.generation;
    drop(root);
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    let key = reservation.begin("state-wire-release-open-commit-window".into())?;
    let projection = runtime.operator_state_read_projection(10, 10).await?;
    let observation = projection.observation.as_ref().unwrap();
    assert_eq!(observation.status, AgentListObservationStatus::Committing);
    assert_eq!(observation.publication_generation, generation);
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    reservation.definitely_not_committed()?;
    drop(reservation);
    let guard = projection.material.acquisition.guard()?;
    let material = Arc::downgrade(&guard);
    drop(guard);
    let timer_loan = projection.timers[0].clone();
    let bytes = projection
        .material
        .into_json_bytes(&projection.agent.agent)?;
    let cloned = bytes.clone();
    let sliced = bytes.slice(1..bytes.len() - 1);
    drop(bytes);
    drop((
        projection.agent,
        projection.tasks,
        projection.waits,
        projection.timers,
        projection.external_triggers,
    ));
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({}),
        ))
        .await?;
    assert!(old_root.upgrade().is_none());
    assert!(material.upgrade().is_some());
    drop(cloned);
    assert!(material.upgrade().is_some(), "the wire slice remains live");
    drop(sliced);
    assert!(
        material.upgrade().is_some(),
        "the detached Timer loan remains live"
    );
    drop(timer_loan);
    assert!(
        material.upgrade().is_none(),
        "the last holder releases material"
    );
    Ok(())
}
