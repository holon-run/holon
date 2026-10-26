use super::super::*;
use crate::{
    config::{ModelRouteRef, ServiceTier, StateMemoryLimits},
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    model_catalog::ResolvedRuntimeModelPolicy,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentListModelSummary, AgentModelSource, AgentModelState},
};

fn text(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 79);
    text.push_str(value);
    text
}

fn route(value: &str) -> ModelRouteRef {
    let mut route = ModelRouteRef::parse("openai@default/fixture").unwrap();
    route.model = text(value);
    route
}

fn model(mask: u8, fallbacks: usize) -> AgentModelState {
    let mut chain = Vec::with_capacity(9);
    chain.extend((0..fallbacks).map(|_| route("fallback-汉字\0🦀")));
    let mut policy = ResolvedRuntimeModelPolicy::default();
    policy.display_name = text("policy is not part of the list summary");
    AgentModelState {
        source: AgentModelSource::AgentOverride,
        runtime_default_model: route("runtime"),
        effective_model: route("effective"),
        requested_model: (mask & 1 != 0).then(|| route("requested")),
        active_model: (mask & 2 != 0).then(|| route("active")),
        fallback_active: mask & 2 != 0,
        effective_fallback_models: chain,
        override_model: (mask & 4 != 0).then(|| route("override")),
        override_reasoning_effort: (mask & 8 != 0).then(|| text("high-汉字\0🦀")),
        override_service_tier: None,
        effective_service_tier: None,
        resolved_policy: policy,
    }
}

fn allocations<'a>(
    routes: impl IntoIterator<Item = &'a ModelRouteRef>,
    reasoning: Option<&String>,
) -> Vec<(usize, usize, usize)> {
    let mut result = Vec::new();
    for route in routes {
        for value in [route.provider.as_str(), route.endpoint.as_str()] {
            result.push((value.as_ptr() as usize, value.len(), 0));
        }
        result.push((
            route.model.as_ptr() as usize,
            route.model.len(),
            route.model.capacity(),
        ));
    }
    if let Some(value) = reasoning {
        result.push((value.as_ptr() as usize, value.len(), value.capacity()));
    }
    result
}

fn source_allocations(model: &AgentModelState) -> Vec<(usize, usize, usize)> {
    allocations(
        [&model.runtime_default_model, &model.effective_model]
            .into_iter()
            .chain(model.requested_model.iter())
            .chain(model.active_model.iter())
            .chain(model.override_model.iter())
            .chain(model.effective_fallback_models.iter()),
        model.override_reasoning_effort.as_ref(),
    )
}

fn summary_allocations(model: &AgentListModelSummary) -> Vec<(usize, usize, usize)> {
    allocations(
        [&model.runtime_default_model, &model.effective_model]
            .into_iter()
            .chain(model.requested_model.iter())
            .chain(model.active_model.iter())
            .chain(model.override_model.iter())
            .chain(model.effective_fallback_models.iter()),
        model.override_reasoning_effort.as_ref(),
    )
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-model-move".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn header() -> Result<(u64, usize)> {
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

#[test]
fn owned_model_matches_every_borrowed_field_and_wire_without_new_material() -> Result<()> {
    let ledger = ledger()?;
    let initial = header()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), initial.0, initial.1)?;
    let mut cases = 0;
    {
        let _entry = phase.enter();
        for mask in 0..16 {
            for fallbacks in [0, 3] {
                for source in [
                    AgentModelSource::RuntimeDefault,
                    AgentModelSource::AgentOverride,
                ] {
                    for override_tier in [None, Some(ServiceTier::Default), Some(ServiceTier::Fast)]
                    {
                        for effective_tier in
                            [None, Some(ServiceTier::Default), Some(ServiceTier::Fast)]
                        {
                            let mut model = model(mask, fallbacks);
                            model.source = source.clone();
                            model.override_service_tier = override_tier;
                            model.effective_service_tier = effective_tier;
                            let expected = AgentListModelSummary::from(&model);
                            let original = source_allocations(&model);
                            let chain = (
                                model.effective_fallback_models.as_ptr(),
                                model.effective_fallback_models.capacity(),
                            );
                            let summary = AgentListModelSummary::from(model);
                            assert_eq!(summary, expected);
                            assert_eq!(summary_allocations(&summary), original);
                            assert_eq!(
                                (
                                    summary.effective_fallback_models.as_ptr(),
                                    summary.effective_fallback_models.capacity()
                                ),
                                chain
                            );
                            let wire = serde_json::to_value(&summary)?;
                            assert_eq!(wire, serde_json::to_value(&expected)?);
                            assert!(wire.get("resolved_policy").is_none());
                            assert_eq!(phase.finish()?, initial);
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 576);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn owned_model_preserves_reserved_empty_strings_and_fallback_vector() {
    let mut model = model(15, 0);
    model.runtime_default_model.model = text("");
    model.effective_model.model = text("");
    model.requested_model.as_mut().unwrap().model = text("");
    model.active_model.as_mut().unwrap().model = text("");
    model.override_model.as_mut().unwrap().model = text("");
    model.override_reasoning_effort = Some(text(""));
    let expected = AgentListModelSummary::from(&model);
    let original = source_allocations(&model);
    let chain = (
        model.effective_fallback_models.as_ptr(),
        model.effective_fallback_models.capacity(),
    );
    let summary = AgentListModelSummary::from(model);
    assert_eq!(summary, expected);
    assert_eq!(summary_allocations(&summary), original);
    assert_eq!(
        (
            summary.effective_fallback_models.as_ptr(),
            summary.effective_fallback_models.capacity()
        ),
        chain
    );
    assert!(summary.effective_fallback_models.is_empty());
    assert!(summary.effective_fallback_models.capacity() > 0);
}

#[test]
fn owned_model_keeps_fixture_charge_original_phase_and_failure_latch() -> Result<()> {
    // This is a fixture source envelope, not a claim that catalog/model initial
    // construction admission is complete. Conversion must not clone or recharge it.
    let source = model(15, 3);
    let fields = source_allocations(&source);
    let bytes = fields
        .iter()
        .map(|(_, len, _)| len + std::mem::size_of::<String>())
        .sum::<usize>()
        + source.effective_fallback_models.capacity() * std::mem::size_of::<ModelRouteRef>();
    let records = fields.len() + source.effective_fallback_models.capacity();
    let initial = header()?;
    let full = (initial.0 + bytes as u64 * 8, initial.1 + records * 8);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(bytes as u64, records)?;
        let summary = AgentListModelSummary::from(source);
        assert_eq!(summary_allocations(&summary), fields);
        assert_eq!(phase.finish()?, full);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        drop(summary);
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        assert!(charge_owned_metadata(1, 1).is_err());
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
        assert!(charge_owned_metadata(1, 1).is_err());
        assert_eq!(ledger.report()?.observed_bytes, full.0);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, AgentState)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let mut state = AgentState::new("web");
    state.model_override = Some(ModelRouteRef::parse("openai@default/gpt-5.4")?);
    state.last_requested_model = state.model_override.clone();
    state.last_active_model = Some(ModelRouteRef::parse("anthropic@default/claude-sonnet-4-5")?);
    state.model_override_reasoning_effort = Some(text("high"));
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    connection.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('model-brief', 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [],
    )?;
    Ok((home, host, state))
}

#[tokio::test]
async fn model_snapshot_and_http_consumers_keep_fields_and_original_holder() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host, state) = fixture().await?;
    let expected = AgentListModelSummary::from(&crate::runtime::agent_model_state_for_catalog(
        &RuntimeModelCatalog::from_config(&host.config()),
        &host.runtime_context_config(),
        &state,
    ));
    assert!(expected.fallback_active);
    assert_eq!(expected.source, AgentModelSource::AgentOverride);
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (model, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                assert_eq!(snapshot.agents.len(), 1);
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (
                    snapshot.agents.pop().unwrap().agent.model,
                    snapshot._material,
                )
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (snapshot.agent.model, snapshot._material)
            }
        };
        assert_eq!(model, expected);
        let cost = phase.finish()?;
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        struct Checked<'a> {
            model: &'a AgentListModelSummary,
            ledger: &'a InputEvidenceBudgetLedger,
            bytes: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
                if self.fail {
                    return Err(serde::ser::Error::custom("model fixture failure"));
                }
                serde::Serialize::serialize(self.model, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/model-fixture",
                &Checked {
                    model: &model,
                    ledger: &ledger,
                    bytes: cost.0,
                    fail,
                },
            );
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    serde_json::to_value(&expected)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(model);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    for (uri, pointer) in [
        ("/api/agents/snapshot", "/agents/0/agent/model"),
        (
            "/api/agents/web/projection-snapshot",
            "/projection/agent/model",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(
            value.pointer(pointer),
            Some(&serde_json::to_value(&expected)?)
        );
        assert!(!String::from_utf8(body.to_vec())?.contains("_material"));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn model_later_anchor_failure_retries_on_same_phase_and_releases_last_holder() -> Result<()> {
    let (_home, host, _) = fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE briefs SET created_at = 'invalid' WHERE evidence_id = 'model-brief'",
        [],
    )?;
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let error = host.agent_projection_snapshot("web").unwrap_err();
        assert!(
            format!("{error:#}").contains("unreadable latest brief timestamp for web"),
            "{error:#}"
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    let failed_cost = phase.finish()?;
    host.runtime_db().connection()?.execute(
        "UPDATE briefs SET created_at = '2026-02-01T00:00:00.000Z' WHERE evidence_id = 'model-brief'", [],
    )?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let full = phase.finish()?;
    assert!(full.0 > failed_cost.0 && full.1 > failed_cost.1);
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
