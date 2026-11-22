use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentSchedulingPosture,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-posture-material".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let mut state = AgentState::new("web");
    state.status = AgentStatus::AwakeIdle;
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "INSERT OR REPLACE INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'awake_idle', '2026-02-01T00:00:00.000Z', ?1)",
        [serde_json::to_string(&state)?],
    )?;
    Ok((home, host))
}

#[tokio::test]
async fn cold_state_closure_does_not_decode_historical_dispatch_bodies() -> Result<()> {
    let (_home, host) = fixture().await?;
    let connection = host.runtime_db().connection()?;
    // These deliberately unreadable historical rows distinguish observer facts
    // from the complete protocol audit. The observer must not open these bodies.
    connection.execute("INSERT INTO execution_protocol_attempts (agent_id,attempt_id,lifecycle_state,source_identity_json,source_generation,payload_json) VALUES ('web','settled-history','settled','{}',1,'{}')",[])?;
    connection.execute("INSERT INTO execution_protocol_outcomes (agent_id,outcome_id,attempt_id,payload_json) VALUES ('web','historical-outcome','settled-history','{}')",[])?;
    connection.execute("INSERT INTO execution_protocol_work_items (agent_id,work_item_id,source_revision,generation,lifecycle_state,payload_json) VALUES ('web','completed-history',1,1,'terminal','{}')",[])?;
    drop(connection);
    let projection = host
        .operator_agent_state_projection("web", 50, 50)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(
        projection.agent.scheduling_posture.posture,
        AgentSchedulingPosture::Idle
    );
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    use tower::ServiceExt;
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/agents/web/state")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    Ok(())
}

fn snapshot_entry(
    host: &RuntimeHost,
    roster: bool,
) -> Result<(
    AgentListEntry,
    Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    if roster {
        let mut snapshot = host.agent_roster_snapshot()?;
        assert_eq!(snapshot.agents.len(), 1);
        Ok((snapshot.agents.pop().unwrap().agent, snapshot._material))
    } else {
        let snapshot = host.agent_projection_snapshot("web")?.unwrap();
        assert!(snapshot.hydration_references.is_empty());
        Ok((snapshot.agent, snapshot._material))
    }
}

#[tokio::test]
async fn posture_keeps_original_holder_through_serialization_and_both_http_routes() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let (_home, host) = fixture().await?;
    let expected = serde_json::json!({
        "posture": "idle",
        "reason": "no queued input, active turn, runnable work, or active waits"
    });
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            snapshot_entry(&host, roster)?
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        let bytes = phase.finish()?.0;
        drop((phase, holder));
        struct Checked<'a> {
            posture: &'a crate::types::AgentPostureProjection,
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
                    return Err(serde::ser::Error::custom("posture fixture failure"));
                }
                serde::Serialize::serialize(self.posture, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/posture-fixture",
                &Checked {
                    posture: &agent.scheduling_posture,
                    ledger: &ledger,
                    bytes,
                    fail,
                },
            );
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                assert_eq!(serde_json::from_slice::<Value>(&bytes)?, expected);
            }
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, bytes);
        }
        drop(agent);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, bytes);
        drop(retained);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    }
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    for (uri, pointer) in [
        ("/api/agents/snapshot", "/agents/0/agent/scheduling_posture"),
        (
            "/api/agents/web/projection-snapshot",
            "/projection/agent/scheduling_posture",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value.pointer(pointer), Some(&expected));
    }
    Ok(())
}
