use super::*;
use crate::runtime::clock::{Clock, SleepFuture, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};

struct WaitClock {
    wall: TestClock,
    sleeps: AtomicUsize,
    wake: tokio::sync::Notify,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitAction {
    Resume,
    Stop,
    Extend,
    BudgetExhausted,
    UnknownDispatch,
    SourceChanged,
    EpisodeChanged,
    Interject,
    InterjectWithClosureWaiter,
}

#[test]
fn latest_non_cooling_denial_cannot_adopt_an_older_cooldown() {
    for denial in [
        LocalDispatchDenial::EpisodeChanged,
        LocalDispatchDenial::Dispatch(ProviderRecoveryDispatchDenial::BudgetExhausted),
        LocalDispatchDenial::Reservation(ProviderRecoveryDispatchDenial::EpisodePaused),
    ] {
        let error = anyhow::Error::new(DispatchDeferred(
            LocalDispatchDenial::ScopeCooling {
                not_before: Utc::now(),
            },
            None,
        ))
        .context(DispatchDeferred(denial, None));
        assert!(scope_cooling_deadline(&error).is_none(), "{denial:?}");
    }
}

#[test]
fn latest_cooling_denial_survives_context_without_adopting_an_older_deadline() {
    let not_before = Utc::now() + chrono::Duration::seconds(11);
    let error = anyhow::Error::new(DispatchDeferred(
        LocalDispatchDenial::ScopeCooling {
            not_before: not_before + chrono::Duration::seconds(600),
        },
        None,
    ))
    .context(DispatchDeferred(
        LocalDispatchDenial::ScopeCooling { not_before },
        None,
    ))
    .context("provider continuation");
    assert_eq!(scope_cooling_deadline(&error), Some(not_before));
}

#[derive(Debug, thiserror::Error)]
#[error("foreign dispatch wrapper")]
struct ForeignDenial(#[source] DispatchDeferred);

#[test]
fn foreign_source_cooldown_is_visible_but_latest_typed_denial_wins() {
    let not_before = Utc::now() + Duration::seconds(600);
    let error = anyhow::Error::new(ForeignDenial(DispatchDeferred(
        LocalDispatchDenial::ScopeCooling { not_before },
        None,
    )))
    .context("ordinary context");
    assert_eq!(scope_cooling_deadline(&error), Some(not_before));
    assert!(dispatch_pause(&error).is_none(), "no transport marker");
    assert!(local_dispatch_rejection_user_summary(&error).is_none());
    let input_changed = anyhow::Error::new(ForeignDenial(DispatchDeferred(
        LocalDispatchDenial::InputChanged,
        None,
    )));
    assert!(is_continuation_input_changed(&input_changed));
    assert!(scope_cooling_deadline(
        &error.context(DispatchDeferred(LocalDispatchDenial::EpisodeChanged, None))
    )
    .is_none());
}

#[derive(Clone, Copy, Debug)]
enum ReplanCase {
    Chain,
    Episode,
    SourceMissing,
    StorageRead,
    AuditAfterReplacement,
    AllCooling,
}

#[test]
fn cooling_replan_failures_preserve_causes_without_waiting_or_dispatch() {
    run_cooling_replan_cases(vec![
        ReplanCase::Chain,
        ReplanCase::Episode,
        ReplanCase::SourceMissing,
        ReplanCase::StorageRead,
        ReplanCase::AuditAfterReplacement,
    ]);
}

#[test]
fn reselectable_plain_continuation_waits_when_all_candidates_are_cooling() {
    run_cooling_replan_cases(vec![ReplanCase::AllCooling]);
}

fn run_cooling_replan_cases(cases: Vec<ReplanCase>) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    for case in cases {
                        cooling_replan_case(case).await;
                    }
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn cooling_replan_case(case: ReplanCase) {
    let server = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let provider = build_provider_from_config(&config).unwrap();
    let clock = Arc::new(WaitClock {
        wall: TestClock::new(Utc::now()),
        sleeps: AtomicUsize::new(0),
        wake: tokio::sync::Notify::new(),
    });
    let runtime = open_runtime_with_clock(home.path(), config, clock.clone());
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("initial input must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    let _active = runtime
        .activate_provider_admission(Some(admission.clone()))
        .unwrap();
    // A retained continuation has crossed a side-effect boundary, so a local
    // rejection must not turn into first-round source parking.
    admission.mark_side_effect_boundary(true).unwrap();
    let lease = admission.state.lock().unwrap().lease.take().unwrap();
    runtime
        .runtime_db()
        .record_provider_admission_failure(
            &lease,
            &admission.candidate.connection,
            ProviderCooldownPolicy::Transport,
            clock.now(),
            Some(clock.now() + Duration::seconds(600)),
            "concurrent/request",
        )
        .unwrap();
    let mut request = ProviderTurnRequest::plain("continuation", Vec::new(), Vec::new());
    request.dispatch_guard = Some(admission.clone());
    let error = provider.complete_turn(request.clone()).await.unwrap_err();
    assert!(crate::provider::is_dispatch_guard_rejected(&error));
    let cooling_until = scope_cooling_deadline(&error).unwrap();
    assert!(dispatch_pause(&error).is_none());
    let before = admission.episode().unwrap().total_requests;
    let expected = match case {
        ReplanCase::AllCooling => {
            use crate::runtime_db::provider_admission::ProviderAdmissionReservation;

            for denial in [
                LocalDispatchDenial::InputChanged,
                LocalDispatchDenial::EpisodeChanged,
            ] {
                let latest = provider
                    .complete_turn(request.clone())
                    .await
                    .unwrap_err()
                    .context(DispatchDeferred(denial, None));
                assert!(crate::provider::is_dispatch_guard_rejected(&latest));
                assert!(scope_cooling_deadline(&latest).is_none());
                assert!(dispatch_pause(&latest).is_none());
                assert_eq!(
                    is_continuation_input_changed(&latest),
                    matches!(denial, LocalDispatchDenial::InputChanged)
                );
                let described = runtime.local_provider_dispatch_error(latest).await;
                let descriptor = crate::runtime_error::describe_runtime_error(&described);
                assert_eq!(descriptor.code, "provider_dispatch_rejected");
                assert_eq!(
                    descriptor.safe_context.get("predicate"),
                    Some(&denial.to_string())
                );
                let summary = local_dispatch_rejection_user_summary(&described).unwrap();
                assert!(summary.contains("local safety check"), "{summary}");
                assert!(!summary.contains("cooling down"), "{summary}");
            }
            let state = runtime.inner.agent.lock().await.state.clone();
            let chain = runtime.provider_admission_chain(&state).unwrap().unwrap();
            let fallback = &chain.candidates[1];
            let ProviderAdmissionReservation::Granted(lease) = runtime
                .runtime_db()
                .reserve_provider_admission(&fallback.scopes, clock.now(), PROBE_LEASE)
                .unwrap()
            else {
                panic!("healthy fallback reservation");
            };
            runtime
                .runtime_db()
                .record_provider_admission_failure(
                    &lease,
                    &fallback.connection,
                    ProviderCooldownPolicy::Transport,
                    clock.now(),
                    Some(clock.now() + Duration::seconds(600)),
                    "concurrent/request",
                )
                .unwrap();
            let mut excluded = Default::default();
            let mut observed = Default::default();
            {
                let waiting =
                    runtime.continue_cooling_provider(error, &mut excluded, true, 1, &mut observed);
                tokio::pin!(waiting);
                tokio::select! {
                    result = &mut waiting => panic!("plain continuation must wait: {}", result.is_ok()),
                    _ = async {
                        while clock.sleeps.load(Ordering::SeqCst) == 0 {
                            tokio::task::yield_now().await;
                        }
                    } => {}
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => panic!("wait not entered"),
                }
                assert_eq!(server.calls.load(Ordering::SeqCst), 0);
                assert_eq!(admission.episode().unwrap().total_requests, before);
                let summary = runtime.agent_summary().await.unwrap();
                let wait = summary.provider_cooldown_wait.as_ref().unwrap();
                assert!(wait.not_before >= cooling_until);
                assert_eq!(
                    wait.not_before,
                    runtime
                        .runtime_db()
                        .provider_admission_blocked(&admission.candidate.scopes, clock.now(),)
                        .unwrap()
                        .unwrap()
                        .not_before,
                );
                assert!(wait.not_before >= clock.now() + Duration::seconds(600));
                clock.wall.advance(
                    (wait.not_before - clock.now()).to_std().unwrap()
                        - std::time::Duration::from_secs(1),
                );
                clock.wake.notify_one();
                tokio::select! {
                    _ = &mut waiting => panic!("cannot resume before the shared deadline"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
                assert_eq!(server.calls.load(Ordering::SeqCst), 0);
                assert_eq!(admission.episode().unwrap().total_requests, before);
                clock.wall.advance(std::time::Duration::from_secs(1));
                clock.wake.notify_one();
                assert!(matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), &mut waiting)
                        .await
                        .unwrap()
                        .unwrap(),
                    CoolingContinuation::RetrySame,
                ));
            }
            assert!(
                excluded.contains(&admission.model().as_string()),
                "real replan ran"
            );
            assert!(Arc::ptr_eq(
                &admission,
                &runtime.active_provider_admission().unwrap().unwrap()
            ));
            assert_eq!(admission.episode().unwrap().total_requests, before);
            assert!(runtime
                .agent_summary()
                .await
                .unwrap()
                .provider_cooldown_wait
                .is_none());
            assert!(runtime
                .runtime_db()
                .provider_queue_parking("default", &scheduled.message.id)
                .unwrap()
                .is_none());
            server.healthy.store(true, Ordering::SeqCst);
            provider.complete_turn(request).await.unwrap();
            assert_eq!(server.calls.load(Ordering::SeqCst), 1);
            assert_eq!(admission.episode().unwrap().total_requests, before + 1);
            return;
        }
        ReplanCase::Chain => {
            runtime.inner.agent.lock().await.state.model_override =
                Some(ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6").unwrap());
            "provider chain changed during candidate replan"
        }
        ReplanCase::Episode => {
            runtime.runtime_db().transaction(|tx| {
                tx.execute(
                    "UPDATE provider_recovery_episodes SET state = 'resolved' WHERE episode_key = ?1",
                    [&admission.episode_key],
                )?;
                Ok(())
            }).unwrap();
            "provider episode changed during candidate replan"
        }
        ReplanCase::SourceMissing => {
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "DELETE FROM messages WHERE evidence_id = ?1",
                        [&scheduled.message.id],
                    )?;
                    Ok(())
                })
                .unwrap();
            "cooling provider source message not persisted"
        }
        ReplanCase::StorageRead => {
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE messages SET payload_json = '{}' WHERE evidence_id = ?1",
                        [&scheduled.message.id],
                    )?;
                    Ok(())
                })
                .unwrap();
            "missing field"
        }
        ReplanCase::AuditAfterReplacement => {
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute_batch(
                        "CREATE TRIGGER reject_candidate_reselection
                     BEFORE INSERT ON audit_events
                     WHEN NEW.kind = 'provider_candidate_reselected'
                     BEGIN SELECT RAISE(FAIL, 'injected candidate audit failure'); END;",
                    )?;
                    Ok(())
                })
                .unwrap();
            "injected candidate audit failure"
        }
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.continue_cooling_provider(
            error,
            &mut Default::default(),
            true,
            1,
            &mut Default::default(),
        ),
    )
    .await
    .expect("real replan faults must fail closed, not enter a cooldown wait");
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("replan fault unexpectedly authorized a continuation: {case:?}"),
    };
    let chain = format!("{error:#}");
    assert!(chain.contains(expected), "{case:?}: {chain}");
    assert!(
        chain.contains("scope cooldown candidate replan failed"),
        "{chain}"
    );
    assert!(
        !chain.contains("only a typed scope cooldown can wait"),
        "{chain}"
    );
    assert!(error.chain().count() > 1, "{chain}");
    assert!(scope_cooling_deadline(&error).is_none());
    assert_eq!(clock.sleeps.load(Ordering::SeqCst), 0);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    assert_eq!(admission.episode().unwrap().total_requests, before);
    assert!(runtime
        .agent_summary()
        .await
        .unwrap()
        .provider_cooldown_wait
        .is_none());
    assert!(runtime
        .runtime_db()
        .provider_queue_parking("default", &scheduled.message.id)
        .unwrap()
        .is_none());
    let replacement = runtime.active_provider_admission().unwrap().unwrap();
    assert_eq!(
        !Arc::ptr_eq(&admission, &replacement),
        matches!(case, ReplanCase::AuditAfterReplacement),
        "audit failure happens after replacing the active admission"
    );
}

impl Clock for WaitClock {
    fn now(&self) -> DateTime<Utc> {
        self.wall.now()
    }

    fn sleep_until(&self, deadline: DateTime<Utc>) -> SleepFuture<'_> {
        Box::pin(async move {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            loop {
                let notified = self.wake.notified();
                if self.now() >= deadline {
                    return;
                }
                notified.await;
            }
        })
    }
}

#[test]
fn native_tool_round_waits_for_shared_cooldown_without_replay() {
    for seconds in [11, 600] {
        run_native_tool_round(seconds, WaitAction::Resume);
    }
}

#[test]
fn stopping_native_cooldown_wait_cancels_without_another_dispatch() {
    run_native_tool_round(600, WaitAction::Stop);
}

#[test]
fn native_cooldown_extension_updates_status_without_dispatch() {
    run_native_tool_round(11, WaitAction::Extend);
}

#[test]
fn native_cooldown_wait_rechecks_authority_and_dispatch_fences() {
    for action in [
        WaitAction::BudgetExhausted,
        WaitAction::UnknownDispatch,
        WaitAction::SourceChanged,
        WaitAction::EpisodeChanged,
    ] {
        run_native_tool_round(600, action);
    }
}

#[test]
fn native_cooldown_wait_admits_interjection_before_another_dispatch() {
    run_native_tool_round(600, WaitAction::Interject);
}

#[test]
fn native_cooldown_wait_does_not_consume_closure_notifications() {
    run_native_tool_round(600, WaitAction::InterjectWithClosureWaiter);
}

#[test]
fn prepared_transport_rechecks_interjection_revision_before_io() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(prepared_transport_input_fence());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn prepared_transport_input_fence() {
    #[derive(Debug)]
    struct PreparedDispatchBarrier {
        admission: Arc<ProviderTurnAdmission>,
        reached: Arc<tokio::sync::Notify>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl ProviderDispatchGuard for PreparedDispatchBarrier {
        fn before_dispatch(&self) -> Result<()> {
            // The adapter has prepared its request but has not charged or sent it.
            self.reached.notify_one();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            self.admission.before_dispatch()
        }
    }
    let server = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let provider = build_provider_from_config(&config).unwrap();
    let runtime = open_runtime(home.path(), config);
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("initial input must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    admission.mark_side_effect_boundary(true).unwrap();
    let revision = runtime.inner.operator_interjection_revision.clone();
    let prepared_revision = revision.load(Ordering::Acquire);
    admission
        .fence_operator_interjections(revision.clone(), prepared_revision)
        .unwrap();
    let before = admission.episode().unwrap().total_requests;
    let reached = Arc::new(tokio::sync::Notify::new());
    let (release, released) = std::sync::mpsc::channel();
    let mut request = ProviderTurnRequest::plain(
        "prepared native continuation",
        vec![crate::provider::ConversationMessage::UserText(
            "hello".into(),
        )],
        Vec::new(),
    );
    request.dispatch_guard = Some(Arc::new(PreparedDispatchBarrier {
        admission: admission.clone(),
        reached: reached.clone(),
        release: Mutex::new(released),
    }));
    let transport = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(provider.complete_turn(request))
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), reached.notified())
        .await
        .expect("transport must reach its final dispatch barrier");
    let mut input = operator();
    input.priority = Priority::Interject;
    runtime.enqueue(input).await.unwrap();
    assert!(revision.load(Ordering::Acquire) > prepared_revision);
    release.send(()).unwrap();
    let error = tokio::task::spawn_blocking(move || transport.join().unwrap())
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        crate::provider::is_dispatch_guard_rejected(&error),
        "{error:#}"
    );
    assert!(is_continuation_input_changed(&error));
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    assert_eq!(admission.episode().unwrap().total_requests, before);
    assert!(admission.state.lock().unwrap().dispatch.is_none());
}

fn run_native_tool_round(seconds: u64, action: WaitAction) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(native_tool_round(seconds, action));
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn native_tool_round(seconds: u64, action: WaitAction) {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    use axum::{routing::post, Json, Router};

    let clock = Arc::new(WaitClock {
        wall: TestClock::new(Utc::now()),
        sleeps: AtomicUsize::new(0),
        wake: tokio::sync::Notify::new(),
    });
    let failure = Arc::new(Mutex::new(
        None::<(RuntimeDb, ProviderAdmissionLease, ProviderAdmissionScope)>,
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let router = Router::new().fallback(post({
        let calls = calls.clone();
        let requests = requests.clone();
        let failure = failure.clone();
        let clock = clock.clone();
        move |Json(body): Json<serde_json::Value>| {
            let calls = calls.clone();
            let requests = requests.clone();
            let failure = failure.clone();
            let clock = clock.clone();
            async move {
                requests.lock().unwrap().push(body);
                match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        let (db, lease, scope) = failure.lock().unwrap().take().unwrap();
                        assert!(db
                            .record_provider_admission_failure(
                                &lease,
                                &scope,
                                ProviderCooldownPolicy::Transport,
                                clock.now(),
                                Some(clock.now() + Duration::seconds(seconds as i64)),
                                "concurrent/request",
                            )
                            .unwrap());
                        Json(serde_json::json!({
                            "id":"msg-tool", "type":"message", "role":"assistant",
                            "model":"claude-sonnet-4-6", "stop_reason":"tool_use",
                            "content": [
                                {"type":"thinking", "thinking":"retain native context",
                                 "signature":"native-opaque"},
                                {"type":"tool_use", "id":"call-tool", "name":"GetAgent", "input":{}}
                            ],
                            "usage":{"input_tokens":1,"output_tokens":1}
                        }))
                    }
                    1 => Json(serde_json::json!({
                        "id":"msg-done", "type":"message", "role":"assistant",
                        "model":"claude-sonnet-4-6", "stop_reason":"end_turn",
                        "content":[{"type":"text","text":"continued once"}],
                        "usage":{"input_tokens":1,"output_tokens":1}
                    })),
                    _ => panic!("same round must dispatch only once after cooling"),
                }
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), ["http://127.0.0.1:1", &url]);
    config.default_model = ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6").unwrap();
    config.fallback_models = vec![ModelRouteRef::parse("openai@default/gpt-5.4").unwrap()];
    let context = crate::runtime::tests::support::continuation_ready_context_config(&home, 40_000);
    let runtime = open_runtime_with_context(home.path(), config, clock.clone(), context);
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("initial input must be admitted");
    };
    let admission = scheduled
        .dispatch_plan
        .provider_admission
        .as_ref()
        .unwrap()
        .clone();
    let episode_key = admission.episode_key.clone();
    let scope = admission.candidate.connection.clone();
    let ProviderAdmissionReservation::Granted(lease) = runtime
        .runtime_db()
        .reserve_provider_admission(&admission.candidate.scopes, clock.now(), PROBE_LEASE)
        .unwrap()
    else {
        panic!("concurrent agent reservation");
    };
    *failure.lock().unwrap() = Some((
        runtime.runtime_db().clone(),
        lease,
        admission.candidate.connection.clone(),
    ));
    let processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::pin!(processing);
    tokio::select! {
        result = &mut processing => panic!("native continuation must wait, not terminate: {result:?}"),
        _ = async {
            while clock.sleeps.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        } => {}
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("wait was not entered"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no IO while cooling");
    let summary = runtime.agent_summary().await.unwrap();
    let wait = summary
        .provider_cooldown_wait
        .as_ref()
        .expect("queryable active wait");
    assert_eq!(
        wait.model.as_ref().unwrap().as_string(),
        "anthropic@default/claude-sonnet-4-6"
    );
    assert!(wait.not_before > clock.now());
    assert_eq!(wait.cause.failure_kind.as_deref(), Some("transport"));
    assert_eq!(
        wait.cause.detail_unavailable_reason.as_deref(),
        Some("cause_details_unavailable")
    );
    assert!(wait.attempt_id.is_some() && wait.turn_id.is_some());
    assert_eq!(
        runtime
            .storage()
            .read_recent_tool_executions(10)
            .unwrap()
            .iter()
            .filter(|tool| tool.tool_name == "GetAgent")
            .count(),
        1
    );
    assert_eq!(
        runtime
            .runtime_db()
            .provider_recovery_episode(&episode_key)
            .unwrap()
            .unwrap()
            .total_requests,
        1,
        "local waiting must not charge or refund requests"
    );
    if action == WaitAction::Stop {
        let workspace = runtime
            .agent_state()
            .await
            .unwrap()
            .active_workspace_entry
            .expect("the native continuation must have a selected workspace");
        assert!(workspace.occupancy_id.is_none());
        runtime
            .control(crate::types::ControlAction::Stop)
            .await
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut processing)
            .await
            .expect("stop must promptly interrupt a ten-minute wait");
        assert!(
            result.is_err(),
            "stop cannot complete as a successful model round"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.agent_state().await.unwrap().active_workspace_entry,
            Some(workspace),
            "Stop must preserve the source of the current resource observation"
        );
        assert!(runtime
            .agent_summary()
            .await
            .unwrap()
            .provider_cooldown_wait
            .is_none());
        let events = runtime.storage().read_recent_events(256).unwrap();
        let ended = events
            .iter()
            .filter(|event| event.data["provider_cooldown_wait_ended"]["outcome"] == "cancelled")
            .collect::<Vec<_>>();
        assert_eq!(ended.len(), 1, "Stop and waiter must share one end event");
        assert_eq!(
            ended[0].data["status"],
            serde_json::to_value(crate::types::AgentStatus::Stopped).unwrap()
        );
        server.abort();
        return;
    }
    if matches!(
        action,
        WaitAction::BudgetExhausted
            | WaitAction::UnknownDispatch
            | WaitAction::SourceChanged
            | WaitAction::EpisodeChanged
    ) {
        // Inject changes after waiting begins: none may authorize another IO.
        if action == WaitAction::UnknownDispatch {
            admission.state.lock().unwrap().unknown_dispatch = true;
        } else {
            runtime
                .runtime_db()
                .transaction(|tx| {
                    match action {
                        WaitAction::BudgetExhausted => {
                            tx.execute(
                                "UPDATE provider_recovery_episodes
                                 SET request_count = 3, total_requests = 3
                                 WHERE episode_key = ?1",
                                [&episode_key],
                            )?;
                        }
                        WaitAction::SourceChanged => {
                            tx.execute(
                                "UPDATE execution_protocol_attempts
                                 SET payload_json = json_set(payload_json, '$.source.generation', 999)
                                 WHERE attempt_id = ?1",
                                [&admission.attempt.attempt_id],
                            )?;
                        }
                        WaitAction::EpisodeChanged => {
                            tx.execute(
                                "UPDATE provider_recovery_episodes SET revision = revision + 1
                                 WHERE episode_key = ?1",
                                [&episode_key],
                            )?;
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                })
                .unwrap();
        }
        clock.wall.advance(std::time::Duration::from_secs(30));
        clock.wake.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut processing)
            .await
            .expect("invalidated wait must stop on the next bounded recheck");
        assert!(result.is_err(), "changed fences must fail closed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(runtime
            .agent_summary()
            .await
            .unwrap()
            .provider_cooldown_wait
            .is_none());
        assert!(runtime
            .storage()
            .read_recent_events(256)
            .unwrap()
            .iter()
            .any(
                |event| event.data["provider_cooldown_wait_ended"]["outcome"]
                    == "validation_failed"
            ));
        server.abort();
        return;
    }
    if action == WaitAction::Extend {
        let extended = clock.now() + Duration::seconds(600);
        runtime
            .runtime_db()
            .transaction(|tx| {
                tx.execute(
                    "UPDATE provider_transport_incidents
                 SET next_retry_at = ?1, server_not_before = ?1, revision = revision + 1
                 WHERE identity_key = ?2",
                    rusqlite::params![extended.to_rfc3339(), scope.key()],
                )?;
                Ok(())
            })
            .unwrap();
        clock.wall.advance(std::time::Duration::from_secs(30));
        clock.wake.notify_one();
        tokio::select! {
            result = &mut processing => panic!("extended cooldown must remain waiting: {result:?}"),
            _ = async {
                while clock.sleeps.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            } => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("extension was not observed"),
        }
        let state = runtime.agent_summary().await.unwrap();
        assert_eq!(
            state.provider_cooldown_wait.as_ref().unwrap().not_before,
            extended
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    if matches!(
        action,
        WaitAction::Interject | WaitAction::InterjectWithClosureWaiter
    ) {
        // This is the notification surface used by concurrent closure waiters.
        let closure_notification = runtime.inner.notify.notified();
        tokio::pin!(closure_notification);
        if action == WaitAction::InterjectWithClosureWaiter {
            closure_notification.as_mut().enable();
        }
        let mut input = operator();
        input.priority = Priority::Interject;
        input.body = MessageBody::Text {
            text: "cooldown interjection".into(),
        };
        let input = runtime.enqueue(input).await.unwrap();
        if action == WaitAction::InterjectWithClosureWaiter {
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut closure_notification)
                .await
                .expect("cooldown and closure waiters must both receive the input wake");
        }
        tokio::select! {
            result = &mut processing => panic!("interjection must retain the native round: {result:?}"),
            _ = async {
                loop {
                    if runtime.runtime_db().queue_entries().latest(&input.id).unwrap()
                        .is_some_and(|entry| entry.status == crate::types::QueueEntryStatus::Interjected)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            } => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                panic!("interjection cannot wait for the ten-minute provider deadline")
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    let advance = if action == WaitAction::Extend {
        640
    } else {
        seconds + 40
    };
    clock.wall.advance(std::time::Duration::from_secs(advance));
    clock.wake.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), &mut processing)
        .await
        .expect("same round must resume")
        .expect("native continuation must not require repair");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(runtime
        .agent_summary()
        .await
        .unwrap()
        .provider_cooldown_wait
        .is_none());
    assert!(runtime
        .storage()
        .read_recent_events(256)
        .unwrap()
        .iter()
        .any(|event| event.data["provider_cooldown_wait_ended"]["outcome"] == "resumed"));
    let bodies = requests.lock().unwrap();
    assert!(bodies[1].to_string().contains("native-opaque"));
    if matches!(
        action,
        WaitAction::Interject | WaitAction::InterjectWithClosureWaiter
    ) {
        assert!(bodies[1].to_string().contains("cooldown interjection"));
    }
    assert!(bodies[1].to_string().contains("tool_result"));
    assert_eq!(
        runtime
            .storage()
            .read_recent_tool_executions(10)
            .unwrap()
            .iter()
            .filter(|tool| tool.tool_name == "GetAgent")
            .count(),
        1,
        "waiting cannot replay the tool"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .provider_recovery_episode(&episode_key)
            .unwrap()
            .unwrap()
            .total_requests,
        2
    );
    server.abort();
}
