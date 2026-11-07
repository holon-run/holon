use super::super::*;
use crate::{
    config::{
        ModelListCatalog, ModelRef, ModelRouteOptions, ModelRouteRef, ServiceTier,
        StateMemoryLimits,
    },
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        agent_model_list_summary_for_catalog, input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope, try_agent_model_list_summary_for_catalog,
    },
    types::{AgentListModelSummary, AgentModelSource},
};

fn route(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse(value).unwrap()
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.to_path_buf()))?;
    config.default_model = route("mistral@remote/devstral-medium-latest");
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.stored_config.model.route_options.clear();
    config.stored_config.model.route_options.insert(
        "mistral@remote/mistral-medium-latest".into(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Fast),
        },
    );
    Ok(config)
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-catalog-view".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn expected_default_summary() -> AgentListModelSummary {
    let primary = route("mistral@remote/devstral-medium-latest");
    AgentListModelSummary {
        source: AgentModelSource::RuntimeDefault,
        runtime_default_model: primary.clone(),
        effective_model: primary.clone(),
        requested_model: Some(primary.clone()),
        active_model: Some(primary),
        fallback_active: false,
        effective_fallback_models: vec![],
        override_model: None,
        override_reasoning_effort: None,
        override_service_tier: None,
        effective_service_tier: Some(ServiceTier::Fast),
    }
}

#[test]
fn borrowed_config_summary_matches_independent_matrix_and_full_catalog() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = config(home.path())?;
    let fallback = route("openai@other/fallback-汉字");
    let override_route = route("openai@other/override-汉字");
    let active_route = route("openai@other/active");
    config.fallback_models = vec![
        config.default_model.clone(),
        fallback.clone(),
        fallback.clone(),
    ];
    for disabled in [false, true] {
        config.disable_provider_fallback = disabled;
        let catalog = RuntimeModelCatalog::from_config(&config);
        for use_override in [false, true] {
            for request_matches in [false, true] {
                for active_fallback in [false, true] {
                    for override_tier in [None, Some(ServiceTier::Fast)] {
                        let effective = if use_override {
                            &override_route
                        } else {
                            &config.default_model
                        };
                        let mut state = AgentState::new("web");
                        state.model_override = use_override.then(|| override_route.clone());
                        state.model_override_reasoning_effort =
                            use_override.then(|| "high-汉字\0🦀".into());
                        state.model_override_service_tier = override_tier;
                        state.last_requested_model = request_matches.then(|| effective.clone());
                        state.last_active_model = active_fallback.then(|| active_route.clone());
                        let is_fallback = request_matches && active_fallback;
                        let expected = AgentListModelSummary {
                            source: if use_override {
                                AgentModelSource::AgentOverride
                            } else {
                                AgentModelSource::RuntimeDefault
                            },
                            runtime_default_model: config.default_model.clone(),
                            effective_model: effective.clone(),
                            requested_model: Some(effective.clone()),
                            active_model: Some(if is_fallback {
                                active_route.clone()
                            } else {
                                effective.clone()
                            }),
                            fallback_active: is_fallback,
                            effective_fallback_models: if disabled {
                                vec![]
                            } else if use_override {
                                vec![config.default_model.clone(), fallback.clone()]
                            } else {
                                vec![fallback.clone()]
                            },
                            override_model: state.model_override.clone(),
                            override_reasoning_effort: state
                                .model_override_reasoning_effort
                                .clone(),
                            override_service_tier: override_tier,
                            effective_service_tier: if is_fallback {
                                None
                            } else {
                                override_tier.or(if use_override {
                                    None
                                } else {
                                    Some(ServiceTier::Fast)
                                })
                            },
                        };
                        let actual = try_agent_model_list_summary_for_catalog(
                            ModelListCatalog::from_config(&config),
                            &state,
                        )?;
                        assert_eq!(actual, expected);
                        assert_eq!(
                            agent_model_list_summary_for_catalog(&catalog, &state),
                            expected
                        );
                        assert_eq!(
                            try_agent_model_list_summary_for_catalog(&catalog, &state)?,
                            expected
                        );
                        assert_ne!(
                            actual.effective_model.model.as_ptr(),
                            effective.model.as_ptr()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn borrowed_catalog_keeps_exact_summary_gates_and_failed_phase_latch() -> Result<()> {
    let home = tempfile::tempdir()?;
    let config = config(home.path())?;
    let state = AgentState::new("web");
    let ledger = ledger()?;
    let initial = ResourceReadMaterial::new(&ledger, &scope())?;
    let header = initial.finish()?;
    drop(initial);
    let primary = &config.default_model;
    // Independent fixture oracle: four final routes, alias-query ModelRef,
    // and the single canonical route key. Borrowing adds no owning material.
    let route_cost = (
        ((primary.provider.as_str().len()
            + primary.endpoint.as_str().len()
            + primary.model.len()
            + std::mem::size_of::<ModelRouteRef>())
            * 8) as u64,
        24,
    );
    let steps = [
        route_cost,
        route_cost,
        route_cost,
        route_cost,
        (
            ((primary.provider.as_str().len()
                + primary.model.len()
                + std::mem::size_of::<ModelRef>())
                * 8) as u64,
            16,
        ),
        (
            (("mistral@remote/mistral-medium-latest".len() + std::mem::size_of::<String>()) * 8)
                as u64,
            8,
        ),
    ];
    let full = steps
        .iter()
        .fold(header, |sum, step| (sum.0 + step.0, sum.1 + step.1));
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let summary = {
        let _entry = phase.enter();
        try_agent_model_list_summary_for_catalog(ModelListCatalog::from_config(&config), &state)?
    };
    assert_eq!(summary, expected_default_summary());
    assert_eq!(phase.finish()?, full);
    drop((summary, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut prefix = header;
    for step in steps {
        let next = (prefix.0 + step.0, prefix.1 + step.1);
        for records in [false, true] {
            let limit = if records {
                (full.0, next.1 - 1)
            } else {
                (next.0 - 1, full.1)
            };
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limit.0, limit.1)?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                let view = ModelListCatalog::from_config(&config);
                assert!(try_agent_model_list_summary_for_catalog(view, &state).is_err());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(try_agent_model_list_summary_for_catalog(
                    ModelListCatalog::from_config(&config),
                    &state
                )
                .is_err());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        prefix = next;
    }
    drop(config);
    Ok(())
}

#[tokio::test]
async fn borrowed_catalog_snapshots_keep_original_holder_and_owned_wire_fields() -> Result<()> {
    let home = tempfile::tempdir()?;
    let host =
        RuntimeHost::new_with_provider(config(home.path())?, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let state = AgentState::new("web");
    host.runtime_db().connection()?.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    let expected = expected_default_summary();
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (model, retained) = {
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
        assert_eq!(model, expected);
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        let cost = phase.finish()?;
        drop((phase, holder));
        struct Checked<'a>(
            &'a AgentListModelSummary,
            &'a InputEvidenceBudgetLedger,
            u64,
            bool,
        );
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.1.report().unwrap().observed_bytes, self.2);
                if self.3 {
                    return Err(serde::ser::Error::custom("catalog view fixture"));
                }
                self.0.serialize(serializer)
            }
        }
        for fail in [false, true] {
            let wire = crate::http::serialize_json(
                "/catalog-view",
                &Checked(&model, &ledger, cost.0, fail),
            );
            assert_eq!(wire.is_err(), fail);
            if let Ok(bytes) = wire {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&bytes)?,
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
    // Actual HTTP routes retain the unchanged wire shape; full route regressions
    // run separately with the same frozen sources.
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    for path in [
        "/api/agents/snapshot",
        "/api/agents/web/projection-snapshot",
    ] {
        let response = crate::http::router(crate::http::AppState::for_tcp(host.clone()))
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let value: serde_json::Value = serde_json::from_slice(&body)?;
        let actual = if path == "/api/agents/snapshot" {
            &value["agents"][0]["agent"]["model"]
        } else {
            &value["projection"]["agent"]["model"]
        };
        assert_eq!(*actual, serde_json::to_value(&expected)?);
        assert!(!std::str::from_utf8(&body)?.contains("_material"));
    }
    Ok(())
}
