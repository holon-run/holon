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

#[tokio::test]
async fn quarantine_rejects_operator_control_and_peer_delivery_without_target_changes() {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host) = canonical_test_host();
    let target = "isolated-peer";
    host.create_named_agent(target, None).await.unwrap();
    let host = restart_fixture_host(host).await;
    let sender = host.default_runtime().await.unwrap();
    let storage = host.agent_storage(target).unwrap();
    let before_state = storage.read_agent().unwrap();
    let before_queue = storage.latest_queue_entries().unwrap();
    reject_constructor(&host, target);

    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let response = app
        .oneshot(
            Request::post(format!("/api/control/agents/{target}/control"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"action":"start"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(body.contains("agent_quarantined"));
    assert!(!body.contains(target));

    let error = sender
        .agent_messaging_service()
        .send(
            AgentMessageSendRequest {
                target_agent_id: target.to_owned(),
                content: MessageBody::Text {
                    text: "synthetic peer delivery".into(),
                },
                client_idempotency_key: "quarantine-peer-test".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Normal),
                forward: false,
                reply_context_id: None,
            },
            AuthorityClass::RuntimeInstruction,
        )
        .await
        .err()
        .expect("peer delivery must not reach an isolated target");
    assert!(error.is::<crate::host::AgentQuarantined>(), "{error:#}");
    assert!(host.try_get_loaded_runtime(target).await.is_none());
    assert_eq!(storage.read_agent().unwrap(), before_state);
    assert_eq!(storage.latest_queue_entries().unwrap(), before_queue);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn quarantine_callback_routes_preserve_auth_and_do_not_enqueue() {
    use crate::{
        callbacks::{generate_callback_token, hash_callback_token},
        runtime_db::repositories::upsert_external_trigger_tx,
        types::{
            CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope,
            ExternalTriggerStatus,
        },
    };
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    // New default ingress is wake-only; seed a valid legacy record for enqueue.
    for (mode, correct_path, wrong_path) in [
        (CallbackDeliveryMode::WakeHint, "wake", "enqueue"),
        (CallbackDeliveryMode::EnqueueMessage, "enqueue", "wake"),
    ] {
        let (_home, host) = canonical_test_host();
        let agent_id = host.config().default_agent_id.clone();
        let token = match mode {
            CallbackDeliveryMode::WakeHint => {
                let runtime = host.default_runtime().await.unwrap();
                let capability = runtime.default_external_trigger(mode).await.unwrap();
                capability
                    .trigger_url
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned()
            }
            CallbackDeliveryMode::EnqueueMessage => {
                let token = generate_callback_token();
                let record = ExternalTriggerRecord {
                    external_trigger_id: crate::ids::external_trigger_id(),
                    target_agent_id: agent_id.clone(),
                    scope: ExternalTriggerScope::Agent,
                    delivery_mode: mode,
                    token_hash: hash_callback_token(&token),
                    token: Some(token.clone()),
                    status: ExternalTriggerStatus::Active,
                    created_at: Utc::now(),
                    revoked_at: None,
                    last_delivered_at: None,
                    delivery_count: 0,
                };
                host.runtime_db()
                    .transaction(|tx| upsert_external_trigger_tx(tx, &record))
                    .unwrap();
                token
            }
        };
        let host = restart_fixture_host(host).await;
        assert!(!host.bootstrap_mode_active());
        host.inner.test_constructor_outcomes.lock().unwrap().insert(
            agent_id.clone(),
            anyhow!("synthetic callback internal detail"),
        );
        let pending = || -> usize {
            host.runtime_db()
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM queue_entries", [], |row| row.get(0))
                .unwrap()
        };
        let baseline = pending();
        let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
        for (case, (path, credential, status)) in [
            (correct_path, "invalid", StatusCode::FORBIDDEN),
            (wrong_path, token.as_str(), StatusCode::FORBIDDEN),
            (
                correct_path,
                token.as_str(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                correct_path,
                token.as_str(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (correct_path, "invalid", StatusCode::FORBIDDEN),
            (
                correct_path,
                token.as_str(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if case == 3 {
                reject_constructor(&host, &agent_id);
            }
            let request = Request::post(format!("/api/callbacks/{path}/{credential}"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let actual = response.status();
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                actual, status,
                "mode {correct_path}, case {case}, error {:?}",
                envelope["error"]
            );
            if status == StatusCode::SERVICE_UNAVAILABLE {
                let body = std::str::from_utf8(&body).unwrap();
                assert!(body.contains("agent_quarantined"));
                assert!(!body.contains(&token));
                assert!(!body.contains(&agent_id));
            } else if status == StatusCode::INTERNAL_SERVER_ERROR {
                let body = std::str::from_utf8(&body).unwrap();
                assert!(body.contains("callback_internal_error"));
                assert!(!body.contains("agent_quarantined"));
                assert!(!body.contains("synthetic callback internal detail"));
                assert!(!body.contains(&token));
                assert!(!body.contains(&agent_id));
            }
            assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
            assert_eq!(pending(), baseline);
        }
        host.shutdown().await.unwrap();
    }
}
