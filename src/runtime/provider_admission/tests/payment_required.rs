use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestedFallback {
    Available,
    Cooling,
    Removed,
    Disabled,
}

#[tokio::test]
async fn payment_scope_is_model_local_and_preserves_legacy_chain_fingerprint() {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;

    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), ["http://127.0.0.1:1", "http://127.0.0.1:2"]);
    config.default_model = ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6").unwrap();
    config.fallback_models =
        vec![ModelRouteRef::parse("anthropic@default/claude-haiku-4-5").unwrap()];
    let runtime = open_runtime(home.path(), config);
    let chain = runtime
        .provider_admission_chain(&runtime.agent_state().await.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(chain.candidates.len(), 2);
    let a = &chain.candidates[0];
    let b = &chain.candidates[1];
    assert_eq!(a.quota, b.quota);
    assert_eq!(a.connection, b.connection);
    assert_ne!(a.payment, b.payment);
    assert!(!a.payment.key().contains("admission-test-key"));
    let legacy_identity: Vec<_> = chain
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.model.as_string(),
                candidate
                    .quota
                    .iter()
                    .chain([&candidate.connection, &candidate.request])
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        chain.fingerprint,
        format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&legacy_identity).unwrap())
        ),
        "adding payment admission must not grant unfinished roots a fresh budget"
    );
    let now = runtime.now();
    let ProviderAdmissionReservation::Granted(lease) = runtime
        .runtime_db()
        .reserve_provider_admission(&a.scopes, now, PROBE_LEASE)
        .unwrap()
    else {
        panic!("healthy A");
    };
    assert!(runtime
        .runtime_db()
        .record_provider_admission_failure(
            &lease,
            &a.payment,
            ProviderCooldownPolicy::PaymentRequired,
            now,
            Some(now + Duration::hours(1)),
            &a.model.as_string(),
        )
        .unwrap());
    assert!(runtime
        .runtime_db()
        .provider_admission_blocked(&a.scopes, now)
        .unwrap()
        .is_some());
    assert!(runtime
        .runtime_db()
        .provider_admission_blocked(&b.scopes, now)
        .unwrap()
        .is_none());
}

#[test]
fn real_http_payment_required_with_override_uses_healthy_fallback() {
    payment_required_fallback(
        true,
        "anthropic@default/claude-sonnet-4-6",
        RequestedFallback::Available,
    );
}

#[test]
fn real_http_payment_required_without_override_uses_healthy_fallback() {
    payment_required_fallback(
        false,
        "anthropic@default/claude-sonnet-4-6",
        RequestedFallback::Available,
    );
}

#[test]
fn real_http_payment_required_uses_bigmodel_fallback() {
    payment_required_fallback(
        true,
        "bigmodel@default/glm-5.3",
        RequestedFallback::Available,
    );
}

#[test]
fn real_http_payment_required_uses_minimax_fallback() {
    payment_required_fallback(
        true,
        "minimax@default/MiniMax-M2.7",
        RequestedFallback::Available,
    );
}

#[test]
fn real_http_payment_required_reselects_c_when_requested_b_is_cooling() {
    payment_required_fallback(
        true,
        "anthropic@default/claude-sonnet-4-6",
        RequestedFallback::Cooling,
    );
}

#[test]
fn real_http_payment_required_reselects_c_when_requested_b_is_removed() {
    payment_required_fallback(
        true,
        "anthropic@default/claude-sonnet-4-6",
        RequestedFallback::Removed,
    );
}

#[test]
fn real_http_payment_required_parks_when_fallback_is_disabled() {
    payment_required_fallback(
        true,
        "anthropic@default/claude-sonnet-4-6",
        RequestedFallback::Disabled,
    );
}

#[test]
fn real_http_payment_required_does_not_exclude_healthy_model_sharing_credential() {
    payment_required_fallback(
        true,
        "openai@default/gpt-5.4-mini",
        RequestedFallback::Available,
    );
}

fn payment_required_fallback(
    with_override: bool,
    fallback_model: &'static str,
    requested_state: RequestedFallback,
) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(payment_required_fallback_scenario(
                    with_override,
                    fallback_model,
                    requested_state,
                ));
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn payment_required_fallback_scenario(
    with_override: bool,
    fallback_model: &str,
    requested_state: RequestedFallback,
) {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    use axum::{extract::OriginalUri, http::StatusCode, routing::post, Json, Router};

    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let observed_primary = primary_calls.clone();
    let observed_fallback = fallback_calls.clone();
    let fallback_models = Arc::new(Mutex::new(Vec::new()));
    let observed_models = fallback_models.clone();
    let router = Router::new().fallback(post(move |OriginalUri(uri): OriginalUri, Json(body): Json<serde_json::Value>| {
        let primary = observed_primary.clone();
        let fallback = observed_fallback.clone();
        let models = observed_models.clone();
        async move {
            if uri.path().ends_with("/responses") && body["model"] == "gpt-5.4" {
                primary.fetch_add(1, Ordering::SeqCst);
                return (
                    StatusCode::PAYMENT_REQUIRED,
                    Json(serde_json::json!({
                        "error": {
                            "code": "personal-team-blocked:spending-limit",
                            "message": "You have reached your spending limit"
                        }
                    })),
                );
            }
            fallback.fetch_add(1, Ordering::SeqCst);
            models.lock().unwrap().push(body["model"].clone());
            if uri.path().ends_with("/responses") {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "id": "payment-fallback", "status": "completed",
                        "output": [{
                            "type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "payment fallback completed"}]
                        }],
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    })),
                );
            }
            if uri.path().ends_with("/chat/completions") {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "id": "payment-fallback", "object": "chat.completion",
                        "choices": [{
                            "index": 0, "finish_reason": "stop",
                            "message": {"role": "assistant", "content": "payment fallback completed"}
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })),
                );
            }
            assert!(uri.path().ends_with("/messages"), "{uri}");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": "payment-fallback", "type": "message", "role": "assistant",
                    "model": "claude-sonnet-4-6", "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": "payment fallback completed"}],
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                })),
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), [&url, &url]);
    let fallback = ModelRouteRef::parse(fallback_model).unwrap();
    config.fallback_models = vec![fallback.clone()];
    let actual = if matches!(
        requested_state,
        RequestedFallback::Cooling | RequestedFallback::Removed
    ) {
        let model = ModelRouteRef::parse("anthropic@default/claude-haiku-4-5").unwrap();
        config.fallback_models.push(model.clone());
        model
    } else {
        fallback.clone()
    };
    let endpoint = config.providers.get_mut(&fallback.provider).unwrap();
    endpoint.base_url = url;
    endpoint.credential = Some("admission-test-key".into());
    if requested_state == RequestedFallback::Disabled {
        config.disable_provider_fallback = true;
        config.stored_config.runtime.disable_provider_fallback = Some(true);
    }
    let context = crate::runtime::tests::support::continuation_ready_context_config(&home, 40_000);
    let runtime = open_runtime_with_context(
        home.path(),
        config,
        Arc::new(crate::runtime::clock::SystemClock),
        context,
    );
    let primary = ModelRouteRef::parse("openai@default/gpt-5.4").unwrap();
    if with_override {
        runtime
            .set_model_override(primary.clone(), None)
            .await
            .unwrap();
    }
    if requested_state == RequestedFallback::Cooling {
        let chain = runtime
            .provider_admission_chain(&runtime.agent_state().await.unwrap())
            .unwrap()
            .unwrap();
        let candidate = chain
            .candidates
            .iter()
            .find(|candidate| candidate.model == fallback)
            .unwrap();
        let ProviderAdmissionReservation::Granted(lease) = runtime
            .runtime_db()
            .reserve_provider_admission(&candidate.scopes, runtime.now(), PROBE_LEASE)
            .unwrap()
        else {
            panic!("the requested B was initially healthy");
        };
        assert!(runtime
            .runtime_db()
            .record_provider_admission_failure(
                &lease,
                &candidate.payment,
                ProviderCooldownPolicy::PaymentRequired,
                runtime.now(),
                Some(runtime.now() + Duration::days(1)),
                &candidate.model.as_string(),
            )
            .unwrap());
    }
    runtime.enqueue(operator()).await.unwrap();
    assert!(process_next_model_input(&runtime).await);
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    if requested_state == RequestedFallback::Disabled {
        assert!(!process_next_model_input(&runtime).await);
        let turn_index = runtime.agent_state().await.unwrap().turn_index;
        for _ in 0..4 {
            assert!(!process_next_model_input(&runtime).await);
        }
        assert_eq!(runtime.agent_state().await.unwrap().turn_index, turn_index);
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime.agent_state().await.unwrap().model_override,
            Some(primary)
        );
        server.abort();
        return;
    }
    if requested_state == RequestedFallback::Removed {
        let mut snapshot = (*runtime.inner.config_snapshot.load_full()).clone();
        let mut config = snapshot.provider_reconfig.as_ref().unwrap().config.clone();
        config.fallback_models = vec![actual.clone()];
        snapshot.model_catalog = RuntimeModelCatalog::from_config(&config);
        snapshot.provider_reconfig = Some(ProviderReconfigurator { config });
        runtime.inner.config_snapshot.store(Arc::new(snapshot));
    }
    assert!(process_next_model_input(&runtime).await);
    assert_eq!(
        fallback_calls.load(Ordering::SeqCst),
        1,
        "the healthy fallback must receive a real HTTP request after HTTP 402; A requests: {}",
        primary_calls.load(Ordering::SeqCst)
    );
    assert_eq!(
        primary_calls.load(Ordering::SeqCst),
        1,
        "HTTP 402 must exclude A rather than immediately selecting it again"
    );
    assert_eq!(
        runtime.agent_state().await.unwrap().model_override,
        with_override.then_some(primary)
    );
    let events = runtime.storage().read_recent_events(256).unwrap();
    let selected = serde_json::to_value(&actual).unwrap();
    let recovery = events
        .iter()
        .find(|event| event.kind == "recovery_turn_started")
        .expect("a real recovery turn");
    assert_eq!(
        recovery.data["requested_fallback_model_ref"],
        serde_json::to_value(&fallback).unwrap()
    );
    assert_eq!(recovery.data["admitted_model_ref"], selected);
    assert_eq!(recovery.data["fallback_model_ref"], selected);
    match requested_state {
        RequestedFallback::Cooling => assert_eq!(
            recovery.data["reselection_reason"],
            "requested_candidate_cooling"
        ),
        RequestedFallback::Removed => assert_eq!(
            recovery.data["reselection_reason"],
            "requested_model_not_in_effective_chain"
        ),
        _ => assert!(recovery.data["reselection_reason"].is_null()),
    }
    assert_eq!(
        *fallback_models.lock().unwrap(),
        vec![serde_json::json!(actual
            .as_string()
            .rsplit('/')
            .next()
            .unwrap())],
        "the actual HTTP body must name the admitted model"
    );
    assert!(events.iter().any(|event| {
        event.kind == "lineage_selected" && event.data["recovery_fallback_model"] == selected
    }));
    assert!(events.iter().any(|event| {
        event.kind == "pending_model_promoted" && event.data["fallback_model"] == selected
    }));
    assert_eq!(
        runtime
            .storage()
            .read_recent_briefs(20)
            .unwrap()
            .iter()
            .filter(|brief| brief.kind.is_success() && brief.text == "payment fallback completed")
            .count(),
        1,
        "the successful fallback is delivered exactly once"
    );
    server.abort();
}
