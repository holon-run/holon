use super::*;
use crate::{
    config::{ModelRouteOptions, ModelRouteRef, ServiceTier, StateMemoryLimits},
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
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
                assert_eq!(
                    self.1.report_for_http_test().unwrap().observed_bytes,
                    self.2
                );
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
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        }
        drop(model);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
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
