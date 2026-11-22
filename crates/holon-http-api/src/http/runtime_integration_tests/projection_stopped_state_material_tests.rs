use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-stopped-state-material".into(),
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
    let connection = host.runtime_db().connection()?;
    connection.execute("DELETE FROM agent_states WHERE agent_id = 'web'", [])?;
    connection.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('stopped-state-brief', 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [],
    )?;
    Ok((home, host))
}

#[tokio::test]
async fn missing_state_snapshots_and_http_keep_original_holder_during_serialization() -> Result<()>
{
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host) = fixture().await?;
    let mut expected = None;
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                assert_eq!(snapshot.agents.len(), 1);
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (snapshot.agents.pop().unwrap().agent, snapshot._material)
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (snapshot.agent, snapshot._material)
            }
        };
        assert_eq!(agent.status, AgentStatus::Stopped);
        assert_eq!(agent.identity.agent_id, "web");
        let wire = serde_json::to_value(&agent)?;
        if let Some(expected) = &expected {
            assert_eq!(&wire, expected);
        } else {
            expected = Some(wire);
        }
        let cost = phase.finish()?;
        drop((phase, holder));
        struct Checked<'a> {
            agent: &'a AgentListEntry,
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
                    return Err(serde::ser::Error::custom("stopped state fixture failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/stopped-state-fixture",
                &Checked {
                    agent: &agent,
                    ledger: &ledger,
                    bytes: cost.0,
                    fail,
                },
            );
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    *expected.as_ref().unwrap()
                );
            }
            assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    }
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    for (uri, pointer) in [
        ("/api/agents/snapshot", "/agents/0/agent"),
        ("/api/agents/web/projection-snapshot", "/projection/agent"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value.pointer(pointer), expected.as_ref());
        assert!(!String::from_utf8(body.to_vec())?.contains("_material"));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
