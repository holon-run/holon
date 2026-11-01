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

const HINT: &str = "agent is administratively stopped; start before new prompts or wakes";

fn statuses() -> [AgentStatus; 6] {
    [
        AgentStatus::Booting,
        AgentStatus::AwakeIdle,
        AgentStatus::AwakeRunning,
        AgentStatus::AwaitingTask,
        AgentStatus::Asleep,
        AgentStatus::Stopped,
    ]
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-lifecycle-material".into(),
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

fn hint_cost() -> (u64, usize) {
    ((HINT.len() + std::mem::size_of::<String>()) as u64 * 8, 8)
}

#[test]
fn checked_lifecycle_matches_all_statuses_and_wire_without_active_phase() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for status in statuses() {
        let expected = AgentLifecycleHint::from_status("web", status.clone());
        let actual = AgentLifecycleHint::try_from_status("web", status.clone())?;
        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_value(&actual)?,
            serde_json::to_value(&expected)?
        );
        assert_eq!(
            actual.accepts_external_messages,
            status != AgentStatus::Stopped
        );
        assert_eq!(
            actual.operator_hint.as_deref(),
            (status == AgentStatus::Stopped).then_some(HINT)
        );
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn lifecycle_exact_cost_and_allocation_free_statuses_use_original_phase() -> Result<()> {
    let initial = header()?;
    let hint = hint_cost();
    for status in statuses() {
        let expected = if status == AgentStatus::Stopped {
            (initial.0 + hint.0, initial.1 + hint.1)
        } else {
            initial
        };
        let ledger = ledger()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
        let holder = phase.guard()?;
        let lifecycle = {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            AgentLifecycleHint::try_from_status("web", status.clone())?
        };
        assert_eq!(phase.finish()?, expected);
        assert_eq!(lifecycle, AgentLifecycleHint::from_status("web", status));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(lifecycle);
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn lifecycle_bytes_and_records_reject_before_hint_and_latch_same_phase() -> Result<()> {
    let initial = header()?;
    let hint = hint_cost();
    let full = (initial.0 + hint.0, initial.1 + hint.1);
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
                let error =
                    AgentLifecycleHint::try_from_status("web", AgentStatus::Stopped).unwrap_err();
                assert!(
                    error.to_string().contains("context resource read material"),
                    "{error}"
                );
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
    state.status = AgentStatus::Stopped;
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'stopped', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    connection.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('lifecycle-brief', 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [],
    )?;
    Ok((home, host, state))
}

#[tokio::test]
async fn snapshot_entry_propagates_lifecycle_gate_failure_without_partial_return() -> Result<()> {
    let (_home, host, state) = fixture().await?;
    let identity = host.agent_identity_record("web")?.unwrap();
    let catalog = RuntimeModelCatalog::from_config(&host.config());
    let full = {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        {
            let _entry = phase.enter();
            let agent = host.agent_list_entry_from_committed_state(
                identity.clone(),
                Some(state.clone()),
                &catalog,
            )?;
            assert_eq!(agent.lifecycle.operator_hint.as_deref(), Some(HINT));
        }
        let full = phase.finish()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        full
    };
    let hint = hint_cost();
    for limits in [(full.0 - 1, full.1), (full.0, full.1 - 1)] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for _ in 0..2 {
                let error = host
                    .agent_list_entry_from_committed_state(
                        identity.clone(),
                        Some(state.clone()),
                        &catalog,
                    )
                    .unwrap_err();
                assert!(
                    format!("{error:#}").contains("context resource read material"),
                    "{error:#}"
                );
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert_eq!(ledger.report()?.observed_bytes, full.0 - hint.0);
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, full.0 - hint.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn lifecycle_snapshots_and_http_keep_hint_and_original_holder_through_serialization(
) -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host, _) = fixture().await?;
    let expected = AgentLifecycleHint::from_status("web", AgentStatus::Stopped);
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (lifecycle, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                assert_eq!(snapshot.agents.len(), 1);
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (
                    snapshot.agents.pop().unwrap().agent.lifecycle,
                    snapshot._material,
                )
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (snapshot.agent.lifecycle, snapshot._material)
            }
        };
        assert_eq!(lifecycle, expected);
        let cost = phase.finish()?;
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        struct Checked<'a> {
            lifecycle: &'a AgentLifecycleHint,
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
                    return Err(serde::ser::Error::custom("lifecycle fixture failure"));
                }
                serde::Serialize::serialize(self.lifecycle, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/lifecycle-fixture",
                &Checked {
                    lifecycle: &lifecycle,
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
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(lifecycle);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    for (uri, pointer) in [
        ("/api/agents/snapshot", "/agents/0/agent/lifecycle"),
        (
            "/api/agents/web/projection-snapshot",
            "/projection/agent/lifecycle",
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

#[tokio::test]
async fn lifecycle_anchor_failure_retries_on_same_phase_and_releases_last_holder() -> Result<()> {
    let (_home, host, _) = fixture().await?;
    // Projection parses this anchor after constructing the hint; roster parses
    // it first. Both preserve the original phase across ordinary parse errors.
    for roster in [false, true] {
        host.runtime_db().connection()?.execute(
            "UPDATE briefs SET created_at = 'invalid' WHERE evidence_id = 'lifecycle-brief'",
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
            "UPDATE briefs SET created_at = '2026-02-01T00:00:00.000Z' WHERE evidence_id = 'lifecycle-brief'", [],
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
        assert_eq!(agent.lifecycle.operator_hint.as_deref(), Some(HINT));
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        let full = phase.finish()?;
        assert!(full.0 > failed.0 && full.1 > failed.1);
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
