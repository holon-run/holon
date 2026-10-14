use super::*;
use crate::types::CallbackDeliveryMode;

#[tokio::test]
async fn cold_summary_and_compact_http_projection_keep_exact_scheduling_until_wire() -> Result<()> {
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let agent_id = runtime.instance_agent_id().to_owned();
    drop(runtime);
    host.shutdown().await?;
    drop(host);
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let summary = cold
        .local_agent_summary(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(summary.active_external_triggers.len(), 1);
    assert_eq!(
        summary.active_external_triggers[0].external_trigger_id,
        trigger.external_trigger_id
    );
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let original = serde_json::to_value(&*summary)?;
    let wire = serde_json::to_value(&summary)?;
    assert_eq!(wire, original);
    assert_eq!(wire["agent"]["id"], agent_id);
    // This is the actual HTTP synchronous serializer, consuming the carrier.
    let response =
        crate::http::traced_json("summary-owned-test", std::time::Instant::now(), summary)
            .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let compact = cold
        .local_agent_lightweight_projection(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(compact._scheduling_material.is_some());
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let response = crate::http::traced_json(
        "compact-owned-test",
        std::time::Instant::now(),
        crate::http_dto::SlimAgentDto::from(&compact),
    )
    .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(compact);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_no_state_known_timer_is_typed_and_never_becomes_empty_ready() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    host.runtime_db().transaction(|tx| {
        tx.execute("INSERT INTO scheduling_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability) VALUES(?1,?2,'timer','summary-known-absent',1,'absent')",
            rusqlite::params![identity.agent_id, identity.incarnation])?;
        Ok(())
    })?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    for full in [true, false] {
        let error = if full {
            host.local_agent_summary(&identity.agent_id).await.err()
        } else {
            host.local_agent_lightweight_projection(&identity.agent_id)
                .await
                .err()
        }
        .context("known NoState Scheduling cannot display empty")?;
        assert!(
            matches!(error,PublicAgentError::Runtime(ref error)
            if matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive))),
            "{error:?}"
        );
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    let current: usize = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_current WHERE agent_id=?1",
        [&identity.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(current, 0);
    host.shutdown().await?;
    Ok(())
}
