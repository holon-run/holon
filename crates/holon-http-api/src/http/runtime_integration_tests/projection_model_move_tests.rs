use super::*;
use crate::{
    config::{ModelRouteRef, StateMemoryLimits},
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentListModelSummary, AgentModelSource},
};

fn text(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 79);
    text.push_str(value);
    text
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
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
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
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        }
        drop(model);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
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
