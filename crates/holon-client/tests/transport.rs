use anyhow::Result;
use holon_client::{ConnectionConfig, LocalClient, LocalHttpError};
use tokio::net::TcpListener;

#[cfg(unix)]
use holon_client::EventStreamRequest;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tempfile::tempdir;
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

#[tokio::test(flavor = "multi_thread")]
async fn github_rate_limit_envelope_decodes_and_displays_recovery_guidance() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new().route(
        "/api/skills/catalog/add",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                axum::Json(serde_json::json!({
                    "ok": false,
                    "error": "GitHub API rate limit exceeded",
                    "code": "remote_skill_rate_limited",
                    "hint": "Wait 17 seconds before retrying.",
                    "domain": "http",
                    "retryable": true,
                    "upstream_status": 403,
                    "retry_after_seconds": 17
                })),
            )
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let config = ConnectionConfig {
        home_dir: Default::default(),
        default_agent_id: "default".into(),
        socket_path: Default::default(),
        control_token: None,
        http_addr: address.to_string(),
    };
    let client = LocalClient::remote(config, format!("http://{address}"), "test-token")?;
    let result: Result<serde_json::Value> = client
        .post_json(
            "/skills/catalog/add",
            &serde_json::json!({"source": "owner/repo", "remote": true}),
        )
        .await;
    server.abort();
    let error = result.expect_err("rate limits must not decode as success");
    let wire_error = error.downcast_ref::<LocalHttpError>().unwrap();
    assert_eq!(wire_error.status_code, 429);
    assert!(wire_error.has_code("remote_skill_rate_limited"));
    assert_eq!(wire_error.retryable, Some(true));
    assert_eq!(
        wire_error.hint.as_deref(),
        Some("Wait 17 seconds before retrying.")
    );
    let displayed = wire_error.to_string();
    assert!(displayed.contains("GitHub API rate limit"));
    assert!(displayed.contains("[remote_skill_rate_limited]"));
    assert!(displayed.contains("Hint: Wait 17 seconds before retrying."));
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn local_client_unix_failure_does_not_retry_over_http() -> Result<()> {
    let socket_dir = tempdir()?;
    let socket_path = socket_dir.path().join("holon.sock");
    let unix_listener = UnixListener::bind(&socket_path)?;
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;

    let config = ConnectionConfig {
        home_dir: socket_dir.path().to_path_buf(),
        default_agent_id: "default".into(),
        socket_path,
        control_token: None,
        http_addr: http_listener.local_addr()?.to_string(),
    };

    let unix_server = tokio::spawn(async move {
        let (stream, _) = unix_listener.accept().await?;
        drop(stream);
        Ok::<_, anyhow::Error>(())
    });
    let mut http_server = tokio::spawn(async move {
        let (mut stream, _) = http_listener.accept().await?;
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).await?;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]",
            )
            .await?;
        Ok::<_, anyhow::Error>(())
    });

    let error = LocalClient::new(config)?
        .list_agent_entries()
        .await
        .expect_err("unix transport failure must be returned");
    unix_server.await??;

    assert!(
        !error.to_string().contains("HTTP fallback"),
        "original unix error should not be wrapped as an HTTP fallback failure: {error:#}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut http_server)
            .await
            .is_err(),
        "unix request failure must not connect to the configured HTTP transport"
    );
    http_server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn local_client_unix_event_stream_failure_does_not_retry_over_http() -> Result<()> {
    let socket_dir = tempdir()?;
    let socket_path = socket_dir.path().join("holon.sock");
    let unix_listener = UnixListener::bind(&socket_path)?;
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;

    let config = ConnectionConfig {
        home_dir: socket_dir.path().to_path_buf(),
        default_agent_id: "default".into(),
        socket_path,
        control_token: None,
        http_addr: http_listener.local_addr()?.to_string(),
    };

    let unix_server = tokio::spawn(async move {
        let (stream, _) = unix_listener.accept().await?;
        drop(stream);
        Ok::<_, anyhow::Error>(())
    });
    let mut http_server = tokio::spawn(async move {
        let (mut stream, _) = http_listener.accept().await?;
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).await?;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await?;
        Ok::<_, anyhow::Error>(())
    });

    let error = match LocalClient::new(config)?
        .stream_agent_events("default", EventStreamRequest::default())
        .await
    {
        Ok(_) => anyhow::bail!("unix event stream failure must be returned"),
        Err(error) => error,
    };
    unix_server.await??;

    assert!(
        !error.to_string().contains("HTTP fallback"),
        "original unix event stream error should be preserved: {error:#}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut http_server)
            .await
            .is_err(),
        "unix event stream failure must not connect to the configured HTTP transport"
    );
    http_server.abort();
    Ok(())
}
