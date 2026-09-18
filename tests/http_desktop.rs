mod support;

use anyhow::Result;
use holon::{
    config::ControlAuthMode,
    host::RuntimeHost,
    http::{self, AppState},
    provider::StubProvider,
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};

#[tokio::test]
async fn desktop_disabled_and_missing_peer_fail_closed() -> Result<()> {
    let (host, base, server) = support::spawn_server().await?;
    let client = reqwest::Client::new();
    let capabilities: serde_json::Value = client
        .get(format!("{base}/api/desktop/capabilities"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(capabilities, json!({ "reveal_in_finder": false }));
    let response = client.post(format!("{base}/api/desktop/reveal")).header("Origin", &base)
        .json(&json!({ "workspace_id": "agent_home:default", "execution_root_id": "canonical_root:agent_home:default", "path": "plan.md" })).send().await?;
    assert_eq!(response.status(), 403);
    server.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = http::router(AppState::for_tcp(host).with_desktop_integration(true));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let capabilities: serde_json::Value = client
        .get(format!("http://{addr}/api/desktop/capabilities"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(capabilities, json!({ "reveal_in_finder": false }));
    task.abort();
    Ok(())
}

#[tokio::test]
async fn desktop_auth_origin_and_file_scope_are_enforced() -> Result<()> {
    let config = support::TestConfigBuilder::new()
        .with_control_auth_mode(ControlAuthMode::Required)
        .with_control_token("desktop-test-token")
        .build();
    let host = RuntimeHost::new_with_provider(
        config.config().clone(),
        Arc::new(StubProvider::new("unused")),
    )?;
    support::attach_default_workspace(&host).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base = format!("http://{addr}");
    let app = http::router(AppState::for_tcp(host.clone()).with_desktop_integration(true));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let caps_url = format!("{base}/api/desktop/capabilities");
    assert_eq!(client.get(&caps_url).send().await?.status(), 401);
    let caps: serde_json::Value = client
        .get(&caps_url)
        .bearer_auth("desktop-test-token")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(caps["reveal_in_finder"], cfg!(target_os = "macos"));
    let payload = json!({ "workspace_id": "agent_home:default", "execution_root_id": "canonical_root:agent_home:default", "path": "missing.md" });
    let url = format!("{base}/api/desktop/reveal");
    assert_eq!(
        client
            .post(&url)
            .header("Origin", &base)
            .json(&payload)
            .send()
            .await?
            .status(),
        401
    );
    for origin in [
        None,
        Some("null"),
        Some("https://evil.test"),
        Some("http://localhost:9999"),
    ] {
        let mut request = client
            .post(&url)
            .bearer_auth("desktop-test-token")
            .json(&payload);
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        assert_eq!(request.send().await?.status(), 403);
    }
    let response = client
        .post(&url)
        .bearer_auth("desktop-test-token")
        .header("Origin", &base)
        .header("Sec-Fetch-Site", "cross-site")
        .json(&payload)
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    if cfg!(target_os = "macos") {
        for (root, path, expected) in [
            ("canonical_root:agent_home:default", "missing.md", 404),
            ("unknown-root", "missing.md", 404),
            ("canonical_root:agent_home:default", "../../outside.md", 403),
        ] {
            let response = client.post(&url).bearer_auth("desktop-test-token").header("Origin", &base)
                .json(&json!({ "workspace_id": "agent_home:default", "execution_root_id": root, "path": path })).send().await?;
            assert_eq!(response.status().as_u16(), expected, "{root} {path}");
        }
        #[cfg(unix)]
        {
            let home = host
                .workspace_entries()?
                .into_iter()
                .find(|w| w.workspace_id == "agent_home:default")
                .unwrap()
                .workspace_anchor;
            let outside = support::tempdir()?;
            std::fs::write(outside.path().join("outside.md"), "outside")?;
            std::os::unix::fs::symlink(outside.path().join("outside.md"), home.join("escape.md"))?;
            let response = client.post(&url).bearer_auth("desktop-test-token").header("Origin", &base)
                .json(&json!({ "workspace_id": "agent_home:default", "execution_root_id": "canonical_root:agent_home:default", "path": "escape.md" })).send().await?;
            assert_eq!(response.status(), 403);
        }
    }
    task.abort();
    Ok(())
}

/// Manual desktop smoke test; never opens Finder during normal tests or CI.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "opens Finder; run explicitly on a local Mac"]
async fn desktop_reveal_opens_a_registered_test_file() -> Result<()> {
    let config = support::TestConfigBuilder::new().build();
    let host = RuntimeHost::new_with_provider(
        config.config().clone(),
        Arc::new(StubProvider::new("unused")),
    )?;
    support::attach_default_workspace(&host).await?;
    let home = host
        .workspace_entries()?
        .into_iter()
        .find(|w| w.workspace_id == "agent_home:default")
        .unwrap()
        .workspace_anchor;
    std::fs::write(
        home.join("Finder smoke test 中文.md"),
        "# Desktop integration smoke test\n",
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = http::router(AppState::for_tcp(host).with_desktop_integration(true));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let response = reqwest::Client::new().post(format!("{base}/api/desktop/reveal")).header("Origin", &base)
        .json(&json!({ "workspace_id": "agent_home:default", "execution_root_id": "canonical_root:agent_home:default", "path": "Finder smoke test 中文.md" })).send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "ok": true })
    );
    server.abort();
    Ok(())
}
