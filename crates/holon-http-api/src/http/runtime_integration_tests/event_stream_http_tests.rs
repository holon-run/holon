use super::*;
use crate::runtime::http_test_support::qualified_event_history_fixture as fixture;
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::runtime_db::current_query::append_qualified_event_for_fixture;
use anyhow::Context;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
};
use futures_util::StreamExt;

#[tokio::test]
async fn qualified_stream_http_replay_preserves_cursor_order_envelope_and_releases_on_cancel(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    let response = crate::http::events_stream(
        Path("default".into()),
        State(Arc::new(crate::http::AppState::for_tcp(host.clone()))),
        axum::http::HeaderMap::new(),
        Query(serde_json::from_value(
            serde_json::json!({"after_seq": 1, "limit": 3}),
        )?),
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
    let mut body = response.into_body().into_data_stream();
    for seq in 2..=4 {
        let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await?
            .context("SSE replay ended")??;
        let text = std::str::from_utf8(&bytes)?;
        assert!(text.starts_with(&format!("id: {seq}\nevent: qualified-history\ndata: ")));
        let value: serde_json::Value =
            serde_json::from_str(text.split_once("\ndata: ").unwrap().1.trim_end())?;
        assert_eq!(value["event_seq"], seq);
        assert_eq!(value["agent_id"], "default");
        assert_eq!(value["payload"]["index"], seq - 1);
        assert!(value["ts"].as_str().is_some());
        assert!(value["event_log_epoch"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
    drop(body);
    tokio::time::timeout(Duration::from_secs(2), async {
        while crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()
            .unwrap()
            .observed_bytes
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn qualified_stream_http_missing_cursor_uses_original_finite_recovery_window() -> Result<()> {
    let (_home, host) = fixture().await?;
    for after in [1, 999] {
        let error = crate::http::events_stream(
            Path("default".into()),
            State(Arc::new(crate::http::AppState::for_tcp(host.clone()))),
            axum::http::HeaderMap::new(),
            Query(serde_json::from_value(
                serde_json::json!({"after_seq": after, "limit": 1}),
            )?),
        )
        .await
        .err()
        .context("missing replay cursor must be refused")?;
        assert_eq!(error.0, axum::http::StatusCode::NOT_FOUND);
        let value = error.1 .0;
        assert_eq!(value["code"], "cursor_not_found");
        assert_eq!(value["after_seq"], after);
        assert_eq!(value["event_seq"], after);
        assert_eq!(value["event_head_seq"], 4);
        assert_eq!(value["oldest_retained_seq"], 0);
        assert!(value["event_log_epoch"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            crate::runtime::http_test_support::input_evidence_budget(&host)
                .report_for_http_test()?
                .observed_bytes,
            0
        );
    }
    assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn qualified_stream_http_live_ack_avoids_body_sql_and_raw_notification_closes() -> Result<()>
{
    let (_home, host) = fixture().await?;
    let storage = host.agent_storage("default")?;
    let response = crate::http::events_stream(
        Path("default".into()),
        State(Arc::new(crate::http::AppState::for_tcp(host.clone()))),
        axum::http::HeaderMap::new(),
        Query(serde_json::from_value(serde_json::json!({}))?),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
    .into_response();
    let mut body = response.into_body().into_data_stream();
    let identity = crate::runtime::http_test_support::active_identity(&host, "default")?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    let effects = append_qualified_event_for_fixture(
        host.runtime_db(),
        &scope,
        &crate::runtime::http_test_support::input_evidence_budget(&host),
        &AuditEvent::legacy(
            "live-qualified",
            serde_json::json!({
                "visible": "unchanged", "provider_request_id": "private"
            }),
        ),
    )?;
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_live_events;
         ALTER TABLE agent_identities RENAME TO forbidden_live_identity;",
    )?;
    assert_eq!(
        crate::runtime::http_test_support::publish_event_fixture(&storage, &effects),
        0
    );
    let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await?
        .context("qualified live event was lost")??;
    let text = std::str::from_utf8(&bytes)?;
    assert!(text.starts_with("id: 5\nevent: live-qualified\ndata: "));
    let value: serde_json::Value =
        serde_json::from_str(text.split_once("\ndata: ").unwrap().1.trim_end())?;
    assert_eq!(value["payload"]["visible"], "unchanged");
    assert!(value["payload"].get("provider_request_id").is_none());
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE forbidden_live_events RENAME TO audit_events;
         ALTER TABLE forbidden_live_identity RENAME TO agent_identities;",
    )?;
    drop((effects, bytes));
    storage.append_event(&AuditEvent::legacy(
        "unqualified-live",
        serde_json::json!({}),
    ))?;
    assert!(tokio::time::timeout(Duration::from_secs(2), body.next())
        .await?
        .is_none());
    drop(body);
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
