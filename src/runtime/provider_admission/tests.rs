use super::*;
use crate::{
    config::{AppConfig, ProviderId, RuntimeModelCatalog},
    provider::{build_provider_from_config, ModelBlock, ProviderTurnRequest},
    runtime::{
        bootstrap::ProviderReconfigurator,
        scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
    },
    types::{MessageBody, MessageDeliverySurface, MessageKind, Priority},
};

fn runtime() -> (tempfile::TempDir, RuntimeHandle) {
    runtime_with_urls(["http://127.0.0.1:1/v1", "http://127.0.0.1:2"])
}

fn runtime_with_urls(urls: [&str; 2]) -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), urls);
    let runtime = open_runtime(home.path(), config);
    (home, runtime)
}

fn config_with_urls(home: &std::path::Path, urls: [&str; 2]) -> AppConfig {
    // Resolve only after the fixture supplies its model and fake credentials.
    let mut config =
        AppConfig::load_with_home_for_config_inspection(Some(home.to_path_buf())).unwrap();
    config.default_model = ModelRouteRef::parse("openai@default/gpt-5.4").unwrap();
    config.fallback_models =
        vec![ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6").unwrap()];
    for (id, url) in [
        (ProviderId::openai(), urls[0]),
        (ProviderId::anthropic(), urls[1]),
    ] {
        let provider = config.providers.get_mut(&id).unwrap();
        provider.base_url = url.into();
        provider.credential = Some("admission-test-key".into());
    }
    config
}

fn open_runtime(home: &std::path::Path, config: AppConfig) -> RuntimeHandle {
    open_runtime_with_clock(home, config, Arc::new(crate::runtime::clock::SystemClock))
}

fn open_runtime_with_clock(
    home: &std::path::Path,
    config: AppConfig,
    clock: Arc<dyn crate::runtime::clock::Clock>,
) -> RuntimeHandle {
    let provider = build_provider_from_config(&config).unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        home.join("data"),
        home.to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider,
        "default".into(),
        crate::runtime::tests::support::context_config(),
        clock,
    )
    .unwrap();
    let mut snapshot = (*runtime.inner.config_snapshot.load_full()).clone();
    snapshot.model_catalog = RuntimeModelCatalog::from_config(&config);
    snapshot.provider_reconfig = Some(ProviderReconfigurator { config });
    runtime.inner.config_snapshot.store(Arc::new(snapshot));
    runtime
}

fn operator() -> MessageEnvelope {
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
            text: "continue".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    )
}

fn cool_quota(runtime: &RuntimeHandle, candidate: &Candidate) {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    let now = runtime.now();
    let ProviderAdmissionReservation::Granted(lease) = runtime
        .inner
        .runtime_db
        .reserve_provider_admission(&candidate.scopes, now, PROBE_LEASE)
        .unwrap()
    else {
        panic!("initially healthy candidate");
    };
    runtime
        .inner
        .runtime_db
        .record_provider_admission_failure(
            &lease,
            candidate.quota.as_ref().unwrap(),
            ProviderCooldownPolicy::HardQuota,
            now,
            Some(now + Duration::days(1)),
            &candidate.model.as_string(),
        )
        .unwrap();
}

#[test]
fn api_credential_quota_identity_is_coarse_and_does_not_cross_endpoint_or_credential() {
    use crate::provider::{ProviderQuotaIdentity, ProviderQuotaIdentityConfidence};

    let (_home, runtime) = runtime();
    let snapshot = runtime.inner.config_snapshot.load();
    let config = &snapshot.provider_reconfig.as_ref().unwrap().config;
    let mut endpoint = config.providers.get(&ProviderId::openai()).unwrap().clone();
    let identity = ProviderQuotaIdentity::for_api_credential(&endpoint).unwrap();
    assert_eq!(identity.confidence, ProviderQuotaIdentityConfidence::Coarse);
    assert!(!identity.scope.contains("admission-test-key"));
    endpoint.base_url.push('/');
    assert_eq!(
        ProviderQuotaIdentity::for_api_credential(&endpoint).unwrap(),
        identity
    );
    endpoint.credential = Some("another-credential".into());
    assert_ne!(
        ProviderQuotaIdentity::for_api_credential(&endpoint).unwrap(),
        identity
    );
    endpoint.credential = Some("admission-test-key".into());
    endpoint.base_url = "http://127.0.0.1:3".into();
    assert_ne!(
        ProviderQuotaIdentity::for_api_credential(&endpoint).unwrap(),
        identity
    );
    endpoint.auth.kind = crate::config::CredentialKind::OAuth;
    assert!(ProviderQuotaIdentity::for_api_credential(&endpoint).is_none());
}

#[tokio::test]
async fn whole_chain_cooling_keeps_input_unclaimed_and_creates_no_turn_or_charge() {
    let (_home, runtime) = runtime();
    let state = runtime.agent_state().await.unwrap();
    let chain = runtime.provider_admission_chain(&state).unwrap().unwrap();
    assert_eq!(chain.candidates.len(), 2);
    for candidate in &chain.candidates {
        cool_quota(&runtime, candidate);
    }
    let message = runtime.enqueue(operator()).await.unwrap();
    for _ in 0..4 {
        assert!(matches!(
            SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap(),
            RunLoopPoll::Idle
        ));
    }
    let parked = runtime
        .inner
        .runtime_db
        .provider_queue_parking("default", &message.id)
        .unwrap()
        .unwrap();
    assert_eq!(parked.reason, "chain_cooling");
    let episode = runtime
        .inner
        .runtime_db
        .provider_recovery_episode(&parked.episode_key)
        .unwrap()
        .unwrap();
    assert_eq!(episode.total_requests, 0);
    assert_eq!(runtime.inner.agent.lock().await.queue.len(), 1);
    assert!(runtime
        .agent_state()
        .await
        .unwrap()
        .current_turn_id
        .is_none());
    assert!(runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .is_none_or(|state| state.attempts.is_empty()));
    assert!(SchedulerDecisionExecutor::new(&runtime)
        .transition_run_loop_idle_to_sleep(Some(parked.not_before))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn cooling_primary_does_not_park_a_healthy_fallback() {
    let (_home, runtime) = runtime();
    let state = runtime.agent_state().await.unwrap();
    let chain = runtime.provider_admission_chain(&state).unwrap().unwrap();
    cool_quota(&runtime, &chain.candidates[0]);
    let message = runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy fallback must be admitted")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    assert_eq!(admission.model(), &chain.candidates[1].model);
    assert_eq!(
        admission.episode().unwrap().total_requests,
        0,
        "reservation is not IO"
    );
    assert!(runtime
        .inner
        .runtime_db
        .provider_queue_parking("default", &message.id)
        .unwrap()
        .is_none());
    assert!(runtime
        .agent_state()
        .await
        .unwrap()
        .current_turn_id
        .is_none());
}

#[tokio::test]
async fn exact_external_wait_dispatch_uses_committed_canonical_fences() {
    external_wait_dispatch_with_claim_race(None).await;
}

#[tokio::test]
async fn exact_external_wait_dispatch_replans_changed_wait_snapshot() {
    external_wait_dispatch_with_claim_race(Some("wait")).await;
}

#[tokio::test]
async fn exact_external_wait_dispatch_replans_changed_work_item_snapshot() {
    external_wait_dispatch_with_claim_race(Some("work_item")).await;
}

#[tokio::test]
async fn exact_external_wait_dispatch_retries_agent_state_without_replacing_admission() {
    external_wait_dispatch_with_claim_race(Some("agent_state")).await;
}

async fn external_wait_dispatch_with_claim_race(race: Option<&'static str>) {
    use crate::{
        runtime::waiting::WaitForWakeKind,
        types::{CallbackDeliveryMode, CallbackDeliveryPayload, ExternalTriggerScope},
    };

    let (_home, runtime) = runtime();
    let chain = runtime
        .provider_admission_chain(&runtime.agent_state().await.unwrap())
        .unwrap()
        .unwrap();
    // An expired transport incident makes the primary reservation exclusive,
    // without changing legacy quota selection or advancing scheduler time.
    let past = runtime.now() - Duration::days(1);
    let crate::runtime_db::provider_admission::ProviderAdmissionReservation::Granted(lease) =
        runtime
            .inner
            .runtime_db
            .reserve_provider_admission(&chain.candidates[0].scopes, past, PROBE_LEASE)
            .unwrap()
    else {
        panic!("healthy transport");
    };
    runtime
        .inner
        .runtime_db
        .record_provider_admission_failure(
            &lease,
            &chain.candidates[0].connection,
            ProviderCooldownPolicy::Transport,
            past,
            None,
            &chain.candidates[0].model.as_string(),
        )
        .unwrap();
    let work = runtime
        .create_work_item("resume after callback".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let trigger = runtime
        .create_external_trigger(
            "admission regression".into(),
            "test".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let wait = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::External,
            None,
            "wait for callback".into(),
            None,
        )
        .await
        .unwrap();
    // Model the committed yield; registration alone has no terminal attempt.
    let mut protocol = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    protocol.work_items.insert(
        work.id.clone(),
        crate::domain::execution_protocol::WorkItemExecutionRecord {
            source_revision: wait.work_item.as_ref().unwrap().revision,
            state: crate::domain::execution_protocol::WorkItemExecutionState::Waiting {
                generation: 1,
                wait: crate::domain::execution_protocol::WaitReference {
                    wait_id: wait.condition.id.clone(),
                },
            },
        },
    );
    runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx(
                tx, &protocol,
            )
        })
        .unwrap();
    runtime
        .deliver_callback(
            &trigger.external_trigger_id,
            CallbackDeliveryPayload {
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
            },
        )
        .await
        .unwrap();
    let observed = Arc::new(Mutex::new(None));
    if let Some(race) = race {
        let observed = observed.clone();
        let work_id = work.id.clone();
        let wait_id = wait.condition.id.clone();
        *runtime.inner.claim_snapshot_before_commit.lock().unwrap() =
            Some(Box::new(move |runtime, admission| {
                let admission = admission.expect("gate must precede the claim snapshot race");
                assert_eq!(admission.episode()?.total_requests, 0);
                assert!(admission
                    .state
                    .lock()
                    .unwrap()
                    .lease
                    .as_ref()
                    .unwrap()
                    .is_probe());
                *observed.lock().unwrap() = Some((
                    Arc::downgrade(admission),
                    admission.attempt.attempt_id.clone(),
                    admission.episode_key.clone(),
                ));
                match race {
                    "agent_state" => {
                        let mut state = runtime
                            .inner
                            .runtime_db
                            .agent_states()
                            .latest("default")?
                            .expect("agent state");
                        state.last_wake_reason = Some("concurrent snapshot update".into());
                        runtime.inner.runtime_db.agent_states().upsert(&state)?;
                    }
                    "wait" => {
                        let mut condition = runtime
                            .inner
                            .runtime_db
                            .wait_conditions()
                            .latest_for_agent("default")?
                            .into_iter()
                            .find(|condition| condition.id == wait_id)
                            .expect("triggered wait");
                        condition.updated_at += Duration::seconds(1);
                        // Move only the OCC snapshot fence, preserving the same wake.
                        runtime.inner.runtime_db.transaction(|tx| {
                            tx.execute(
                                "UPDATE wait_conditions SET updated_at = ?1, payload_json = ?2
                                 WHERE wait_condition_id = ?3",
                                rusqlite::params![
                                    crate::runtime_db::repositories::timestamp(
                                        condition.updated_at
                                    ),
                                    serde_json::to_string(&condition)?,
                                    condition.id,
                                ],
                            )?;
                            Ok(())
                        })?;
                    }
                    "work_item" => {
                        let mut work = runtime
                            .inner
                            .runtime_db
                            .work_items()
                            .latest(&work_id)?
                            .expect("wait owner");
                        let revision = work.revision;
                        work.revision += 1;
                        work.objective.push_str(" with concurrent update");
                        runtime
                            .inner
                            .runtime_db
                            .work_items()
                            .update_expected(&work, revision)?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }));
    }
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("exact external wait must resume");
    };
    assert_eq!(
        scheduled.message.source_refs.get("wait_id"),
        Some(&wait.condition.id)
    );
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    assert_eq!(
        admission.model(),
        &chain.candidates[0].model,
        "replanning must retain the original eligible route"
    );
    assert_eq!(admission.episode().unwrap().total_requests, 0);
    if let Some((old, attempt_id, episode_key)) = observed.lock().unwrap().as_ref() {
        assert_eq!(
            attempt_id, &admission.attempt.attempt_id,
            "the same source retains its deterministic canonical identity"
        );
        if race == Some("agent_state") {
            assert!(
                old.upgrade().is_some(),
                "AgentState retry must keep its valid guard"
            );
        } else {
            assert!(
                old.upgrade().is_none(),
                "stale guard must release its lease"
            );
        }
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .provider_recovery_episode(episode_key)
                .unwrap()
                .unwrap()
                .total_requests,
            0,
            "no dispatch before atomic claim"
        );
    }
    let protocol = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let committed = &protocol.attempts[&admission.attempt.attempt_id];
    assert_eq!(
        admission.attempt.admitted_fences, committed.admitted_fences,
        "the preclaim guard must use the same aligned wait admission as the commit"
    );
    admission.before_dispatch().unwrap();
    assert_eq!(admission.episode().unwrap().total_requests, 1);
}

#[tokio::test]
async fn healthy_responses_do_not_apply_a_global_three_request_limit() {
    let (_home, runtime) = runtime();
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy input")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    for _ in 0..5 {
        admission.before_dispatch().unwrap();
        admission
            .after_response(&Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "usable".into(),
                }],
                stop_reason: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }))
            .unwrap();
    }
    assert_eq!(admission.episode().unwrap().total_requests, 5);
    assert_eq!(
        admission.episode().unwrap().state,
        ProviderRecoveryBudgetState::Resolved
    );
    let _request = ProviderTurnRequest::plain("local preflight", Vec::new(), Vec::new());
    assert_eq!(admission.episode().unwrap().total_requests, 5);
}

#[tokio::test]
async fn concurrent_healthy_success_keeps_pre_turn_reservation_dispatchable() {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    let (_home, runtime) = runtime();
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy input")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    let ProviderAdmissionReservation::Granted(other) = runtime
        .inner
        .runtime_db
        .reserve_provider_admission(&admission.candidate.scopes, runtime.now(), PROBE_LEASE)
        .unwrap()
    else {
        panic!("concurrent healthy request")
    };
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .record_provider_admission_success(&other, runtime.now())
            .unwrap(),
        0
    );
    admission.before_dispatch().unwrap();
    assert_eq!(admission.episode().unwrap().total_requests, 1);
}

#[tokio::test]
async fn scarce_operator_probe_skips_auxiliary_until_usable_model_response() {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    let (_home, runtime) = runtime();
    let chain = runtime
        .provider_admission_chain(&runtime.agent_state().await.unwrap())
        .unwrap()
        .unwrap();
    for candidate in chain.candidates {
        let ProviderAdmissionReservation::Granted(lease) = runtime
            .inner
            .runtime_db
            .reserve_provider_admission(&candidate.scopes, runtime.now(), PROBE_LEASE)
            .unwrap()
        else {
            panic!("initially healthy")
        };
        runtime
            .inner
            .runtime_db
            .record_provider_admission_failure(
                &lease,
                candidate.quota.as_ref().unwrap(),
                ProviderCooldownPolicy::HardQuota,
                runtime.now(),
                None,
                &candidate.model.as_string(),
            )
            .unwrap();
    }
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("operator policy probe")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    assert!(!admission.allows_auxiliary_dispatch().unwrap());
    assert_eq!(admission.episode().unwrap().total_requests, 0);
    admission.before_dispatch().unwrap();
    let response = ProviderTurnResponse {
        blocks: vec![ModelBlock::Text {
            text: "usable".into(),
        }],
        stop_reason: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_usage: None,
        provider_message_id: None,
        provider_request_id: None,
        request_diagnostics: None,
    };
    admission.after_request(Ok(Some(&response))).unwrap();
    assert!(admission.allows_auxiliary_dispatch().unwrap());
    assert_eq!(admission.episode().unwrap().total_requests, 1);
}

#[tokio::test]
async fn unknown_http_attempts_and_a_denied_retry_never_refill_the_budget() {
    let (_home, runtime) = runtime();
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy input")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    admission.before_dispatch().unwrap();
    for _ in 0..3 {
        let error = admission.before_dispatch().unwrap_err();
        assert!(
            error.to_string().contains("ScopeAlreadyAttempted"),
            "{error}"
        );
    }
    admission
        .after_response(&Err(anyhow!("guard rejected a repeated quota scope")))
        .unwrap();
    let episode = admission.episode().unwrap();
    assert_eq!(episode.total_requests, 1);
    assert_eq!(episode.request_count, 1);
    assert_ne!(episode.state, ProviderRecoveryBudgetState::Resolved);
}

#[test]
fn real_http_operator_compaction_settles_model_before_auxiliary_request() {
    run_guarded_openai_scenario(GuardedOpenAiCase::Compact);
}

#[test]
fn real_http_operator_lossless_replay_settles_rejected_continuation() {
    run_guarded_openai_scenario(GuardedOpenAiCase::Replay);
}

#[test]
fn real_http_pre_compact_failure_keeps_typed_transport_error_and_charge() {
    run_guarded_openai_scenario(GuardedOpenAiCase::PreCompactFailure);
}

#[test]
fn real_http_post_compact_admission_race_preserves_usable_model_response() {
    run_guarded_openai_scenario(GuardedOpenAiCase::PostCompactRace);
}

#[derive(Clone, Copy)]
enum GuardedOpenAiCase {
    Compact,
    Replay,
    PreCompactFailure,
    PostCompactRace,
}

fn run_guarded_openai_scenario(case: GuardedOpenAiCase) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(guarded_openai_scenario(case));
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn guarded_openai_scenario(case: GuardedOpenAiCase) {
    use crate::provider::{
        AgentProvider, ContinuationScopeId, ConversationMessage, OpenAiProvider,
    };
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let sends = Arc::new(AtomicUsize::new(0));
    let compactions = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let compact = !matches!(case, GuardedOpenAiCase::Replay);
    let pre_failure = matches!(case, GuardedOpenAiCase::PreCompactFailure);
    let race = matches!(case, GuardedOpenAiCase::PostCompactRace);
    type ConcurrentFailure = (
        RuntimeDb,
        ProviderAdmissionLease,
        ProviderAdmissionScope,
        DateTime<Utc>,
    );
    let pending_failure = Arc::new(Mutex::<Option<ConcurrentFailure>>::new(None));
    let concurrent_failure = pending_failure.clone();
    let (observed, compacted, rejections) = (sends.clone(), compactions.clone(), rejected.clone());
    let app = Router::new().fallback(post(
        move |uri: axum::http::Uri, Json(body): Json<serde_json::Value>| {
            let (observed, compacted, rejections) =
                (observed.clone(), compacted.clone(), rejections.clone());
            let concurrent_failure = concurrent_failure.clone();
            async move {
                let number = observed.fetch_add(1, Ordering::SeqCst);
                if uri.path().ends_with("/compact") {
                    compacted.fetch_add(1, Ordering::SeqCst);
                    if pre_failure {
                        return (StatusCode::SERVICE_UNAVAILABLE, "planned compact outage").into_response();
                    }
                    return Json(serde_json::json!({
                        "output": [{"type": "compaction_summary", "encrypted_content": "opaque"}]
                    }))
                    .into_response();
                }
                if !compact && body.get("previous_response_id").is_some() {
                    rejections.fetch_add(1, Ordering::SeqCst);
                    return (StatusCode::BAD_REQUEST, "continuation rejected").into_response();
                }
                if let Some((db, lease, scope, now)) = concurrent_failure.lock().unwrap().take() {
                    assert!(db.record_provider_admission_failure(
                        &lease, &scope, ProviderCooldownPolicy::Transport,
                        now, None, "concurrent/request",
                    ).unwrap());
                }
                Json(serde_json::json!({
                    "id": format!("resp-{number}"), "object": "response", "status": "completed",
                    "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "usable"}]}],
                    "usage": {"input_tokens": if pre_failure { 1 } else { 2048 }, "output_tokens": 1}
                }))
                .into_response()
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_home, runtime) = runtime_with_urls([&url, "http://127.0.0.1:1"]);
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy operator input")
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    if race {
        use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
        let db = runtime.runtime_db().clone();
        let ProviderAdmissionReservation::Granted(lease) = db
            .reserve_provider_admission(&admission.candidate.scopes, runtime.now(), PROBE_LEASE)
            .unwrap()
        else {
            panic!("concurrent healthy request")
        };
        *pending_failure.lock().unwrap() = Some((
            db,
            lease,
            admission.candidate.request.clone(),
            runtime.now(),
        ));
    }
    let mut config = runtime
        .inner
        .config_snapshot
        .load()
        .provider_reconfig
        .as_ref()
        .unwrap()
        .config
        .clone();
    if compact {
        config.validated_model_overrides.insert(
            crate::config::ModelRef::parse("openai/gpt-5.4").unwrap(),
            crate::model_catalog::ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(256),
                compaction_trigger_estimated_tokens: Some(128),
                compaction_keep_recent_estimated_tokens: Some(32),
                ..Default::default()
            },
        );
    }
    let provider = OpenAiProvider::from_config(&config, "gpt-5.4").unwrap();
    let mut request = ProviderTurnRequest::plain(
        "local test",
        vec![ConversationMessage::UserText(if compact && !pre_failure {
            "context ".repeat(1024)
        } else {
            "hello".into()
        })],
        Vec::new(),
    );
    request.continuation_scope_id = ContinuationScopeId::new("guard-aware-http");
    request.dispatch_guard = Some(admission.clone());
    let first = provider.complete_turn(request.clone()).await.unwrap();
    assert!(first
        .blocks
        .iter()
        .any(|block| matches!(block, ModelBlock::Text { text } if text == "usable")));
    if race {
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(compactions.load(Ordering::SeqCst), 0);
        assert_eq!(admission.episode().unwrap().total_requests, 1);
        assert!(runtime
            .runtime_db()
            .provider_admission_blocked(&admission.candidate.scopes, runtime.now())
            .unwrap()
            .is_some());
        server.abort();
        return;
    }
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(first.blocks),
        ConversationMessage::UserText(if pre_failure {
            "context ".repeat(1024)
        } else {
            "continue".into()
        }),
    ]);
    let second_result = provider.complete_turn(request).await;
    if pre_failure {
        let error = second_result.unwrap_err();
        assert_eq!(
            classify_provider_error(&error).kind,
            ProviderFailureKind::ServerError
        );
        assert_eq!(
            provider_transport_diagnostics(&error).unwrap().status,
            Some(503)
        );
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert_eq!(compactions.load(Ordering::SeqCst), 1);
        let episode = admission.episode().unwrap();
        assert_eq!((episode.total_requests, episode.request_count), (2, 1));
        assert_ne!(episode.state, ProviderRecoveryBudgetState::Resolved);
        assert!(runtime
            .runtime_db()
            .provider_admission_blocked(&admission.candidate.scopes, runtime.now())
            .unwrap()
            .is_some());
        server.abort();
        return;
    }
    let second = second_result.unwrap();
    assert!(second
        .blocks
        .iter()
        .any(|block| matches!(block, ModelBlock::Text { text } if text == "usable")));
    if compact {
        assert!(compactions.load(Ordering::SeqCst) >= 1);
        assert_eq!(rejected.load(Ordering::SeqCst), 0);
    } else {
        assert_eq!(compactions.load(Ordering::SeqCst), 0);
        assert_eq!(rejected.load(Ordering::SeqCst), 1);
        assert_eq!(sends.load(Ordering::SeqCst), 3);
    }
    let episode = admission.episode().unwrap();
    assert_eq!(episode.total_requests, sends.load(Ordering::SeqCst) as i64);
    assert_eq!(episode.state, ProviderRecoveryBudgetState::Resolved);
    server.abort();
}

struct OutageServer {
    url: String,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    healthy: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for OutageServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OutageServer {
    async fn start(server_deadline: bool) -> Self {
        use axum::{
            http::{StatusCode, Uri},
            routing::post,
            Json, Router,
        };
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let healthy = Arc::new(AtomicBool::new(false));
        let available = healthy.clone();
        let app = Router::new().fallback(post(move |uri: Uri| {
            let observed = observed.clone();
            let available = available.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                if available.load(Ordering::SeqCst) {
                    let body = if uri.path().ends_with("/messages") {
                        serde_json::json!({
                            "id": "msg-restored", "type": "message", "role": "assistant",
                            "model": "claude-sonnet-4-6", "stop_reason": "end_turn",
                            "content": [{"type": "text", "text": "restored"}],
                            "usage": {"input_tokens": 1, "output_tokens": 1}
                        })
                    } else {
                        serde_json::json!({
                            "id": "resp-restored", "object": "response", "status": "completed",
                            "output": [{"type": "message", "role": "assistant",
                                "content": [{"type": "output_text", "text": "restored"}]}],
                            "usage": {"input_tokens": 1, "output_tokens": 1}
                        })
                    };
                    return (StatusCode::OK, [("retry-after", "0")], Json(body));
                }
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("retry-after", if server_deadline { "3600" } else { "0" })],
                    Json(serde_json::json!({
                        "error": { "message": "planned local outage" }
                    })),
                )
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            calls,
            healthy,
            task,
        }
    }
}

async fn process_next_model_input(runtime: &RuntimeHandle) -> bool {
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap()
    else {
        return false;
    };
    let message = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    assert!(runtime
        .commit_queue_terminal_settlement(
            crate::types::QueueEntryRecord {
                message_id: message.id,
                agent_id: message.agent_id,
                priority: message.priority,
                status: crate::types::QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: runtime.now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap());
    true
}

#[test]
fn real_http_outage_has_one_send_per_route_and_then_parks_before_another_turn() {
    run_real_http_scenario(false, false, false);
}

#[test]
fn real_http_outage_with_untried_candidates_stops_at_three_requests_across_restart() {
    run_real_http_scenario(true, false, false);
}

#[test]
fn real_http_outage_recovers_one_due_probe_after_restart_and_keeps_other_incident() {
    run_real_http_scenario(false, true, false);
}

#[test]
fn real_http_outage_server_deadline_survives_restart_and_new_operator_input() {
    run_real_http_scenario(false, false, true);
}

fn run_real_http_scenario(extra_candidates: bool, restore: bool, server_deadline: bool) {
    let thread = std::thread::Builder::new()
        .name("provider-admission-http".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(real_http_outage_scenario(
                    extra_candidates,
                    restore,
                    server_deadline,
                ));
        })
        .unwrap();
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn real_http_outage_scenario(extra_candidates: bool, restore: bool, server_deadline: bool) {
    use std::sync::atomic::Ordering;

    let server = OutageServer::start(server_deadline).await;
    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), [&server.url, &server.url]);
    if extra_candidates {
        for (provider, model) in [("gemini", "gemini-3-pro"), ("xai", "grok-4.7")] {
            let route = ModelRouteRef::parse(&format!("{provider}@default/{model}")).unwrap();
            let endpoint = config.providers.get_mut(&route.provider).unwrap();
            endpoint.base_url = server.url.clone();
            endpoint.credential = Some("admission-test-key".into());
            config.fallback_models.push(route);
        }
    }
    let clock = Arc::new(crate::runtime::clock::TestClock::new(Utc::now()));
    let runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
    runtime.enqueue(operator()).await.unwrap();
    assert!(process_next_model_input(&runtime).await);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    assert!(process_next_model_input(&runtime).await);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let expected_requests = if extra_candidates {
        assert!(process_next_model_input(&runtime).await);
        3
    } else {
        2
    };
    assert!(!process_next_model_input(&runtime).await);
    let turn_index = runtime.agent_state().await.unwrap().turn_index;
    for _ in 0..4 {
        assert!(!process_next_model_input(&runtime).await);
    }
    assert_eq!(runtime.agent_state().await.unwrap().turn_index, turn_index);
    assert_eq!(server.calls.load(Ordering::SeqCst), expected_requests);
    let guard = runtime.inner.agent.lock().await;
    let parked = guard.queue.peek().unwrap();
    let parking = runtime
        .runtime_db()
        .provider_queue_parking("default", &parked.id)
        .unwrap()
        .unwrap();
    let episode = runtime
        .runtime_db()
        .provider_recovery_episode(&parking.episode_key)
        .unwrap()
        .unwrap();
    assert_eq!(episode.total_requests, expected_requests as i64);
    assert_eq!(
        parking.reason,
        if extra_candidates {
            "recovery_budget_exhausted"
        } else {
            "chain_cooling"
        }
    );
    drop(guard);
    drop(runtime);
    let reopened = open_runtime_with_clock(home.path(), config, clock.clone());
    for _ in 0..4 {
        assert!(!process_next_model_input(&reopened).await);
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), expected_requests);
    assert_eq!(reopened.agent_state().await.unwrap().turn_index, turn_index);
    assert_eq!(
        reopened
            .runtime_db()
            .provider_queue_parking("default", &parking.message_id)
            .unwrap()
            .unwrap(),
        parking,
    );
    if server_deadline {
        assert!(parking.not_before >= clock.now() + Duration::minutes(59));
        let chain = reopened
            .provider_admission_chain(&reopened.agent_state().await.unwrap())
            .unwrap()
            .unwrap();
        let deadlines: Vec<_> = chain
            .candidates
            .iter()
            .map(|candidate| {
                reopened
                    .runtime_db()
                    .provider_admission_blocked(&candidate.scopes, clock.now())
                    .unwrap()
                    .unwrap()
                    .not_before
            })
            .collect();
        // Manual permission can skip local jitter, but never the server bound.
        let earliest_server_bound = reopened
            .runtime_db()
            .with_read(|connection| {
                let value: String = connection.query_row(
                    "SELECT MIN(server_not_before) FROM provider_transport_incidents
                 WHERE resolved_at IS NULL AND server_not_before IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                Ok(DateTime::parse_from_rfc3339(&value)?.with_timezone(&Utc))
            })
            .unwrap();
        let mut new_input = operator();
        new_input.priority = Priority::Interject;
        let message = reopened.enqueue(new_input).await.unwrap();
        for _ in 0..3 {
            assert!(!process_next_model_input(&reopened).await);
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), expected_requests);
        assert_eq!(reopened.agent_state().await.unwrap().turn_index, turn_index);
        let operator_parking = reopened
            .runtime_db()
            .provider_queue_parking("default", &message.id)
            .unwrap()
            .unwrap();
        assert!(operator_parking.not_before >= earliest_server_bound);
        assert!(operator_parking.not_before >= clock.now() + Duration::minutes(59));
        for (candidate, deadline) in chain.candidates.iter().zip(deadlines) {
            assert_eq!(
                reopened
                    .runtime_db()
                    .provider_admission_blocked(&candidate.scopes, clock.now())
                    .unwrap()
                    .unwrap()
                    .not_before,
                deadline
            );
        }
    }
    if restore {
        clock.advance(
            (parking.not_before - clock.now()).to_std().unwrap()
                + std::time::Duration::from_secs(1),
        );
        server.healthy.store(true, Ordering::SeqCst);
        assert!(process_next_model_input(&reopened).await);
        assert_eq!(server.calls.load(Ordering::SeqCst), expected_requests + 1);
        let episode = reopened
            .runtime_db()
            .provider_recovery_episode(&parking.episode_key)
            .unwrap()
            .unwrap();
        assert_eq!(episode.state, ProviderRecoveryBudgetState::Resolved);
        assert!(reopened
            .runtime_db()
            .provider_queue_parking("default", &parking.message_id)
            .unwrap()
            .is_none());
        let unresolved = reopened
            .runtime_db()
            .with_read(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM provider_transport_incidents
                 WHERE resolved_at IS NULL AND consecutive_failures > 0",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            unresolved, 1,
            "one route's success must not clear the other"
        );
    }
}
