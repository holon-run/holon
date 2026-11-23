use super::*;

#[tokio::test]
async fn global_hint_http_keeps_raw_event_body_out_of_metadata_stream() -> Result<()> {
    use axum::{extract::State, response::IntoResponse};
    use futures_util::StreamExt;
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let response = crate::http::global_events_stream(
        State(Arc::new(crate::http::AppState::for_tcp(host.clone()))),
        axum::http::HeaderMap::new(),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
    .into_response();
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(
        response.headers()[axum::http::header::CACHE_CONTROL],
        "no-cache"
    );
    let storage = host.agent_storage(&identity.agent_id)?;
    storage.append_event(&crate::types::AuditEvent::legacy(
        "raw-hint-only",
        serde_json::json!({ "private_body": "must never be in global hint" }),
    ))?;
    let mut body = response.into_body().into_data_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await?
        .context("global hint stream ended")??;
    let text = std::str::from_utf8(&bytes)?;
    assert!(text.starts_with("event: agent_roster_hint\ndata: "));
    let hint: serde_json::Value = serde_json::from_str(
        text.split_once("\ndata: ")
            .context("hint framing")?
            .1
            .trim_end(),
    )?;
    assert_eq!(hint, serde_json::json!({ "agent_id": identity.agent_id }));
    assert!(!text.contains("private_body"));
    let slice = bytes.slice(1..);
    drop(bytes);
    drop(body);
    assert!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes
            > 0
    );
    drop(slice);
    assert_eq!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes,
        0
    );
    assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
    host.shutdown().await?;
    Ok(())
}
