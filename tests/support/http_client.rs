// HTTP client integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    client::{
        AgentStreamEvent, EventPageRequest, EventStreamRequest, LocalClient, LocalEventStream,
    },
    config::{AppConfig, ControlAuthMode},
    daemon::RuntimeServiceHandle,
    host::RuntimeHost,
    http::{self, AppState},
    provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider},
    runtime::RuntimeHandle,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction,
        ExternalTriggerScope, ExternalTriggerStatus, MessageBody, MessageDeliverySurface,
        MessageKind, MessageOrigin, OperatorDeliveryStatus, TodoItem, TodoItemState, WorkItemState,
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
    attach_default_workspace, callback_path, connect_addr, git, init_git_repo, parse_sse_frame,
    read_next_sse_event, spawn_server, spawn_server_for_host, spawn_server_with_config,
    spawn_server_with_runtime_config, spawn_unix_server, tempdir, test_config,
    test_config_with_paths, wait_until, ParsedSseEvent,
};

async fn next_message_admitted_event(stream: &mut LocalEventStream) -> Result<AgentStreamEvent> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = stream.next_event().await?;
            if event.event == "message_admitted" {
                return Ok(event);
            }
        }
    })
    .await?
}

async fn wait_for_event_type(runtime: &RuntimeHandle, event_type: &str) -> Result<()> {
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_recent_events(50)?
            .iter()
            .any(|event| event.kind == event_type))
    })
    .await
}

pub async fn local_client_over_unix_socket_can_poll_without_http_fallback() -> Result<()> {
    let config = test_config();
    let (_host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let client = LocalClient::new(config)?;

    let agents = client.list_agent_entries().await?;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].identity.agent_id, "default");

    let status = client.agent_status("default").await?;
    assert_eq!(status.identity.agent_id, "default");

    server.abort();
    Ok(())
}

pub async fn http_success_response_shapes_follow_route_class_policy() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let root: serde_json::Value = client
        .get(format!("{base}/api"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(root["ok"], true, "discovery root should use ok envelope");
    assert_eq!(root["default_agent"], "default");

    let models: serde_json::Value = client
        .get(format!("{base}/api/models"))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        models.get("ok").is_none(),
        "/models should remain a direct catalog record"
    );
    assert!(models["available_models"].is_array());

    let agents: serde_json::Value = client
        .get(format!("{base}/api/agents/list"))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        agents.as_array().is_some(),
        "read-model list routes should return direct arrays"
    );

    let prompt: serde_json::Value = client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "success shape policy" }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        prompt["ok"], true,
        "control mutations should use ok envelope"
    );
    assert_eq!(prompt["agent_id"], "default");
    assert!(prompt["message_id"].is_string());

    let capability = runtime
        .create_external_trigger(
            "success shape callback".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await?;
    let callback: serde_json::Value = client
        .post(format!(
            "{}{}",
            base,
            callback_path(&capability.trigger_url)
        ))
        .json(&serde_json::json!({ "event": "success_shape" }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        callback["ok"], true,
        "capability callbacks should use ok envelope"
    );
    assert_eq!(callback["delivery_mode"], "wake_hint");

    let stream_response = client
        .get(format!("{base}/api/agents/default/events/stream"))
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await?;
    assert_eq!(stream_response.status(), reqwest::StatusCode::OK);
    assert!(
        stream_response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "stream route success should be SSE, not a JSON envelope"
    );

    server.abort();
    Ok(())
}

pub async fn agent_list_entries_are_slim_for_tui_bootstrap() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let original_cwd = tempdir()?.keep();
    let worktree_path = tempdir()?.keep();
    runtime
        .enter_worktree(
            original_cwd,
            "main".into(),
            worktree_path,
            "slim-list-test".into(),
        )
        .await?;
    let state = runtime.agent_state().await?;
    assert!(
        state
            .active_workspace_entry
            .as_ref()
            .and_then(|entry| entry.projection_metadata.as_ref())
            .is_some(),
        "test setup should seed nested projection metadata"
    );

    let client = reqwest::Client::new();
    let payload: serde_json::Value = client
        .get(format!("{base}/api/agents/list"))
        .send()
        .await?
        .json()
        .await?;
    let entry = payload
        .as_array()
        .and_then(|entries| entries.first())
        .expect("agent list should contain default agent");
    assert_eq!(entry["identity"]["agent_id"], "default");
    assert!(entry.get("status").is_some());
    assert_eq!(entry["scheduling_posture"]["posture"], "idle");
    assert!(entry["scheduling_posture"]["reason"].is_string());
    assert!(entry.get("model").is_some());
    assert!(
        entry.get("model_availability").is_none(),
        "/agents/list must not embed runtime-global model availability"
    );
    let workspace_entry = entry
        .get("active_workspace_entry")
        .expect("active workspace entry should be present");
    assert!(
        workspace_entry.get("projection_metadata").is_none(),
        "projection metadata should not be present in /agents/list"
    );

    for heavy_field in [
        "skills",
        "active_children",
        "active_waiting_intents",
        "active_external_triggers",
        "recent_operator_notifications",
        "loaded_agents_md",
        "recent_brief_count",
        "recent_event_count",
    ] {
        assert!(
            entry.get(heavy_field).is_none(),
            "{heavy_field} should not be present in /agents/list"
        );
    }

    let removed_agents = client.get(format!("{base}/api/agents")).send().await?;
    assert_eq!(
        removed_agents.status(),
        reqwest::StatusCode::NOT_FOUND,
        "/agents aggregate summary endpoint should not be exposed"
    );

    let models_payload: serde_json::Value = client
        .get(format!("{base}/api/models"))
        .send()
        .await?
        .json()
        .await?;
    let model_availability = models_payload["model_availability"]
        .as_array()
        .expect("/models model_availability should be an array");
    assert!(
        model_availability.iter().all(|entry| entry.is_object()),
        "/models remains the source for runtime-global model availability"
    );

    let mut config = test_config();
    config.http_addr = base.trim_start_matches("http://").to_string();
    let local_client = LocalClient::new(config)?;
    let entries = local_client.list_agent_entries().await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].identity.agent_id, "default");
    assert!(entries[0]
        .active_workspace_entry
        .as_ref()
        .is_some_and(|entry| entry.projection_metadata.is_none()));

    server.abort();
    Ok(())
}

pub async fn json_responses_support_gzip_without_compressing_sse() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::builder().no_gzip().build()?;

    let response = client
        .get(format!("{base}/api/agents/list"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await?;
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    let compressed = response.bytes().await?;
    let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
    let mut decoded = String::new();
    decoder.read_to_string(&mut decoded)?;
    let payload: serde_json::Value = serde_json::from_str(&decoded)?;
    assert!(payload.as_array().is_some());

    let stream_response = client
        .get(format!("{base}/api/agents/default/events/stream"))
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await?;
    assert_eq!(stream_response.status(), reqwest::StatusCode::OK);
    assert!(
        stream_response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_none(),
        "SSE responses must remain uncompressed"
    );

    server.abort();
    Ok(())
}

pub async fn agent_list_entries_tolerate_unloaded_agent_with_corrupt_work_queue() -> Result<()> {
    let data_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let mut config = test_config_with_paths(
        data_dir.clone(),
        workspace_dir,
        "127.0.0.1:0".to_string(),
        ControlAuthMode::Auto,
    );
    let (host, _base, server) = spawn_server_with_config(config.clone()).await?;
    host.create_named_agent("corrupt-list", None).await?;
    server.abort();

    let work_items_path = data_dir
        .join("agents")
        .join("corrupt-list")
        .join(".holon")
        .join("ledger")
        .join("work_items.jsonl");
    if let Some(parent) = work_items_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&work_items_path)?;
    writeln!(file, "{{not valid json")?;
    let agent_state_path = data_dir
        .join("agents")
        .join("corrupt-list")
        .join(".holon")
        .join("state")
        .join("agent.json");
    if agent_state_path.exists() {
        std::fs::remove_file(&agent_state_path)?;
    }

    config.workspace_dir = tempdir()?.keep();
    let (_host, base, server) = spawn_server_with_config(config.clone()).await?;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/agents/list"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await?;
    let entries = payload
        .as_array()
        .expect("agent list response should be an array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["identity"]["agent_id"] == "corrupt-list"),
        "agent list should include the unloaded public agent"
    );
    let corrupt_entry = entries
        .iter()
        .find(|entry| entry["identity"]["agent_id"] == "corrupt-list")
        .expect("corrupt-list entry should be present");
    assert_eq!(
        corrupt_entry["status"], "asleep",
        "unloaded agent should use canonical RuntimeDb state and ignore corrupt legacy work queue"
    );

    let mut client_config = config;
    client_config.http_addr = base.trim_start_matches("http://").to_string();
    let entries = LocalClient::new(client_config)?
        .list_agent_entries()
        .await?;
    assert!(entries
        .iter()
        .any(|entry| entry.identity.agent_id == "corrupt-list"));

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
    wait_for_event_type(&runtime, "operator_notification_requested").await?;

    let snapshot = client.agent_state_snapshot("default").await?;
    assert_eq!(snapshot.agent.identity.agent_id, "default");
    assert!(snapshot.session.pending_count <= snapshot.agent.agent.pending);
    assert!(snapshot.operator_notifications.is_empty());
    let events_page = client
        .agent_events_page(
            "default",
            EventPageRequest {
                limit: Some(20),
                order: Some("desc".into()),
                ..Default::default()
            },
        )
        .await?;
    assert!(events_page.newest_seq.is_some());
    assert!(events_page
        .events
        .iter()
        .any(|event| event.event_type == "operator_notification_requested"));

    let raw_state: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    let raw_agent = raw_state["agent"]
        .as_object()
        .expect("state snapshot should include an agent object");
    assert!(
        !raw_agent.contains_key("model_availability"),
        "/agents/{{agent_id}}/state must not embed runtime-global model availability"
    );
    assert!(
        !raw_state
            .as_object()
            .expect("state snapshot should be an object")
            .contains_key("operator_notifications"),
        "/agents/{{agent_id}}/state must not embed operator notifications"
    );
    assert!(
        !raw_state
            .as_object()
            .expect("state snapshot should be an object")
            .contains_key("execution"),
        "/agents/{{agent_id}}/state must not embed duplicate execution snapshot"
    );
    assert!(
        !raw_state
            .as_object()
            .expect("state snapshot should be an object")
            .contains_key("cursor"),
        "/agents/{{agent_id}}/state must not expose chat event cursors"
    );

    server.abort();
    Ok(())
}

pub async fn local_client_over_http_can_stream_events_with_cursor_query() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "http stream bootstrap")
        .await?;
    wait_for_event_type(&runtime, "message_admitted").await?;
    let after_seq = client
        .agent_events_page(
            "default",
            EventPageRequest {
                limit: Some(1),
                order: Some("desc".into()),
                ..Default::default()
            },
        )
        .await?
        .newest_seq
        .expect("event seq should be present");

    client
        .control_prompt("default", "http stream replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                after_seq: Some(after_seq),
                ..Default::default()
            },
        )
        .await?;
    let first_event = next_message_admitted_event(&mut stream).await?;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");
    assert_eq!(
        first_event
            .data
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.get("authority_class"))
            .and_then(|authority_class| authority_class.as_str()),
        Some("operator_instruction")
    );

    server.abort();
    Ok(())
}

pub async fn local_client_over_http_stream_without_cursor_starts_at_tail() -> Result<()> {
    let mut config = test_config();
    let (host, base, server) = spawn_server_with_config(config.clone()).await?;
    config.http_addr = base.trim_start_matches("http://").to_string();
    let runtime = host.default_runtime().await?;
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "http tail bootstrap")
        .await?;
    wait_for_event_type(&runtime, "message_admitted").await?;

    let mut stream = client
        .stream_agent_events("default", EventStreamRequest::default())
        .await?;
    client.control_prompt("default", "http tail live").await?;
    let first_event = next_message_admitted_event(&mut stream).await?;
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
pub async fn local_client_over_unix_socket_can_stream_events_with_cursor_query() -> Result<()> {
    let config = test_config();
    let (host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "unix stream bootstrap")
        .await?;
    wait_for_event_type(&runtime, "message_admitted").await?;
    let after_seq = client
        .agent_events_page(
            "default",
            EventPageRequest {
                limit: Some(1),
                order: Some("desc".into()),
                ..Default::default()
            },
        )
        .await?
        .newest_seq
        .expect("event seq should be present");

    client
        .control_prompt("default", "unix stream replay")
        .await?;
    let mut stream = client
        .stream_agent_events(
            "default",
            EventStreamRequest {
                after_seq: Some(after_seq),
                ..Default::default()
            },
        )
        .await?;
    let first_event = next_message_admitted_event(&mut stream).await?;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}

#[cfg(unix)]
#[cfg(unix)]
pub async fn local_client_over_unix_socket_stream_without_cursor_starts_at_tail() -> Result<()> {
    let config = test_config();
    let (host, _socket_path, server) = spawn_unix_server(config.clone()).await?;
    let runtime = host.default_runtime().await?;
    let client = LocalClient::new(config)?;

    client
        .control_prompt("default", "unix tail bootstrap")
        .await?;
    wait_for_event_type(&runtime, "message_admitted").await?;

    let mut stream = client
        .stream_agent_events("default", EventStreamRequest::default())
        .await?;
    client.control_prompt("default", "unix tail live").await?;
    let first_event = next_message_admitted_event(&mut stream).await?;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data.event_type, "message_admitted");

    server.abort();
    Ok(())
}
