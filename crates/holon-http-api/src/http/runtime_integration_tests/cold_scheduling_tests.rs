use super::*;
use crate::provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider};
use async_trait::async_trait;

#[tokio::test]
async fn cold_operator_state_after_provider_failure_and_stop_keeps_retry_work() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, initial) = test_host();
    let config = initial.config().as_ref().clone();
    initial.shutdown().await?;
    drop(initial);
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(FailedColdStateProvider))?;
    let agent_id = config.default_agent_id.clone();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    runtime.control(ControlAction::Stop).await?;
    let work = runtime
        .create_work_item(
            "retry after provider failure".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;
    runtime.pick_work_item(work.id.clone()).await?;
    host.control_public_agent(&agent_id, ControlAction::Start)
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if host
                .runtime_db()
                .agent_states()
                .latest(&agent_id)?
                .is_some_and(|state| state.last_runtime_failure.is_some())
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    host.control_public_agent(&agent_id, ControlAction::Stop)
        .await?;
    host.shutdown().await?;
    drop((runtime, host));

    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let baseline = crate::runtime::http_test_support::input_evidence_budget(&host)
        .report_for_http_test()?
        .observed_bytes;
    let read = host
        .operator_agent_state_projection(&agent_id, 50, 50)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(matches!(read.source, AgentStateProjectionSource::Storage));
    assert_eq!(read.agent.agent.status, AgentStatus::Stopped);
    assert!(read.agent.agent.last_runtime_failure.is_some());
    assert_eq!(
        read.agent.work_queue.current.as_ref().map(|item| &item.id),
        Some(&work.id)
    );
    let held = crate::runtime::http_test_support::input_evidence_budget(&host)
        .report_for_http_test()?
        .observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert_eq!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes,
        held
    );
    drop(last);
    assert_eq!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes,
        baseline
    );
    let response = crate::http::router(crate::http::AppState::for_tcp(host.clone()))
        .oneshot(
            Request::builder()
                .uri(format!("/api/agents/{agent_id}/state"))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
    host.shutdown().await?;
    Ok(())
}

struct FailedColdStateProvider;

#[async_trait]
impl AgentProvider for FailedColdStateProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!("cold State provider transport failure")
    }
}
