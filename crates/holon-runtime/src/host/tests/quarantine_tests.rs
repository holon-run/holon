use super::*;
use crate::{daemon::read_quarantine_report, startup::StartupObservation};

fn reject_constructor(host: &RuntimeHost, agent_id: &str) {
    let incarnation = host.active_agent_identity(agent_id).unwrap().incarnation;
    host.inner.test_constructor_outcomes.lock().unwrap().insert(
        agent_id.to_owned(),
        crate::runtime::AgentConstructorQuarantine {
            agent_id: agent_id.to_owned(),
            incarnation,
        }
        .into(),
    );
}

#[tokio::test]
async fn quarantine_startup_recovery_continues_past_isolated_candidate() {
    let (_home, host) = canonical_test_host();
    for id in ["a-healthy", "b-isolated", "c-healthy"] {
        seed_observed_startup_owner(&host, id).await;
    }
    let host = restart_fixture_host(host).await;
    let observation = StartupObservation::with_id("startup_synthetic_recovery".into());
    host.begin_quarantine_report(&observation).await;
    let before = host
        .agent_storage("b-isolated")
        .unwrap()
        .latest_task_record("task-b-isolated")
        .unwrap();
    reject_constructor(&host, "b-isolated");
    host.recover_startup_observed(&observation).await.unwrap();
    for id in ["a-healthy", "c-healthy"] {
        assert!(host.try_get_loaded_runtime(id).await.is_some(), "{id}");
    }
    assert!(host.try_get_loaded_runtime("b-isolated").await.is_none());
    let after = host
        .agent_storage("b-isolated")
        .unwrap()
        .latest_task_record("task-b-isolated")
        .unwrap();
    assert_eq!(
        before.unwrap().status,
        after.unwrap().status,
        "skipped constructor must not fabricate a task transition"
    );
    let report = serde_json::to_value(read_quarantine_report(host.config().as_ref())).unwrap();
    assert_eq!(report["status"], "available", "{report}");
    assert_eq!(
        report["summary"]["current"]["startup_id"],
        "startup_synthetic_recovery"
    );
    assert_eq!(
        report["summary"]["current"]["entries"][0]["agent_id"],
        "b-isolated"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn quarantine_default_first_activation_can_bind_but_unknown_failure_cannot() {
    for (isolated, already_isolated) in [(true, false), (true, true), (false, false)] {
        let (_home, host) = canonical_test_host();
        let agent_id = host.config().default_agent_id.clone();
        let previous = StartupObservation::with_id("startup_previous_default".into());
        host.begin_quarantine_report(&previous).await;
        host.publish_quarantine_report().await;
        let host = restart_fixture_host(host).await;
        let observation = StartupObservation::with_id("startup_synthetic_default".into());
        host.begin_quarantine_report(&observation).await;
        host.recover_startup_observed(&observation).await.unwrap();
        assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
        let bind_observed = Arc::new(AtomicBool::new(false));
        let observation = observation.with_test_hook({
            let bind_observed = bind_observed.clone();
            Arc::new(move |_, phase| {
                if phase == "bind" {
                    bind_observed.store(true, Ordering::SeqCst);
                }
                Box::pin(async { Ok(()) })
            })
        });
        if isolated {
            reject_constructor(&host, &agent_id);
            if already_isolated {
                let error = host.default_runtime().await.err().unwrap();
                assert!(error.downcast_ref::<AgentQuarantined>().is_some());
            }
        } else {
            host.inner.test_constructor_outcomes.lock().unwrap().insert(
                agent_id.clone(),
                anyhow!("synthetic unclassified constructor failure"),
            );
        }
        let listener = host
            .bind_primary_for_serve(&observation, "127.0.0.1:0")
            .await;
        assert_eq!(bind_observed.load(Ordering::SeqCst), isolated);
        if listener.is_ok() {
            host.publish_quarantine_report().await;
        }
        let report = serde_json::to_value(read_quarantine_report(host.config().as_ref())).unwrap();
        assert_eq!(report["status"], "available", "{report}");
        let entries = report["summary"]["current"]["entries"].as_array().unwrap();
        if isolated {
            assert!(listener.unwrap().local_addr().unwrap().ip().is_loopback());
            assert_eq!(
                report["summary"]["current"]["startup_id"],
                observation.startup_id
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["agent_id"], agent_id);
        } else {
            assert!(listener.unwrap_err().to_string().contains("unclassified"));
            assert_eq!(
                report["summary"]["current"]["startup_id"],
                previous.startup_id
            );
            assert!(entries.is_empty());
        }
        assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn quarantine_retries_only_on_demand_and_serializes_concurrent_requests() {
    let (_home, host) = canonical_test_host();
    let deadline = Duration::from_secs(15);
    let agent_id = host.config().default_agent_id.clone();
    let incarnation = host.active_agent_identity(&agent_id).unwrap().incarnation;
    let demand = RuntimeActivationReason::OperatorControl;
    reject_constructor(&host, &agent_id);
    let error = host.activate_agent(&agent_id, demand).await.err().unwrap();
    assert!(error.downcast_ref::<AgentQuarantined>().is_some());

    reject_constructor(&host, &agent_id);
    let generation = host.inner.runtimes.read().await.next_generation;
    let error = host.activate_agent(&agent_id, demand).await.err().unwrap();
    assert!(error.downcast_ref::<AgentQuarantined>().is_some());
    assert_eq!(host.inner.runtimes.read().await.next_generation, generation);

    // Backdate only the synthetic host admission record, not the storage fixture.
    async fn expire(host: &RuntimeHost, agent_id: &str, incarnation: u64) {
        let mut registry = host.inner.runtimes.write().await;
        let generation = registry.next_generation - 1;
        registry.quarantines.record(
            agent_id,
            incarnation,
            generation,
            crate::daemon::QuarantineStage::StorageConstructor,
            crate::daemon::QuarantineReason::ProjectionChanged,
            tokio::time::Instant::now() - Duration::from_secs(15),
        );
    }
    expire(&host, &agent_id, incarnation).await;
    let error = host
        .activate_agent(&agent_id, RuntimeActivationReason::SchedulerDispatch)
        .await
        .err()
        .unwrap();
    assert!(error.downcast_ref::<AgentQuarantined>().is_some());

    let (first, second) = tokio::time::timeout(deadline, async {
        tokio::join!(
            host.activate_agent(&agent_id, demand),
            host.activate_agent(&agent_id, demand),
        )
    })
    .await
    .expect("concurrent requests did not settle after the synthetic rejection");
    for result in [first, second] {
        assert!(result
            .err()
            .unwrap()
            .downcast_ref::<AgentQuarantined>()
            .is_some());
    }
    assert_eq!(
        host.inner.runtimes.read().await.next_generation,
        generation + 1
    );
    assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());

    expire(&host, &agent_id, incarnation).await;
    tokio::time::timeout(deadline, host.drain_runtime_operations())
        .await
        .expect("previous activation requests did not drain");
    // The default agent bypasses bootstrap readiness. Hold the handoff lock
    // to prevent the admitted retry from reaching construction.
    let handoff = host.inner.bootstrap_handoff_lock.lock().await;
    let activation_host = host.clone();
    let activation_id = agent_id.clone();
    let activation =
        tokio::spawn(async move { activation_host.activate_agent(&activation_id, demand).await });
    wait_for_runtime_operation_admission(&host, 1).await;
    activation.abort();
    assert!(activation.await.err().unwrap().is_cancelled());
    tokio::time::timeout(deadline, host.drain_runtime_operations())
        .await
        .expect("cancelled activation did not drain with the handoff held");
    drop(handoff);
    assert_eq!(
        host.inner.runtimes.read().await.next_generation,
        generation + 1
    );

    let error = host
        .activate_agent_observed_for_incarnation(
            &agent_id,
            RuntimeActivationReason::ExternalIngress,
            None,
            Some(incarnation + 1),
        )
        .await
        .err()
        .unwrap();
    assert!(error.downcast_ref::<RuntimeError>().is_some());
    assert_eq!(
        host.inner.runtimes.read().await.next_generation,
        generation + 1
    );

    let (first, second) = tokio::time::timeout(deadline, async {
        tokio::join!(
            host.activate_agent(&agent_id, demand),
            host.activate_agent(&agent_id, demand),
        )
    })
    .await
    .expect("concurrent requests did not settle after successful construction");
    let first = first.unwrap();
    let second = second.unwrap();
    tokio::time::timeout(deadline, async {
        first.wait_for_bootstrap().await.unwrap();
        second.wait_for_bootstrap().await.unwrap();
    })
    .await
    .expect("installed runtime did not finish bootstrap");
    assert_eq!(
        host.inner.runtimes.read().await.next_generation,
        generation + 2
    );
    assert!(!host.agent_is_quarantined(&agent_id, incarnation).await);
    tokio::time::timeout(deadline, host.shutdown())
        .await
        .expect("host did not shut down")
        .unwrap();
}

fn local_missing_start_hook(
    agent_id: &str,
    incarnation: u64,
) -> Arc<crate::startup::StartupTestHook> {
    let agent_id = agent_id.to_owned();
    Arc::new(move |observation, phase| {
        let agent_id = agent_id.clone();
        Box::pin(async move {
            if phase == "runtime.bootstrap.tasks"
                && observation.agent_id.as_deref() == Some(agent_id.as_str())
            {
                return Err(crate::runtime::AgentLocalMissingStart {
                    agent_id,
                    incarnation,
                }
                .into());
            }
            Ok(())
        })
    })
}

#[tokio::test]
async fn quarantine_startup_recovery_isolates_certified_local_missing_start() {
    let (_home, host) = canonical_test_host();
    for id in ["a-healthy", "b-local-missing", "c-healthy"] {
        seed_observed_startup_owner(&host, id).await;
    }
    let isolated = "b-local-missing";
    let incarnation = host.active_agent_identity(isolated).unwrap().incarnation;
    let host = restart_fixture_host(host).await;
    let observation = StartupObservation::with_id("startup_local_missing_start".into())
        .with_test_hook(local_missing_start_hook(isolated, incarnation));
    host.begin_quarantine_report(&observation).await;
    host.recover_startup_observed(&observation).await.unwrap();
    for id in ["a-healthy", "c-healthy"] {
        assert!(host.try_get_loaded_runtime(id).await.is_some(), "{id}");
    }
    assert!(host.try_get_loaded_runtime(isolated).await.is_none());
    assert!(host.agent_is_quarantined(isolated, incarnation).await);
    let report = serde_json::to_value(read_quarantine_report(host.config().as_ref())).unwrap();
    assert_eq!(
        report["summary"]["current"]["entries"][0]["stage"],
        "startup_recovery"
    );
    assert_eq!(
        report["summary"]["current"]["entries"][0]["reason"],
        "dispatch_memory_missing_start"
    );
    // A non-compliant activation still sees isolation and never retries.
    let generation = host.inner.runtimes.read().await.next_generation;
    let error = host
        .activate_agent(isolated, RuntimeActivationReason::SchedulerDispatch)
        .await
        .err()
        .unwrap();
    assert!(error.downcast_ref::<AgentQuarantined>().is_some());
    assert_eq!(host.inner.runtimes.read().await.next_generation, generation);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn quarantine_default_certified_local_missing_start_can_bind() {
    let (_home, host) = canonical_test_host();
    let agent_id = host.config().default_agent_id.clone();
    let incarnation = host.active_agent_identity(&agent_id).unwrap().incarnation;
    let host = restart_fixture_host(host).await;
    let observation = StartupObservation::with_id("startup_default_local_missing".into())
        .with_test_hook(local_missing_start_hook(&agent_id, incarnation));
    host.begin_quarantine_report(&observation).await;
    let listener = host
        .bind_primary_for_serve(&observation, "127.0.0.1:0")
        .await
        .expect("certified local failure must still bind the daemon");
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    assert!(host.agent_is_quarantined(&agent_id, incarnation).await);
    let report = serde_json::to_value(read_quarantine_report(host.config().as_ref())).unwrap();
    assert_eq!(
        report["summary"]["current"]["entries"][0]["stage"],
        "startup_recovery"
    );
    assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
    host.shutdown().await.unwrap();
}

// Non-bootstrap mode: no onboarding deadlock exists, so an unclassified
// activation or bootstrap-gate failure still aborts bind before serve is up.
#[tokio::test]
async fn quarantine_default_unclassified_bootstrap_failure_still_aborts_bind() {
    let (_home, host) = canonical_test_host();
    let host = restart_fixture_host(host).await;
    let observation = StartupObservation::with_id("startup_default_unclassified".into())
        .with_test_hook(Arc::new(|_, phase| {
            Box::pin(async move {
                if phase == "runtime.bootstrap.tasks" {
                    anyhow::bail!("fixture unclassified bootstrap failure");
                }
                Ok(())
            })
        }));
    host.begin_quarantine_report(&observation).await;
    let error = host
        .bind_primary_for_serve(&observation, "127.0.0.1:0")
        .await
        .err()
        .expect("unclassified bootstrap failure must abort binding");
    assert!(error.to_string().contains("unclassified"), "{error:#}");
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bootstrap_recovered_default_owner_does_not_block_serve_bind() {
    let (_home, host) = canonical_test_host();
    let agent_id = host.config().default_agent_id.clone();
    // Seed durable startup-recovery ownership for the *default* agent, which
    // is the one activation path exempt from the pre-bind readiness gate.
    {
        let runtime = host.default_runtime().await.unwrap();
        crate::runtime::tests::support::persist_timer_creation_for_restart(
            &runtime,
            &startup_timer_fixture(
                "timer-bootstrap-default",
                &agent_id,
                TimerStatus::Active,
                None,
                Some(Utc::now() + chrono::Duration::hours(1)),
            ),
        )
        .await
        .unwrap();
        crate::runtime::tests::support::persist_task_transition(
            &runtime,
            &TaskRecord {
                id: "task-bootstrap-default".into(),
                agent_id: agent_id.clone(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: None,
                recovery: None,
            },
            "startup_original_task_fixture",
        )
        .await
        .unwrap();
        drop(runtime);
        host.unload_runtime(&agent_id).await;
    }
    let host = restart_fixture_host(host).await;
    host.inner
        .bootstrap_provider_active
        .store(true, std::sync::atomic::Ordering::Release);
    let observation = StartupObservation::with_id("startup_bootstrap_recovered_default".into());
    host.begin_quarantine_report(&observation).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.recover_startup_observed(&observation),
    )
    .await
    .expect("startup recovery must not wait for bootstrap readiness before serve binds")
    .expect("startup recovery must succeed");
    assert!(
        host.try_get_loaded_runtime(&agent_id).await.is_some(),
        "recovered default owner stays activated while its gate is deferred"
    );
    let listener = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.bind_primary_for_serve(&observation, "127.0.0.1:0"),
    )
    .await
    .expect("serve bind must not wait for the deferred default gate")
    .expect("bootstrap serve bind must succeed");
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    // Onboarding publishes the provider through the now-bound listener.
    host.inner
        .bootstrap_provider_active
        .store(false, std::sync::atomic::Ordering::Release);
    host.inner.bootstrap_ready_notify.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let runtime = host.try_get_loaded_runtime(&agent_id).await.unwrap();
        runtime.wait_for_bootstrap().await.unwrap();
    })
    .await
    .expect("deferred default gate must resume once a provider is published");
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bootstrap_default_activation_does_not_block_serve_bind() {
    let (_home, host) = canonical_test_host();
    // Bootstrap mode with a durable Active default agent but no usable
    // provider: the default agent can only be released through onboarding,
    // which needs the HTTP listener bound here. Activation still runs before
    // bind; only the blocking bootstrap-gate wait is deferred.
    host.inner
        .bootstrap_provider_active
        .store(true, std::sync::atomic::Ordering::Release);
    let observation = StartupObservation::with_id("startup_bootstrap_bind".into());
    host.begin_quarantine_report(&observation).await;

    let listener = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.bind_primary_for_serve(&observation, "127.0.0.1:0"),
    )
    .await
    .expect("serve bind must not wait for bootstrap default activation")
    .expect("bootstrap serve bind must succeed");
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    assert!(
        host.bootstrap_mode_active(),
        "binding must not publish a provider by itself"
    );

    // Publishing a provider through onboarding releases the deferred
    // activation instead of dropping it.
    host.inner
        .bootstrap_provider_active
        .store(false, std::sync::atomic::Ordering::Release);
    host.inner.bootstrap_ready_notify.notify_waiters();
    let agent_id = host.config().default_agent_id.clone();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(runtime) = host.try_get_loaded_runtime(&agent_id).await {
                runtime.wait_for_bootstrap().await.unwrap();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deferred default activation must resume once a provider is published");
    host.shutdown().await.unwrap();
}

// Bootstrap mode defers only the bootstrap gate. Activation and construction
// failures still abort bind, so a broken default agent never silently serves.
#[tokio::test]
async fn bootstrap_mode_activation_failure_still_aborts_bind() {
    let (_home, host) = canonical_test_host();
    host.inner
        .bootstrap_provider_active
        .store(true, std::sync::atomic::Ordering::Release);
    let agent_id = host.config().default_agent_id.clone();
    host.inner.test_constructor_outcomes.lock().unwrap().insert(
        agent_id,
        anyhow!("synthetic unclassified constructor failure"),
    );
    let observation = StartupObservation::with_id("startup_bootstrap_activation_failure".into());
    host.begin_quarantine_report(&observation).await;
    let error = host
        .bind_primary_for_serve(&observation, "127.0.0.1:0")
        .await
        .err()
        .expect("bootstrap mode must still abort bind on an activation failure");
    assert!(error.to_string().contains("unclassified"), "{error:#}");
    host.shutdown().await.unwrap();
}
