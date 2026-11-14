//! `GET /agents/{agent_id}/model` regressions: output compatibility with the
//! full summary `.model`, lightweight read evidence, and error parity with the
//! status route.
use super::*;
use crate::types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset};
use axum::{
    body::{to_bytes, Body},
    http::{header::AUTHORIZATION, Request, StatusCode},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tower::ServiceExt;
use tracing_subscriber::prelude::*;

const ID: &str = "model-http-fixture";
const HEAVY: &str = "model-http-fixture-heavy";
const MISSING: &str = "model-http-fixture-missing";

fn named_identity(agent_id: &str) -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        agent_id,
        AgentKind::Named,
        crate::types::AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

async fn get(app: axum::routing::Router, uri: &str, token: bool) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    if token {
        builder = builder.header(AUTHORIZATION, "Bearer secret");
    }
    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

/// Counts `holon::agent_summary` events, which every full or compact summary
/// build emits. Zero events during a model read is deterministic evidence that
/// no summary builder ran.
#[derive(Clone, Default)]
struct SummaryEvents(Arc<AtomicU64>);

impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for SummaryEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == "holon::agent_summary" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn override_fixture_state(agent_id: &str) -> crate::types::AgentState {
    let route = crate::config::ModelRouteRef::parse("openai@default/gpt-5.4").unwrap();
    let mut state = crate::types::AgentState::new(agent_id.to_string());
    state.status = crate::types::AgentStatus::Stopped;
    state.model_override = Some(route.clone());
    state.model_override_reasoning_effort = Some("high".into());
    state.model_override_service_tier = Some(crate::config::ServiceTier::Fast);
    state.last_requested_model = Some(route.clone());
    state.last_active_model = Some(route);
    state
}

#[tokio::test]
async fn cold_model_endpoint_matches_full_summary_model_without_summary_reads() -> Result<()> {
    let (_home, host) = super::tests::control_token_test_host();
    host.append_agent_identity(&named_identity(ID))?;
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&override_fixture_state(ID))?;
    assert!(host.try_get_loaded_runtime(ID).await.is_none());
    let app = router(AppState::for_tcp(host.clone()));
    let events = SummaryEvents::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(events.clone()));

    // Positive control: the full summary read emits holon::agent_summary events.
    let (code, body) = get(
        app.clone(),
        &format!("/api/agents/{ID}/status?detail=full"),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let summary: serde_json::Value = serde_json::from_slice(&body)?;
    assert!(
        events.0.load(Ordering::Relaxed) > 0,
        "full summary read should emit summary events"
    );

    events.0.store(0, Ordering::Relaxed);
    let (code, body) = get(app.clone(), &format!("/api/agents/{ID}/model"), true).await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let model: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(model, summary["model"]);
    assert_eq!(
        events.0.load(Ordering::Relaxed),
        0,
        "model read must not build any summary"
    );
    assert_eq!(model["source"], "agent_override");
    assert_eq!(model["override_model"], "openai@default/gpt-5.4");
    assert!(model["requested_model"].is_string());
    assert!(model["active_model"].is_string());
    assert_eq!(model["override_reasoning_effort"], "high");
    assert!(model["resolved_policy"].is_object());

    // Durable clear must read back as runtime default through both surfaces.
    let cleared_state = crate::types::AgentState::new(ID.to_string());
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&cleared_state)?;
    let (code, body) = get(app.clone(), &format!("/api/agents/{ID}/model"), true).await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let cleared: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(cleared["source"], "runtime_default");
    assert!(cleared.get("override_model").is_none());
    // `requested_model` mirrors the summary contract: it is always the
    // effective request, so after the clear it falls back to the default.
    assert!(cleared["requested_model"].is_string());
    assert_eq!(cleared["requested_model"], cleared["effective_model"]);
    assert_eq!(cleared["effective_model"], cleared["runtime_default_model"]);
    assert!(cleared.get("override_reasoning_effort").is_none());
    assert_eq!(
        events.0.load(Ordering::Relaxed),
        0,
        "model reads must stay summary-free across durable mutations"
    );

    // The model read must not have activated a runtime or written anything.
    assert!(host.try_get_loaded_runtime(ID).await.is_none());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_model_endpoint_matches_full_summary_model_without_summary_reads() -> Result<()> {
    let (_home, host) = super::tests::control_token_test_host();
    let agent_id = host.config().default_agent_id.clone();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    assert!(host.try_get_loaded_runtime(&agent_id).await.is_some());
    let app = router(AppState::for_tcp(host.clone()));
    let events = SummaryEvents::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(events.clone()));

    let (code, body) = get(
        app.clone(),
        &format!("/api/agents/{agent_id}/status?detail=full"),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let summary: serde_json::Value = serde_json::from_slice(&body)?;
    assert!(events.0.load(Ordering::Relaxed) > 0);

    // Steady-state loaded reads: identical output, no summary builder, and the
    // runtime stays loaded.
    for _ in 0..2 {
        events.0.store(0, Ordering::Relaxed);
        let (code, body) = get(app.clone(), &format!("/api/agents/{agent_id}/model"), true).await;
        assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let model: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(model, summary["model"]);
        assert_eq!(events.0.load(Ordering::Relaxed), 0);
    }
    assert!(host.try_get_loaded_runtime(&agent_id).await.is_some());
    drop(runtime);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_model_response_is_independent_of_non_model_material() -> Result<()> {
    let (_home, host) = super::tests::control_token_test_host();
    for agent_id in [ID, HEAVY] {
        host.append_agent_identity(&named_identity(agent_id))?;
    }
    let light = override_fixture_state(ID);
    let mut heavy = override_fixture_state(HEAVY);
    heavy.last_wake_reason = Some("非模型材料-汉🦀".repeat(16_384));
    heavy.total_input_tokens = 987_654_321;
    heavy.total_model_rounds = 123_456;
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&light)?;
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&heavy)?;
    let app = router(AppState::for_tcp(host.clone()));

    let (_, light_model) = get(app.clone(), &format!("/api/agents/{ID}/model"), true).await;
    let (_, heavy_model) = get(app.clone(), &format!("/api/agents/{HEAVY}/model"), true).await;
    assert_eq!(light_model, heavy_model);

    // Control: the full summary response does scale with non-model material.
    let (_, light_summary) = get(
        app.clone(),
        &format!("/api/agents/{ID}/status?detail=full"),
        true,
    )
    .await;
    let (_, heavy_summary) = get(
        app.clone(),
        &format!("/api/agents/{HEAVY}/status?detail=full"),
        true,
    )
    .await;
    assert!(
        heavy_summary.len() > light_summary.len() + 16_384,
        "heavy summary should carry the extra non-model material: {} vs {}",
        heavy_summary.len(),
        light_summary.len()
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn model_endpoint_errors_match_status_and_create_nothing() -> Result<()> {
    let (_home, host) = super::tests::control_token_test_host();
    host.append_agent_identity(&named_identity(ID))?;
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&override_fixture_state(ID))?;
    let app = router(AppState::for_tcp(host.clone()));

    // Unknown agent: same mapping as status, and nothing is created.
    let (model_code, model_body) =
        get(app.clone(), &format!("/api/agents/{MISSING}/model"), true).await;
    let (status_code, status_body) = get(
        app.clone(),
        &format!("/api/agents/{MISSING}/status?detail=full"),
        true,
    )
    .await;
    assert_eq!(model_code, status_code);
    let model_error: serde_json::Value = serde_json::from_slice(&model_body)?;
    let status_error: serde_json::Value = serde_json::from_slice(&status_body)?;
    assert_eq!(model_code, StatusCode::NOT_FOUND);
    assert_eq!(model_error["code"], status_error["code"]);
    assert_eq!(model_error["code"], "not_found");
    assert!(host.agent_identity_record(MISSING)?.is_none());
    assert!(host.runtime_db().agent_states().latest(MISSING)?.is_none());
    assert!(host.try_get_loaded_runtime(MISSING).await.is_none());

    // Unauthorized access behaves like the status route.
    let (model_unauthorized, _) = get(app.clone(), &format!("/api/agents/{ID}/model"), false).await;
    let (status_unauthorized, _) = get(
        app.clone(),
        &format!("/api/agents/{ID}/status?detail=full"),
        false,
    )
    .await;
    assert_eq!(model_unauthorized, status_unauthorized);
    assert_eq!(model_unauthorized, StatusCode::UNAUTHORIZED);

    host.shutdown().await?;
    Ok(())
}
