use super::*;
use crate::runtime::mutation_owner::{
    MutationOwner, MutationOwnerError, MutationReservation, RecoveryPhase, WitnessUnknownResolution,
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
