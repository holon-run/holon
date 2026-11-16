use super::*;

async fn healthy_admission() -> (tempfile::TempDir, RuntimeHandle, Arc<ProviderTurnAdmission>) {
    let (home, runtime) = runtime();
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy operator input must schedule");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    assert!(admission.allows_auxiliary_dispatch().unwrap());
    (home, runtime, admission)
}

#[tokio::test]
async fn healthy_auxiliary_dispatch_charges_without_a_probe_and_retains_the_main_budget() {
    let (_home, _runtime, admission) = healthy_admission().await;
    assert!(admission.before_auxiliary_dispatch().unwrap());
    assert!(!admission
        .state
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .unwrap()
        .is_probe());
    assert_eq!(admission.episode().unwrap().total_requests, 1);
    admission.after_request(Ok(None)).unwrap();
    admission.before_dispatch().unwrap();
    assert_eq!(admission.episode().unwrap().total_requests, 2);
}

#[tokio::test]
async fn auxiliary_dispatch_skips_a_concurrent_incident_without_spending_the_main_probe() {
    for server_floor in [false, true] {
        let (_home, runtime, admission) = healthy_admission().await;
        let concurrent = stale_admission::concurrent_lease(&runtime, &admission);
        assert!(runtime
            .runtime_db()
            .record_provider_admission_failure(
                &concurrent,
                &admission.candidate.connection,
                ProviderCooldownPolicy::Transport,
                runtime.now(),
                server_floor.then(|| runtime.now() + Duration::minutes(10)),
                &admission.candidate.model.as_string(),
            )
            .unwrap());
        admission.declare_adapter_correctable_dispatch();
        admission.declare_auth_refreshable_dispatch();
        assert!(!admission.before_auxiliary_dispatch().unwrap());
        {
            let state = admission.state.lock().unwrap();
            assert!(!state.dispatched);
            assert!(!state.unknown_dispatch);
            assert!(state.lease.is_none());
            assert!(state.dispatch.is_none());
            assert!(!state.adapter_correctable_dispatch);
            assert!(!state.auth_refreshable_dispatch);
        }
        assert_eq!(admission.episode().unwrap().total_requests, 0);
        assert_eq!(admission.episode().unwrap().request_count, 0);
        let probe: Option<String> = runtime
            .runtime_db()
            .with_read(|connection| {
                Ok(connection.query_row(
                    "SELECT probe_token FROM provider_transport_incidents
                     WHERE identity_key = ?1",
                    [admission.candidate.connection.key()],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(
            probe.is_none(),
            "skipped auxiliary reservation must roll back"
        );
        if server_floor {
            let error = admission.before_dispatch().unwrap_err();
            assert!(matches!(
                dispatch_denial(&error).map(|denial| denial.0),
                Some(LocalDispatchDenial::ScopeCooling { .. })
            ));
            assert_eq!(admission.episode().unwrap().total_requests, 0);
        } else {
            admission.before_dispatch().unwrap();
            assert!(admission
                .state
                .lock()
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .is_probe());
            assert_eq!(admission.episode().unwrap().total_requests, 1);
            assert!(admission.before_dispatch().is_err());
            assert_eq!(admission.episode().unwrap().total_requests, 1);
        }
    }
}

#[tokio::test]
async fn auxiliary_dispatch_propagates_input_fence_rejection() {
    let (_home, _runtime, admission) = healthy_admission().await;
    admission
        .fence_operator_interjections(Arc::new(std::sync::atomic::AtomicU64::new(1)), 0)
        .unwrap();
    let error = admission.before_auxiliary_dispatch().unwrap_err();
    assert!(matches!(
        dispatch_denial(&error).map(|denial| denial.0),
        Some(LocalDispatchDenial::InputChanged)
    ));
    assert_eq!(admission.episode().unwrap().total_requests, 0);
}
