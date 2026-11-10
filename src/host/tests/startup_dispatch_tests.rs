use super::*;

async fn queued_startup_fixture() -> (
    tempfile::TempDir,
    RuntimeHost,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let (home, host) = canonical_test_host();
    for agent in ["a-startup-dispatch", "b-startup-bootstrap"] {
        host.create_named_agent(agent, None).await.unwrap();
        host.unload_runtime(agent).await;
        let runtime = host.construct_runtime(agent, None).unwrap();
        runtime
            .enqueue(MessageEnvelope::new(
                agent,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "startup dispatch barrier input".into(),
                },
            ))
            .await
            .unwrap();
    }
    let config = host.config().as_ref().clone();
    host.shutdown().await.unwrap();
    drop(host);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let host =
        RuntimeHost::new_with_provider(config, Arc::new(QueuedStartupProvider(calls.clone())))
            .unwrap();
    (home, host, calls)
}

fn paused_second_bootstrap(
    reached: Arc<Notify>,
    release: Arc<Notify>,
) -> crate::startup::StartupObservation {
    crate::startup::StartupObservation::new().with_test_hook(Arc::new(move |observation, phase| {
        let reached = reached.clone();
        let release = release.clone();
        Box::pin(async move {
            if phase == "runtime.bootstrap.tasks"
                && observation.agent_id.as_deref() == Some("b-startup-bootstrap")
            {
                reached.notify_one();
                release.notified().await;
            }
            Ok(())
        })
    }))
}

#[tokio::test]
async fn startup_dispatch_daemon_initialization_holds_default_and_recovered_owners() {
    let (_home, host, calls) = queued_startup_fixture().await;
    let (_, dispatch) = host
        .recover_startup_deferred_observed(&crate::startup::StartupObservation::new())
        .await
        .unwrap();
    let default = host
        .default_runtime_bootstrapped_observed(None)
        .await
        .unwrap();
    default
        .enqueue(MessageEnvelope::new(
            host.config().default_agent_id.clone(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "default input before daemon readiness".into(),
            },
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    dispatch.release();
    for agent in ["a-startup-dispatch", "b-startup-bootstrap"] {
        wait_for_agent_inputs_settled(&host.try_get_loaded_runtime(agent).await.unwrap()).await;
    }
    wait_for_agent_inputs_settled(&default).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_dispatch_daemon_initialization_failure_retires_waiting_owners() {
    let (_home, host, calls) = queued_startup_fixture().await;
    let (_, dispatch) = host
        .recover_startup_deferred_observed(&crate::startup::StartupObservation::new())
        .await
        .unwrap();
    let _default = host
        .default_runtime_bootstrapped_observed(None)
        .await
        .unwrap();
    drop(dispatch);
    host.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(host
        .runtime_db()
        .agent_states()
        .latest("a-startup-dispatch")
        .unwrap()
        .unwrap()
        .last_runtime_failure
        .is_none());
}

#[tokio::test]
async fn startup_dispatch_rejected_recovery_does_not_block_later_activation() {
    let (_home, host, calls) = queued_startup_fixture().await;
    host.default_runtime()
        .await
        .unwrap()
        .wait_for_bootstrap()
        .await
        .unwrap();
    assert!(host
        .recover_orphaned_queue_claims_at_startup()
        .await
        .is_err());
    let runtime = host
        .activate_agent(
            "a-startup-dispatch",
            RuntimeActivationReason::OperatorControl,
        )
        .await
        .unwrap();
    runtime.wait_for_bootstrap().await.unwrap();
    wait_for_agent_inputs_settled(&runtime).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_dispatch_waits_for_later_bootstrap_then_processes_each_input_once() {
    let (_home, host, calls) = queued_startup_fixture().await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observation = paused_second_bootstrap(reached.clone(), release.clone());
    let recovery_host = host.clone();
    let recovery =
        tokio::spawn(async move { recovery_host.recover_startup_observed(&observation).await });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    let first = host
        .try_get_loaded_runtime("a-startup-dispatch")
        .await
        .unwrap();
    first.wait_for_bootstrap().await.unwrap();
    let premature = tokio::time::timeout(Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    release.notify_one();
    let _repaired_claim_owners = tokio::time::timeout(Duration::from_secs(30), recovery)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        premature.is_err(),
        "ordinary provider work ran while another startup owner was still bootstrapping"
    );
    for agent in ["a-startup-dispatch", "b-startup-bootstrap"] {
        let runtime = host.try_get_loaded_runtime(agent).await.unwrap();
        wait_for_agent_inputs_settled(&runtime).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_dispatch_failure_never_releases_an_earlier_bootstrapped_owner() {
    let (_home, host, calls) = queued_startup_fixture().await;
    let observation = crate::startup::StartupObservation::new().with_test_hook(Arc::new(
        move |observation, phase| {
            Box::pin(async move {
                if phase == "runtime.bootstrap.tasks"
                    && observation.agent_id.as_deref() == Some("b-startup-bootstrap")
                {
                    anyhow::bail!("fixture later bootstrap failure");
                }
                Ok(())
            })
        },
    ));
    assert!(host.recover_startup_observed(&observation).await.is_err());
    host.shutdown().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a failed startup batch must not claim ordinary inputs"
    );
    assert!(
        host.runtime_db()
            .agent_states()
            .latest("a-startup-dispatch")
            .unwrap()
            .unwrap()
            .last_runtime_failure
            .is_none(),
        "another owner's bootstrap failure must not become this agent's business failure"
    );
}

#[tokio::test]
async fn startup_dispatch_wait_does_not_block_original_runtime_retirement() {
    let (_home, host, calls) = queued_startup_fixture().await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observation = paused_second_bootstrap(reached.clone(), release.clone());
    let recovery_host = host.clone();
    let recovery =
        tokio::spawn(async move { recovery_host.recover_startup_observed(&observation).await });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    let (runtime, monitor) = {
        let registry = host.inner.runtimes.read().await;
        let entry = registry.agents.get("a-startup-dispatch").unwrap();
        (entry.runtime.clone(), entry.task.abort_handle())
    };
    runtime
        .retire_instance_locally("fixture startup retirement")
        .await;
    let retired = tokio::time::timeout(Duration::from_secs(3), async {
        while !monitor.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let premature = calls.load(Ordering::SeqCst);
    release.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(30), recovery)
        .await
        .unwrap()
        .unwrap();
    assert!(
        retired.is_ok(),
        "retirement cannot wait for another startup owner's bootstrap"
    );
    assert_eq!(premature, 0);
    host.shutdown().await.unwrap();
}
