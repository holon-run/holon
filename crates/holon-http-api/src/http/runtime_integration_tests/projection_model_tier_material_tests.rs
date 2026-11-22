use super::*;
use crate::{
    config::{ModelRouteOptions, ModelRouteRef, ServiceTier, StateMemoryLimits},
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        agent_model_list_summary_for_catalog, input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentListModelSummary,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-model-tier-material".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn route(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse(value).unwrap()
}

fn catalog() -> RuntimeModelCatalog {
    let mut catalog = RuntimeModelCatalog::default();
    catalog.default_model = route("mistral@remote/devstral-medium-latest");
    catalog.disable_provider_fallback = true;
    catalog.fallback_models.clear();
    catalog.route_options.clear();
    catalog.route_options.insert(
        "mistral@remote/mistral-medium-latest".into(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Fast),
        },
    );
    catalog
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    AppConfig::load_with_home(Some(home.to_path_buf()))
}

#[tokio::test]
async fn alias_tier_snapshots_retain_original_holder_through_http_serialization() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = config(home.path())?;
    config.stored_config.model.route_options = catalog().route_options;
    let mut state = AgentState::new("web");
    state.model_override = Some(route("mistral@remote/devstral-medium-latest"));
    let expected =
        agent_model_list_summary_for_catalog(&RuntimeModelCatalog::from_config(&config), &state);
    assert_eq!(expected.effective_service_tier, Some(ServiceTier::Fast));
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    host.runtime_db().connection()?.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (model, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
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
                assert_eq!(
                    self.ledger.report_for_http_test().unwrap().observed_bytes,
                    self.bytes
                );
                if self.fail {
                    return Err(serde::ser::Error::custom("tier material fixture"));
                }
                serde::Serialize::serialize(self.model, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/tier-material",
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
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        }
        drop(model);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    }
    Ok(())
}
