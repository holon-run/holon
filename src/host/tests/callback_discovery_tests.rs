use super::*;
use crate::types::CallbackDeliveryMode;

#[tokio::test]
async fn callback_locator_is_read_only_and_actual_revocation_fence_prevents_delivery() -> Result<()>
{
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    let capability = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let token = capability
        .trigger_url
        .rsplit('/')
        .next()
        .context("original token URL missing")?;
    // Let the original idle transition finish before measuring callback effects.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if runtime.agent_state().await?.status == AgentStatus::Asleep {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .context("original idle transition did not finish")??;
    let baseline = host.inner.input_evidence_budget.report()?;
    let locator = host
        .resolve_external_trigger_locator(token)?
        .context("original token missing")?;
    assert_eq!(locator.target_agent_id, runtime.instance_agent_id());
    assert_eq!(locator.external_trigger_id, capability.external_trigger_id);
    // Discovery owns Host guards; concurrent runtime scopes are independent.
    assert!(
        host.inner
            .input_evidence_budget
            .report()?
            .host_owned
            .payload_bytes
            > baseline.host_owned.payload_bytes
    );
    drop(locator);
    let released = host.inner.input_evidence_budget.report()?;
    assert_eq!(
        released.host_owned, baseline.host_owned,
        "before: {baseline:?}; after: {released:?}"
    );
    let locator = host
        .resolve_external_trigger_locator(token)?
        .context("second original route missing")?;
    runtime
        .revoke_external_trigger(&locator.external_trigger_id)
        .await?;
    let state = runtime.agent_state().await?;
    let before: (usize, usize) = host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let error = host
        .deliver_external_callback(
            &locator.target_agent_id,
            &locator.external_trigger_id,
            crate::types::CallbackDeliveryPayload {
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
            },
        )
        .await
        .err()
        .context("discovery is not permission past the original revoke fence")?;
    assert!(error.to_string().contains("not active"), "{error:#}");
    assert_eq!(runtime.agent_state().await?, state);
    let after: (usize, usize) = host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(before, after);
    drop(locator);
    assert!(host.resolve_external_trigger_locator(token)?.is_none());
    host.shutdown().await?;
    Ok(())
}
