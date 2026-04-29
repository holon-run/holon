// HTTP client integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
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
        OperatorDeliveryStatus, TrustLevel, WaitingIntentStatus, WorkItemStatus, WorkPlanItem,
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
    attach_default_workspace, connect_addr, git, init_git_repo, parse_sse_frame,
    read_next_sse_event, spawn_server, spawn_server_for_host, spawn_server_with_config,
    spawn_server_with_runtime_config, spawn_unix_server, tempdir, test_config,
    test_config_with_paths, wait_until, ParsedSseEvent,
};

pub async fn local_client_over_unix_socket_can_poll_without_http_fallback() -> Result<()> {
    let config = test_config();
    let (_host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let client = LocalClient::new(config)?;

    let agents = client.list_agents().await?;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].identity.agent_id, "default");

    let status = client.agent_status("default").await?;
    assert_eq!(status.identity.agent_id, "default");

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn local_client_over_http_can_read_agent_state_snapshot() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let client = LocalClient::new(config)?;

    client.control_prompt("default", "state bootstrap").await?;
    runtime
        .notify_operator("HTTP state visible operator note".into())
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let snapshot = client.agent_state_snapshot("default").await?;
    assert_eq!(snapshot.agent.identity.agent_id, "default");
    assert!(snapshot.session.pending_count <= snapshot.agent.agent.pending);
    assert!(snapshot
        .operator_notifications
        .iter()
        .any(|notification| notification.summary == "HTTP state visible operator note"));
    assert!(snapshot.cursor.is_some());

    server.abort();
    Ok(())
}

pub async fn local_client_over_http_can_stream_events_with_since_query() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "http stream bootstrap")
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;
    let cursor = client
        .agent_state_snapshot("default")
        .await?
        .cursor
        .expect("cursor should be present");

    client
        .control_prompt("default", "http stream replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                since: Some(cursor),
                ..Default::default()
            },
        )
        .await?;
    let first_event = tokio::time::timeout(Duration::from_secs(5), stream.next_event()).await??;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}

pub async fn local_client_over_http_can_stream_events_with_last_event_id_header() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "http header bootstrap")
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;
    let cursor = client
        .agent_state_snapshot("default")
        .await?
        .cursor
        .expect("cursor should be present");

    client
        .control_prompt("default", "http header replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                last_event_id: Some(cursor),
                ..Default::default()
            },
        )
        .await?;
    let first_event = tokio::time::timeout(Duration::from_secs(5), stream.next_event()).await??;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn local_client_over_unix_socket_can_read_agent_state_snapshot() -> Result<()> {
    let config = test_config();
    let (_host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let client = LocalClient::new(config)?;

    let snapshot = client.agent_state_snapshot("default").await?;
    assert_eq!(snapshot.agent.identity.agent_id, "default");
    assert!(snapshot.agent.agent.active_workspace_entry.is_some());

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn local_client_over_unix_socket_can_stream_events_with_since_query() -> Result<()> {
    let config = test_config();
    let (host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "unix stream bootstrap")
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;
    let cursor = client
        .agent_state_snapshot("default")
        .await?
        .cursor
        .expect("cursor should be present");

    client
        .control_prompt("default", "unix stream replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                since: Some(cursor),
                ..Default::default()
            },
        )
        .await?;
    let first_event = tokio::time::timeout(Duration::from_secs(5), stream.next_event()).await??;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn local_client_over_unix_socket_can_stream_events_with_last_event_id_header(
) -> Result<()> {
    let config = test_config();
    let (host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "unix header bootstrap")
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;
    let cursor = client
        .agent_state_snapshot("default")
        .await?
        .cursor
        .expect("cursor should be present");

    client
        .control_prompt("default", "unix header replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                last_event_id: Some(cursor),
                ..Default::default()
            },
        )
        .await?;
    let first_event = tokio::time::timeout(Duration::from_secs(5), stream.next_event()).await??;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}
