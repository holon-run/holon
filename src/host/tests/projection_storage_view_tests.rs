use super::super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentSchedulingPosture,
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    store_state(&host, AgentStatus::AwakeIdle)?;
    Ok((home, host))
}

fn store_state(host: &RuntimeHost, status: AgentStatus) -> Result<()> {
    let mut agent = AgentState::new("web");
    agent.status = status;
    host.runtime_db().connection()?.execute(
        "INSERT OR REPLACE INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', ?1, '2026-02-01T00:00:00.000Z', ?2)",
        rusqlite::params![
            serde_json::to_value(&agent.status)?.as_str().unwrap(),
            serde_json::to_string(&agent)?,
        ],
    )?;
    Ok(())
}

fn entry(
    host: &RuntimeHost,
    roster: bool,
) -> Result<(AgentListEntry, Option<Arc<BudgetViewGuard>>)> {
    if roster {
        let mut snapshot = host.agent_roster_snapshot()?;
        assert_eq!(snapshot.agents.len(), 1);
        Ok((snapshot.agents.pop().unwrap().agent, snapshot._material))
    } else {
        let snapshot = host.agent_projection_snapshot("web")?.unwrap();
        Ok((snapshot.agent, snapshot._material))
    }
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-storage-view".into(),
        identity_incarnation: 1,
    }
}

#[tokio::test]
async fn snapshot_storage_view_keeps_original_holder_through_wire_success_and_failure() -> Result<()>
{
    let (_home, host) = fixture().await?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::Idle
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
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
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
                if self.fail {
                    return Err(serde::ser::Error::custom("storage-view fixture failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/storage-view-fixture",
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
                    serde_json::to_value(&agent)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_storage_view_propagates_sql_errors_without_partial_entry_or_phase_reset(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    host.runtime_db()
        .connection()?
        .execute("DROP TABLE queue_entries", [])?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = entry(&host, roster).unwrap_err();
            let sql = error.downcast_ref::<rusqlite::Error>().unwrap();
            assert_eq!(sql.to_string(), "no such table: queue_entries");
            let used = phase.finish()?;
            assert!(used.0 > 0);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            // Early posture selection still precedes the unavailable queue.
            store_state(&host, AgentStatus::Stopped)?;
            let (agent, retained) = entry(&host, roster)?;
            assert_eq!(
                agent.scheduling_posture.posture,
                AgentSchedulingPosture::Stopped
            );
            assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
            assert!(phase.finish()?.0 > used.0);
            drop((agent, retained));
        }
        store_state(&host, AgentStatus::AwakeIdle)?;
        drop(phase);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_storage_view_preserves_both_real_http_routes() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let (_home, host) = fixture().await?;
    let expected = serde_json::to_value(entry(&host, false)?.0)?;
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
        assert_eq!(value.pointer(pointer), Some(&expected));
    }
    Ok(())
}
