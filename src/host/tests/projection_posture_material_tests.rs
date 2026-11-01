use super::super::*;
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
async fn host_snapshots_propagate_final_posture_gate_refusal_on_original_phase() -> Result<()> {
    let (_home, host) = fixture().await?;
    // Independent last-gate oracle: no Work/Task/Run IDs, lifecycle hint,
    // or later hydration allocation in this fixture.
    let reason = "no queued input, active turn, runnable work, or active waits";
    let delta = ((reason.len() + std::mem::size_of::<String>()) as u64 * 8, 8);
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let (agent, retained) = {
            let _entry = phase.enter();
            snapshot_entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::Idle
        );
        assert_eq!(agent.scheduling_posture.reason, reason);
        let full = phase.finish()?;
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let before = (full.0 - delta.0, full.1 - delta.1);
        for records in [false, true] {
            let ledger = self::ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                full.0 - u64::from(!records),
                full.1 - usize::from(records),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                for attempt in 0..2 {
                    let error = snapshot_entry(&host, roster).unwrap_err();
                    if attempt == 0 {
                        let io = error.downcast_ref::<std::io::Error>().unwrap();
                        assert_eq!(io.kind(), std::io::ErrorKind::Other);
                        assert_eq!(
                            io.to_string(),
                            "ResourceExhausted: context resource read material"
                        );
                    } else {
                        let runtime = error
                            .downcast_ref::<crate::runtime_error::RuntimeError>()
                            .unwrap();
                        let descriptor = runtime.descriptor();
                        assert_eq!(
                            descriptor.domain,
                            crate::runtime_error::RuntimeErrorDomain::Io
                        );
                        assert_eq!(descriptor.code, "context_resource_exhausted");
                        assert!(descriptor.retryable);
                    }
                    assert_eq!(ledger.report()?.observed_bytes, before.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
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
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
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
            assert_eq!(ledger.report()?.observed_bytes, bytes);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, bytes);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
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
