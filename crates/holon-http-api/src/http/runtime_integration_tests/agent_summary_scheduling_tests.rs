use super::*;
use crate::provider::StubProvider;
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
    assert!(fixture::has_no_loaded_agents(&cold).await);
    let baseline = fixture::input_evidence_budget(&cold)
        .report_for_http_test()?
        .observed_bytes;
    let summary = cold
        .local_agent_summary(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(summary.active_external_triggers.len(), 1);
    assert_eq!(
        summary.active_external_triggers[0].external_trigger_id,
        trigger.external_trigger_id
    );
    assert!(
        fixture::input_evidence_budget(&cold)
            .report_for_http_test()?
            .observed_bytes
            > baseline
    );
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
        fixture::input_evidence_budget(&cold)
            .report_for_http_test()?
            .observed_bytes,
        baseline
    );
    let compact = cold
        .local_agent_lightweight_projection(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(compact.has_scheduling_material_for_http_test());
    assert!(
        fixture::input_evidence_budget(&cold)
            .report_for_http_test()?
            .observed_bytes
            > baseline
    );
    let response = crate::http::traced_json(
        "compact-owned-test",
        std::time::Instant::now(),
        crate::http_dto::SlimAgentDto::from(&compact),
    )
    .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        fixture::input_evidence_budget(&cold)
            .report_for_http_test()?
            .observed_bytes
            > baseline
    );
    drop(compact);
    assert_eq!(
        fixture::input_evidence_budget(&cold)
            .report_for_http_test()?
            .observed_bytes,
        baseline
    );
    assert!(fixture::has_no_loaded_agents(&cold).await);
    cold.shutdown().await?;
    Ok(())
}
