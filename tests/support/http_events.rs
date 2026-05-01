// HTTP events route integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use holon::{
    client::{EventStreamRequest, LocalClient},
    config::{AppConfig, ControlAuthMode},
    daemon::RuntimeServiceHandle,
    host::RuntimeHost,
    http::{self, AppState},
    provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction,
        ExternalTriggerStatus, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        OperatorDeliveryStatus, TrustLevel, WaitingIntentStatus, WorkItemState, WorkPlanItem,
        WorkPlanStepStatus,
    },
};
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::{sleep, Duration, Instant};
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use super::{
    attach_default_workspace, connect_addr, git, init_git_repo, read_next_sse_event, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config, tempdir,
    test_config, test_config_with_paths, wait_until, ParsedSseEvent,
};

pub async fn events_route_supports_cursor_replay() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "first message" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let bootstrap: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    let cursor = bootstrap["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "second message" }))
        .send()
        .await?;
    let mut stream = client
        .get(format!("{base}/agents/default/events?since={cursor}"))
        .send()
        .await?;
    let first_event =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data["type"], "message_admitted");

    server.abort();
    Ok(())
}

pub async fn events_route_supports_last_event_id_header_and_rfc3339_ts() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "header bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let bootstrap: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    let cursor = bootstrap["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "header replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!("{base}/agents/default/events"))
        .header("Last-Event-ID", cursor)
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;
    assert_eq!(replayed.event, "message_admitted");
    assert!(replayed.data["ts"].is_string());
    assert!(chrono::DateTime::parse_from_rfc3339(
        replayed.data["ts"].as_str().expect("ts should be a string")
    )
    .is_ok());

    server.abort();
    Ok(())
}

pub async fn events_route_with_missing_cursor_returns_refresh_hint() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/agents/default/events?since=evt_missing"))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    assert_eq!(status, reqwest::StatusCode::GONE);
    assert_eq!(body["code"], "cursor_too_old");
    server.abort();
    Ok(())
}
