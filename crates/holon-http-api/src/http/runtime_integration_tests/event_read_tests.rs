use super::*;
use crate::runtime::http_test_support::qualified_event_history_fixture as fixture;

#[tokio::test]
async fn finite_history_http_page_uses_qualified_read_and_retains_its_response_body() -> Result<()>
{
    use axum::{
        extract::{Path, Query, State},
        response::IntoResponse,
    };
    let (_home, host) = fixture().await?;
    let state = Arc::new(crate::http::AppState::for_tcp(host.clone()));
    let response = crate::http::events(
        Path("default".to_owned()),
        State(Arc::clone(&state)),
        axum::http::HeaderMap::new(),
        Query(serde_json::from_value(serde_json::json!({
            "limit": 2, "order": "asc", "event_kind": "qualified-history"
        }))?),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["cursor_seq"], 4);
    assert_eq!(value["oldest_seq"], 1);
    assert_eq!(value["newest_seq"], 2);
    assert_eq!(value["has_newer"], true);
    assert_eq!(value["has_older"], false);
    assert_eq!(value["next_after_seq"], 2);
    assert!(value["next_before_seq"].is_null());
    assert_eq!(value["events"][0]["payload"]["index"], 0);
    assert_eq!(value["events"].as_array().unwrap().len(), 2);
    assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
    assert!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes
            > 0
    );
    drop(bytes);
    assert_eq!(
        crate::runtime::http_test_support::input_evidence_budget(&host)
            .report_for_http_test()?
            .observed_bytes,
        0
    );
    for (query, expected, next_before) in [
        (
            serde_json::json!({"order": "asc", "after_seq": 2}),
            vec![3, 4],
            None,
        ),
        (serde_json::json!({"order": "desc"}), vec![4, 3], Some(3)),
        (
            serde_json::json!({"order": "desc", "before_seq": 3}),
            vec![2, 1],
            None,
        ),
    ] {
        let mut query = query;
        query["limit"] = 2.into();
        query["event_kind"] = "qualified-history".into();
        let response = crate::http::events(
            Path("default".to_owned()),
            State(Arc::clone(&state)),
            axum::http::HeaderMap::new(),
            Query(serde_json::from_value(query)?),
        )
        .await
        .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
        .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["cursor_seq"], 4, "high water is not a paging cursor");
        assert_eq!(value["next_before_seq"], serde_json::json!(next_before));
        assert!(value["next_after_seq"].is_null());
        assert_eq!(value["has_older"], next_before.is_some());
        assert_eq!(value["has_newer"], false);
        assert_eq!(
            value["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["event_seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            value["oldest_seq"],
            serde_json::json!(expected.iter().min())
        );
        assert_eq!(
            value["newest_seq"],
            serde_json::json!(expected.iter().max())
        );
        drop(bytes);
        assert_eq!(
            crate::runtime::http_test_support::input_evidence_budget(&host)
                .report_for_http_test()?
                .observed_bytes,
            0
        );
    }
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn finite_history_http_budget_returns_partial_then_nonretryable_resource_error() -> Result<()>
{
    use axum::{
        extract::{Path, Query, State},
        response::IntoResponse,
    };
    let (_home, host) = fixture().await?;
    let oversized = AuditEvent::legacy(
        "oversized-history",
        serde_json::json!({
            "secret": "event-page-body-must-not-leak",
            "padding": "x".repeat(8 * 1024 * 1024),
        }),
    );
    host.runtime_db()
        .audit_events()
        .append(Some("default"), &oversized)?;
    host.runtime_db().audit_events().append(
        Some("default"),
        &AuditEvent::legacy("after-oversized", serde_json::json!({})),
    )?;
    let state = Arc::new(crate::http::AppState::for_tcp(host.clone()));
    for (query, expected, direction, next_query) in [
        (
            serde_json::json!({"order": "asc", "after_seq": 3}),
            4,
            "next_after_seq",
            serde_json::json!({"order": "asc", "after_seq": 4}),
        ),
        (
            serde_json::json!({"order": "desc"}),
            6,
            "next_before_seq",
            serde_json::json!({"order": "desc", "before_seq": 6}),
        ),
    ] {
        let response = crate::http::events(
            Path("default".to_owned()),
            State(Arc::clone(&state)),
            axum::http::HeaderMap::new(),
            Query(serde_json::from_value(query)?),
        )
        .await
        .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
        .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["events"].as_array().unwrap().len(), 1);
        assert_eq!(value["oldest_seq"], expected);
        assert_eq!(value["newest_seq"], expected);
        assert_eq!(value["cursor_seq"], 6);
        assert_eq!(value[direction], expected);
        assert_eq!(
            value[if direction == "next_after_seq" {
                "has_newer"
            } else {
                "has_older"
            }],
            true
        );
        drop(bytes);
        assert_eq!(
            crate::runtime::http_test_support::input_evidence_budget(&host)
                .report_for_http_test()?
                .observed_bytes,
            0
        );

        let result = crate::http::events(
            Path("default".to_owned()),
            State(Arc::clone(&state)),
            axum::http::HeaderMap::new(),
            Query(serde_json::from_value(next_query)?),
        )
        .await;
        let (status, body) = result
            .err()
            .context("oversized first candidate must fail")?;
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0["code"], "event_page_resource_exhausted");
        assert_eq!(body.0["domain"], "runtime");
        assert_eq!(body.0["retryable"], false);
        assert!(body.0.get("next_before_seq").is_none());
        assert!(body.0.get("next_after_seq").is_none());
        assert!(!body.0.to_string().contains("event-page-body-must-not-leak"));
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
