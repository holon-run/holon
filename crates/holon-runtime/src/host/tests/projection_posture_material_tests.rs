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

#[tokio::test]
async fn roster_uses_per_agent_allowances_and_retains_them_through_last_wire_owner() -> Result<()> {
    let (_home, host) = fixture().await?;
    for index in 0..16 {
        let id = format!("history-{index}");
        host.create_named_agent(&id, None).await?;
        let mut state = AgentState::new(&id);
        state.status = AgentStatus::AwakeIdle;
        state.working_memory.current_working_memory.recent_decisions =
            vec!["retained metadata".into(); 512];
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&state)?;
    }
    let mut snapshot = host.agent_roster_snapshot()?;
    assert_eq!(snapshot.agents.len(), 17);
    assert_eq!(snapshot.agent_materials.len(), 17);
    assert!(
        snapshot
            .agent_materials
            .iter()
            .map(|source| source.finish().unwrap().1)
            .sum::<usize>()
            > 65536
    );
    assert!(snapshot
        .agent_materials
        .iter()
        .all(|source| source.finish().unwrap().1 <= 65536));
    let sources = std::mem::take(&mut snapshot.agent_materials);
    let weak = sources
        .iter()
        .map(|source| source.guard().map(|guard| Arc::downgrade(&guard)))
        .collect::<Result<Vec<_>>>()?;
    let bytes = crate::host::observer_json_bytes_with_sources(
        &snapshot
            .agents
            .iter()
            .map(|entry| &entry.agent)
            .collect::<Vec<_>>(),
        snapshot.acquisition.clone(),
        sources,
    )?;
    drop(snapshot);
    assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
    let last = bytes.clone();
    drop(bytes);
    assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
    drop(last);
    assert!(weak.iter().all(|guard| guard.upgrade().is_none()));
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
