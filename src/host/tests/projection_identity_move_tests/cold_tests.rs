use super::*;
use crate::{
    host::tests::test_host,
    types::{TimerRecord, TimerStatus},
};

async fn cold_fixture(
    named: bool,
    present_state: bool,
) -> Result<(tempfile::TempDir, RuntimeHost, AgentIdentityRecord)> {
    let (home, host) = test_host();
    let identity = if named {
        let (_, created) = host
            .ensure_named_agent(
                "cold-identity",
                None,
                NamedAgentExistingBehavior::Reuse,
                None,
                AgentBootstrapDesiredState {
                    template: None,
                    catalog_agent_home: None,
                    workspace: None,
                    model_resolution: None,
                    initial_message: None,
                },
            )
            .await?;
        assert!(created);
        host.rename_public_agent("cold-identity", "冷 identity 汉🦀", "fixture")
            .map_err(|error| anyhow!("{error}"))?;
        host.active_agent_identity("cold-identity")
            .map_err(|error| anyhow!("{error}"))?
    } else {
        host.ensure_default_agent_identity()?
    };
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    if present_state {
        let mut state = AgentState::new(&identity.agent_id);
        state.status = AgentStatus::Asleep;
        state.last_wake_reason = Some("preserved cold State 汉🦀".repeat(7));
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
    }
    assert_eq!(
        host.runtime_db()
            .agent_states()
            .latest(&identity.agent_id)?
            .is_some(),
        present_state
    );
    Ok((home, host, identity))
}

fn assert_identity(actual: &AgentIdentityView, expected: &AgentIdentityView) -> Result<()> {
    assert_eq!(actual, expected);
    assert_eq!(
        serde_json::to_value(actual)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}

#[tokio::test]
async fn cold_summary_identity_matches_borrowed_view_and_last_holder() -> Result<()> {
    for named in [false, true] {
        for present in [false, true] {
            let (_home, host, identity) = cold_fixture(named, present).await?;
            let expected =
                AgentIdentityView::from_record(&identity, &host.config().default_agent_id);
            let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
            let read = host
                .local_agent_summary(&identity.agent_id)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            assert_identity(&read.identity, &expected)?;
            let held = host.inner.input_evidence_budget.report()?.observed_bytes;
            assert!(held > baseline);
            let first = Arc::new(read);
            let last = first.clone();
            drop(first);
            assert_identity(&last.identity, &expected)?;
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                held
            );
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            drop(last);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                baseline
            );
            host.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn cold_lightweight_identity_matches_borrowed_view_and_last_holder() -> Result<()> {
    for named in [false, true] {
        for present in [false, true] {
            let (_home, host, identity) = cold_fixture(named, present).await?;
            let expected =
                AgentIdentityView::from_record(&identity, &host.config().default_agent_id);
            let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
            let read = host
                .local_agent_lightweight_projection(&identity.agent_id)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            assert_identity(&read.identity, &expected)?;
            assert!(read._scheduling_material.is_some());
            let held = host.inner.input_evidence_budget.report()?.observed_bytes;
            assert!(held > baseline);
            let first = Arc::new(read);
            let last = first.clone();
            drop(first);
            assert_identity(&last.identity, &expected)?;
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                held
            );
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            drop(last);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                baseline
            );
            host.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn cold_operator_state_identity_matches_borrowed_view_and_last_holder() -> Result<()> {
    for named in [false, true] {
        for present in [false, true] {
            let (_home, host, identity) = cold_fixture(named, present).await?;
            let expected =
                AgentIdentityView::from_record(&identity, &host.config().default_agent_id);
            let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
            let read = host
                .operator_agent_state_projection(&identity.agent_id, 0, 0)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            assert!(matches!(read.source, AgentStateProjectionSource::Storage));
            assert_identity(&read.agent.identity, &expected)?;
            assert!(read.material._scheduling.is_some());
            assert!(read.agent._scheduling_material.is_none());
            let held = host.inner.input_evidence_budget.report()?.observed_bytes;
            assert!(held > baseline);
            let first = Arc::new(read);
            let last = first.clone();
            drop(first);
            assert_identity(&last.agent.identity, &expected)?;
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                held
            );
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            drop(last);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                baseline
            );
            host.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn cold_identity_entrypoints_keep_missing_identity_and_scheduling_rejection() -> Result<()> {
    let (_home, host, identity) = cold_fixture(false, false).await?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let errors = [
        host.local_agent_summary("missing").await.err().unwrap(),
        host.local_agent_lightweight_projection("missing")
            .await
            .err()
            .unwrap(),
        host.operator_agent_state_projection("missing", 0, 0)
            .await
            .err()
            .unwrap(),
    ];
    for error in errors {
        assert!(matches!(error, PublicAgentError::NotFound { agent_id } if agent_id == "missing"));
    }
    let timer = TimerRecord {
        id: "unqualified-cold-identity".into(),
        agent_id: identity.agent_id.clone(),
        created_at: Utc::now(),
        duration_ms: 3_600_000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: None,
        last_fired_at: None,
        fire_count: 0,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &timer)?;
        Ok(())
    })?;
    let errors = [
        host.local_agent_summary(&identity.agent_id)
            .await
            .err()
            .unwrap(),
        host.local_agent_lightweight_projection(&identity.agent_id)
            .await
            .err()
            .unwrap(),
        host.operator_agent_state_projection(&identity.agent_id, 0, 0)
            .await
            .err()
            .unwrap(),
    ];
    for error in errors {
        assert!(matches!(error, PublicAgentError::Runtime(ref error)
        if matches!(
            error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
            Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)
        )));
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
