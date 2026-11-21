use super::super::*;
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

fn header() -> Result<(u64, usize)> {
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

fn id_cost(id: &str) -> (u64, usize) {
    ((id.len() + std::mem::size_of::<String>()) as u64 * 8, 8)
}

#[test]
fn stopped_placeholder_without_phase_preserves_complete_state_and_wire() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for id in ["web", "代理-🦀", "long-agent-id"] {
        let expected = stopped_unloaded_agent(id);
        let actual = try_stopped_unloaded_agent(id)?;
        assert_eq!(actual, expected);
        assert_eq!(actual.id, id);
        assert_eq!(actual.status, AgentStatus::Stopped);
        assert_eq!(
            serde_json::to_value(actual)?,
            serde_json::to_value(expected)?
        );
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn stopped_placeholder_exact_cost_keeps_original_phase_and_last_holder() -> Result<()> {
    let initial = header()?;
    for id in ["web", "代理-🦀", "long-agent-id"] {
        let cost = id_cost(id);
        let expected = (initial.0 + cost.0, initial.1 + cost.1);
        let ledger = ledger()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
        let holder = phase.guard()?;
        let state = {
            let _entry = phase.enter();
            let state = try_stopped_unloaded_agent(id)?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            state
        };
        assert_eq!(state, stopped_unloaded_agent(id));
        assert_eq!(phase.finish()?, expected);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(state);
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn stopped_placeholder_rejects_before_copy_and_latches_both_limits() -> Result<()> {
    let initial = header()?;
    let cost = id_cost("web");
    let full = (initial.0 + cost.0, initial.1 + cost.1);
    for limits in [(full.0 - 1, full.1), (full.0, full.1 - 1)] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            for _ in 0..2 {
                let error = try_stopped_unloaded_agent("web").unwrap_err();
                assert!(format!("{error:#}").contains("context resource read material"));
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert_eq!(ledger.report()?.observed_bytes, initial.0);
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, initial.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
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
async fn snapshot_entry_only_charges_missing_state_and_propagates_rejection() -> Result<()> {
    let (_home, host) = fixture().await?;
    let identity = host.agent_identity_record("web")?.unwrap();
    let catalog = RuntimeModelCatalog::from_config(&host.config());
    let mut costs = Vec::new();
    let mut entries = Vec::new();
    let mut retained = Vec::new();
    for missing in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let identity = identity.clone();
        let state = (!missing).then(|| stopped_unloaded_agent("web"));
        let entry = {
            let _entry = phase.enter();
            host.agent_list_entry_from_committed_state(identity, state, &catalog)?
        };
        costs.push(phase.finish()?);
        entries.push(entry);
        retained.push((ledger, phase));
    }
    let id = id_cost("web");
    assert_eq!(costs[1], (costs[0].0 + id.0, costs[0].1 + id.1));
    assert_eq!(entries[0], entries[1]);
    assert_eq!(
        serde_json::to_value(&entries[0])?,
        serde_json::to_value(&entries[1])?
    );
    drop(entries);
    for (ledger, phase) in retained {
        assert_eq!(ledger.report()?.observed_bytes, phase.finish()?.0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let initial = header()?;
    for limits in [
        (initial.0 + id.0 - 1, initial.1 + id.1),
        (initial.0 + id.0, initial.1 + id.1 - 1),
    ] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for _ in 0..2 {
                let error = host
                    .agent_list_entry_from_committed_state(identity.clone(), None, &catalog)
                    .unwrap_err();
                assert!(format!("{error:#}").contains("context resource read material"));
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert_eq!(ledger.report()?.observed_bytes, initial.0);
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, initial.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
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
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
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
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
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

#[tokio::test]
async fn missing_state_anchor_failure_retries_same_phase_and_releases_last_holder() -> Result<()> {
    let (_home, host) = fixture().await?;
    for roster in [false, true] {
        host.runtime_db().connection()?.execute(
            "UPDATE briefs SET created_at = 'invalid' WHERE evidence_id = 'stopped-state-brief'",
            [],
        )?;
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = if roster {
                host.agent_roster_snapshot().unwrap_err()
            } else {
                host.agent_projection_snapshot("web").unwrap_err()
            };
            assert!(
                format!("{error:#}").contains("unreadable latest brief timestamp for web"),
                "{error:#}"
            );
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        let failed = phase.finish()?;
        host.runtime_db().connection()?.execute(
            "UPDATE briefs SET created_at = '2026-02-01T00:00:00.000Z'
             WHERE evidence_id = 'stopped-state-brief'",
            [],
        )?;
        let (agent, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                (snapshot.agents.pop().unwrap().agent, snapshot._material)
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                (snapshot.agent, snapshot._material)
            }
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        let full = phase.finish()?;
        assert!(full.0 > failed.0);
        assert!(full.1 > failed.1);
        assert_eq!(agent.status, AgentStatus::Stopped);
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
