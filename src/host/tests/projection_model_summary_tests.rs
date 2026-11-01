use super::super::*;
use crate::{
    config::{ModelRouteOptions, ModelRouteRef, ServiceTier, StateMemoryLimits},
    context::ContextConfig,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    provider::StubProvider,
    runtime::{
        agent_model_list_summary_for_catalog, agent_model_state_for_catalog,
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentListModelSummary, AgentModelSource},
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-model-summary".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn route(model: &str) -> ModelRouteRef {
    ModelRouteRef::parse(model).unwrap()
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    AppConfig::load_with_home(Some(home.to_path_buf()))
}

#[test]
fn borrowed_chain_uses_original_phase_without_charging_or_resetting_it() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut catalog = RuntimeModelCatalog::from_config(&config(home.path())?);
    catalog.fallback_models = vec![
        catalog.default_model.clone(),
        route("openai@other/fallback"),
        route("openai@other/fallback"),
    ];
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let initial = phase.finish()?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let mut chain = catalog.provider_chain_refs(None);
        assert!(std::ptr::eq(chain.next().unwrap(), &catalog.default_model));
        assert!(std::ptr::eq(
            chain.next().unwrap(),
            &catalog.fallback_models[1]
        ));
        assert!(chain.next().is_none());
        assert_eq!(phase.finish()?, initial);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, initial.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), initial.0, initial.1)?;
    {
        let _entry = phase.enter();
        assert!(charge_owned_metadata(1, 1).is_err());
        // Borrowing cannot reset a failed phase or make its guard available.
        assert_eq!(catalog.provider_chain_refs(None).count(), 2);
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn summary_owns_final_routes_after_borrowed_sources_are_destroyed() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut catalog = RuntimeModelCatalog::from_config(&config(home.path())?);
    let mut state = AgentState::new("web");
    state.model_override = Some(route("openai@other/override"));
    state.last_requested_model = state.model_override.clone();
    state.last_active_model = Some(route("openai@other/active"));
    state.model_override_reasoning_effort = Some("high-汉字\0🦀".into());
    catalog.fallback_models = vec![
        state.model_override.clone().unwrap(),
        route("openai@other/fallback"),
        route("openai@other/fallback"),
    ];
    let actual = agent_model_list_summary_for_catalog(&catalog, &state);
    let expected = AgentListModelSummary {
        source: AgentModelSource::AgentOverride,
        runtime_default_model: catalog.default_model.clone(),
        effective_model: route("openai@other/override"),
        requested_model: Some(route("openai@other/override")),
        active_model: Some(route("openai@other/active")),
        fallback_active: true,
        effective_fallback_models: vec![route("openai@other/fallback")],
        override_model: Some(route("openai@other/override")),
        override_reasoning_effort: Some("high-汉字\0🦀".into()),
        override_service_tier: None,
        effective_service_tier: None,
    };
    assert_ne!(
        actual.effective_model.model.as_ptr(),
        state.model_override.as_ref().unwrap().model.as_ptr()
    );
    assert_ne!(
        actual.effective_fallback_models[0].model.as_ptr(),
        catalog.fallback_models[1].model.as_ptr()
    );
    assert_ne!(
        actual.active_model.as_ref().unwrap().model.as_ptr(),
        state.last_active_model.as_ref().unwrap().model.as_ptr()
    );
    drop((catalog, state));
    assert_eq!(actual, expected);
    assert_eq!(
        serde_json::to_value(actual)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}

#[test]
fn summary_selection_matrix_preserves_fields_full_policy_and_wire() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut catalog = RuntimeModelCatalog::from_config(&config(home.path())?);
    let primary = catalog.default_model.clone();
    let alternate = route("anthropic@default/claude-sonnet-4-5");
    let fallback = route("openai@default/gpt-5.4-mini");
    // Duplicates and a fallback equal to the primary retain first-wins order.
    catalog.fallback_models = vec![
        primary.clone(),
        fallback.clone(),
        fallback.clone(),
        alternate.clone(),
    ];
    for model in [&primary, &alternate] {
        catalog.route_options.insert(
            model.as_string(),
            ModelRouteOptions {
                service_tier: Some(ServiceTier::Fast),
            },
        );
    }
    catalog.route_options.insert(
        fallback.as_string(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Default),
        },
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for overridden in [false, true] {
        for disabled in [false, true] {
            catalog.disable_provider_fallback = disabled;
            for history in 0..4 {
                for override_tier in [None, Some(ServiceTier::Default), Some(ServiceTier::Fast)] {
                    let effective = if overridden { &alternate } else { &primary };
                    let mut state = AgentState::new("web");
                    state.model_override = overridden.then(|| alternate.clone());
                    state.model_override_reasoning_effort = Some("high-汉字\0🦀".into());
                    state.model_override_service_tier = override_tier;
                    state.last_requested_model = match history {
                        1 => Some(route("openai@default/stale")),
                        2 | 3 => Some(effective.clone()),
                        _ => None,
                    };
                    state.last_active_model = (history != 3).then(|| fallback.clone());
                    let active_fallback = history == 2;
                    let active = if active_fallback {
                        &fallback
                    } else {
                        effective
                    };
                    let fallbacks = if disabled {
                        vec![]
                    } else if overridden {
                        vec![primary.clone(), fallback.clone()]
                    } else {
                        vec![fallback.clone(), alternate.clone()]
                    };
                    let expected = AgentListModelSummary {
                        source: if overridden {
                            AgentModelSource::AgentOverride
                        } else {
                            AgentModelSource::RuntimeDefault
                        },
                        runtime_default_model: primary.clone(),
                        effective_model: effective.clone(),
                        requested_model: Some(effective.clone()),
                        active_model: Some(active.clone()),
                        fallback_active: active_fallback,
                        effective_fallback_models: fallbacks,
                        override_model: state.model_override.clone(),
                        override_reasoning_effort: Some("high-汉字\0🦀".into()),
                        override_service_tier: override_tier,
                        effective_service_tier: Some(if active_fallback {
                            ServiceTier::Default
                        } else {
                            override_tier.unwrap_or(ServiceTier::Fast)
                        }),
                    };
                    let actual = agent_model_list_summary_for_catalog(&catalog, &state);
                    assert_eq!(actual, expected);
                    assert_eq!(
                        serde_json::to_value(&actual)?,
                        serde_json::to_value(&expected)?
                    );
                    for tokens in [4096, 8192] {
                        let base = ContextConfig {
                            prompt_budget_estimated_tokens: tokens,
                            ..ContextConfig::default()
                        };
                        let full = agent_model_state_for_catalog(&catalog, &base, &state);
                        assert_eq!(
                            full.resolved_policy,
                            catalog.resolved_model_policy(&base, state.model_override.as_ref())
                        );
                        assert_eq!(AgentListModelSummary::from(full), expected);
                    }
                    assert!(serde_json::to_value(actual)?
                        .get("resolved_policy")
                        .is_none());
                }
            }
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn summary_keeps_original_phase_charge_identity_and_failed_latch() -> Result<()> {
    let home = tempfile::tempdir()?;
    let catalog = RuntimeModelCatalog::from_config(&config(home.path())?);
    let state = AgentState::new("web");
    let expected = agent_model_list_summary_for_catalog(&catalog, &state);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let initial = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let full = (initial.0 + 8 * 19, initial.1 + 8);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let holder = phase.guard()?;
    let actual = {
        let _entry = phase.enter();
        // A source fixture envelope, not admission for remaining catalog/model
        // allocations. Removing discarded policy must not invent another phase.
        charge_owned_metadata(19, 1)?;
        let actual = agent_model_list_summary_for_catalog(&catalog, &state);
        assert_eq!(phase.finish()?, full);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        assert!(charge_owned_metadata(1, 1).is_err());
        assert_eq!(
            agent_model_list_summary_for_catalog(&catalog, &state),
            expected
        );
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
        actual
    };
    assert_eq!(actual, expected);
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop((phase, actual));
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, AgentState)> {
    let home = tempfile::tempdir()?;
    let host =
        RuntimeHost::new_with_provider(config(home.path())?, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let mut state = AgentState::new("web");
    state.model_override = Some(route("openai@default/gpt-5.4"));
    state.last_requested_model = state.model_override.clone();
    state.last_active_model = Some(route("anthropic@default/claude-sonnet-4-5"));
    state.model_override_reasoning_effort = Some("high".into());
    host.runtime_db().connection()?.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    host.runtime_db().connection()?.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('summary-brief', 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [],
    )?;
    Ok((home, host, state))
}

#[tokio::test]
async fn shared_entry_uses_supplied_catalog_without_changing_full_model() -> Result<()> {
    let (_home, host, state) = fixture().await?;
    let mut catalog = RuntimeModelCatalog::from_config(&host.config());
    catalog.default_model = route("anthropic@default/claude-sonnet-4-5");
    catalog.fallback_models = vec![route("openai@default/gpt-5.4-mini")];
    let identity = host.agent_identity_record("web")?.unwrap();
    let expected = AgentListModelSummary::from(agent_model_state_for_catalog(
        &catalog,
        &host.runtime_context_config(),
        &state,
    ));
    let entry = host.agent_list_entry_from_committed_state(identity, Some(state), &catalog)?;
    assert_eq!(entry.model, expected);
    assert_eq!(entry.model.runtime_default_model, catalog.default_model);
    assert_eq!(
        entry.model.effective_fallback_models,
        catalog.fallback_models
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn summary_snapshots_http_and_serialization_keep_original_holder() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host, state) = fixture().await?;
    let expected = AgentListModelSummary::from(agent_model_state_for_catalog(
        &RuntimeModelCatalog::from_config(&host.config()),
        &host.runtime_context_config(),
        &state,
    ));
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (summary, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                assert_eq!(snapshot.agents.len(), 1);
                (
                    snapshot.agents.pop().unwrap().agent.model,
                    snapshot._material,
                )
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                (snapshot.agent.model, snapshot._material)
            }
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        assert_eq!(summary, expected);
        let cost = phase.finish()?;
        drop((phase, holder));
        struct Checked<'a> {
            summary: &'a AgentListModelSummary,
            ledger: &'a InputEvidenceBudgetLedger,
            cost: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.cost);
                if self.fail {
                    return Err(serde::ser::Error::custom("summary fixture failure"));
                }
                serde::Serialize::serialize(self.summary, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/summary-fixture",
                &Checked {
                    summary: &summary,
                    ledger: &ledger,
                    cost: cost.0,
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
        drop(summary);
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
        let text = String::from_utf8(body.to_vec())?;
        assert!(!text.contains("resolved_policy"));
        assert!(!text.contains("_material"));
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_snapshot_error_allows_retry_on_same_phase() -> Result<()> {
    let (_home, host, _state) = fixture().await?;
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let retained = {
            let _entry = phase.enter();
            host.runtime_db().connection()?.execute(
                "UPDATE briefs SET created_at = 'broken' WHERE evidence_id = 'summary-brief'",
                [],
            )?;
            let error = if roster {
                host.agent_roster_snapshot().unwrap_err()
            } else {
                host.agent_projection_snapshot("web").unwrap_err()
            };
            assert!(format!("{error:#}").contains("timestamp"));
            let failed_cost = phase.finish()?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            host.runtime_db().connection()?.execute(
                "UPDATE briefs SET created_at = '2026-02-01T00:00:00.000Z'
                 WHERE evidence_id = 'summary-brief'",
                [],
            )?;
            let retained = if roster {
                host.agent_roster_snapshot()?._material
            } else {
                host.agent_projection_snapshot("web")?.unwrap()._material
            };
            let cost = phase.finish()?;
            assert!(cost.0 > failed_cost.0);
            assert!(cost.1 > failed_cost.1);
            assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
            retained
        };
        let cost = phase.finish()?;
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
