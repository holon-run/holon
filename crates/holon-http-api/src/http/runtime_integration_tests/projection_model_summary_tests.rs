use super::*;
use crate::{
    config::{ModelRouteRef, StateMemoryLimits},
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        agent_model_state_for_catalog, input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentListModelSummary,
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
                assert_eq!(
                    self.ledger.report_for_http_test().unwrap().observed_bytes,
                    self.cost
                );
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
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        }
        drop(summary);
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
        let text = String::from_utf8(body.to_vec())?;
        assert!(!text.contains("resolved_policy"));
        assert!(!text.contains("_material"));
    }
    Ok(())
}
