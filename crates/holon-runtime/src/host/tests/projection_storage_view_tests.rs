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
async fn snapshot_storage_view_propagates_sql_errors_without_partial_entry_or_phase_reset(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    let connection = host.runtime_db().connection()?;
    for roster in [false, true] {
        connection.execute(
            "ALTER TABLE queue_entries RENAME TO unavailable_queue_entries",
            [],
        )?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for status in [AgentStatus::AwakeIdle, AgentStatus::Stopped] {
                store_state(&host, status)?;
                let error = entry(&host, roster).unwrap_err();
                let sql = error.downcast_ref::<rusqlite::Error>().unwrap();
                assert_eq!(sql.to_string(), "no such table: queue_entries");
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
            }
            let used = phase.finish()?;
            assert!(used.0 > 0);
            connection.execute(
                "ALTER TABLE unavailable_queue_entries RENAME TO queue_entries",
                [],
            )?;
            let (agent, retained) = entry(&host, roster)?;
            assert_eq!(
                agent.scheduling_posture.posture,
                AgentSchedulingPosture::Stopped
            );
            assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
            assert!(phase.finish()?.0 > used.0);
            drop((agent, retained));
        }
        drop(phase);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
