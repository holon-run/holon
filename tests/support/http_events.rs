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
        AdmissionContext, AgentStatus, AuditEvent, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, CommandTaskSpec, ContinuationClass, ControlAction,
        ExternalTriggerStatus, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, OperatorDeliveryStatus, Priority, TaskKind, TaskRecord, TaskStatus,
        TodoItem, TodoItemState, WaitingIntentStatus, WorkItemState,
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

async fn next_sse_event_kind(
    stream: &mut reqwest::Response,
    event_kind: &str,
) -> Result<ParsedSseEvent> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = read_next_sse_event(stream).await?;
            if event.event == event_kind {
                return Ok(event);
            }
        }
    })
    .await?
}

async fn newest_event_seq(base: &str, client: &Client, token: Option<&str>) -> Result<u64> {
    let mut request = client.get(format!("{base}/agents/default/events?limit=1&order=desc"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let page: serde_json::Value = request.send().await?.json().await?;
    page["newest_seq"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("newest_seq should be present"))
}

pub async fn events_route_supports_cursor_pagination() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.storage().append_event(&AuditEvent::new(
        "test_event",
        serde_json::json!({ "n": 1 }),
    ))?;
    runtime.storage().append_event(&AuditEvent::new(
        "test_event",
        serde_json::json!({ "n": 2 }),
    ))?;
    runtime.storage().append_event(&AuditEvent::new(
        "test_event",
        serde_json::json!({ "n": 3 }),
    ))?;
    let latest: serde_json::Value = client
        .get(format!("{base}/agents/default/events?limit=2&order=desc"))
        .send()
        .await?
        .json()
        .await?;
    let latest_events = latest["events"].as_array().expect("events");
    assert_eq!(latest_events.len(), 2);
    let latest_newest = latest_events[0]["event_seq"].as_u64().expect("newest seq");
    let latest_oldest = latest_events[1]["event_seq"].as_u64().expect("oldest seq");
    assert_eq!(latest["newest_seq"], latest_newest);
    assert_eq!(latest["oldest_seq"], latest_oldest);
    assert_eq!(latest["has_older"], true);
    assert_eq!(latest["has_newer"], false);

    let older: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?before_seq={}&limit=2&order=desc",
            latest["oldest_seq"].as_u64().expect("oldest seq")
        ))
        .send()
        .await?
        .json()
        .await?;
    let older_events = older["events"].as_array().expect("events");
    assert!(!older_events.is_empty());
    let older_newest = older_events[0]["event_seq"]
        .as_u64()
        .expect("older newest seq");
    assert_ne!(older_newest, latest_newest);
    assert_ne!(older_newest, latest_oldest);
    assert_eq!(older["newest_seq"], older_newest);
    assert_eq!(older["has_newer"], false);

    let newer: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?after_seq={}&limit=2&order=asc",
            older["newest_seq"].as_u64().expect("newest seq")
        ))
        .send()
        .await?
        .json()
        .await?;
    let newer_events = newer["events"].as_array().expect("events");
    assert_eq!(newer_events.len(), 2);
    assert_eq!(newer_events[0]["event_seq"], latest_oldest);
    assert_eq!(newer_events[1]["event_seq"], latest_newest);
    assert_eq!(newer["oldest_seq"], latest_oldest);
    assert_eq!(newer["newest_seq"], latest_newest);
    assert_eq!(newer["has_older"], false);
    assert_eq!(newer["has_newer"], false);

    let bounded_newer: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?after_seq={latest_oldest}&limit=10&order=desc"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(bounded_newer["events"].as_array().expect("events").len(), 1);
    assert_eq!(bounded_newer["has_older"], false);
    assert_eq!(bounded_newer["has_newer"], false);

    let bounded_older: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?after_seq={older_newest}&before_seq={latest_newest}&limit=10&order=asc"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(bounded_older["events"].as_array().expect("events").len(), 1);
    assert_eq!(bounded_older["has_older"], false);
    assert_eq!(bounded_older["has_newer"], false);

    let empty_cursor: serde_json::Value = client
        .get(format!("{base}/agents/default/events?limit=2&order=desc"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(empty_cursor["events"].as_array().expect("events").len(), 2);
    assert_eq!(empty_cursor["newest_seq"], latest_newest);
    assert_eq!(empty_cursor["oldest_seq"], latest_oldest);

    server.abort();
    Ok(())
}

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

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "second message" }))
        .send()
        .await?;
    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let first_event = next_sse_event_kind(&mut stream, "message_admitted").await?;
    assert_eq!(first_event.event, "message_admitted");
    assert_eq!(first_event.data["type"], "message_admitted");

    server.abort();
    Ok(())
}

pub async fn events_stream_supports_cursor_and_rfc3339_ts() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "cursor bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "cursor replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "message_admitted").await?;
    assert_eq!(replayed.event, "message_admitted");
    assert_eq!(
        replayed._id,
        replayed.data["event_seq"]
            .as_u64()
            .expect("event_seq should be present")
            .to_string()
    );
    assert!(replayed.data["ts"].is_string());
    assert!(chrono::DateTime::parse_from_rfc3339(
        replayed.data["ts"].as_str().expect("ts should be a string")
    )
    .is_ok());

    server.abort();
    Ok(())
}

pub async fn events_route_preserves_replay_provenance() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "provenance bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "provenance replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "message_admitted").await?;
    assert_eq!(replayed.event, "message_admitted");
    assert_eq!(replayed.data["type"], "message_admitted");
    assert_eq!(replayed.data["provenance"]["origin"]["kind"], "operator");
    assert_eq!(
        replayed.data["provenance"]["authority_class"],
        "operator_instruction"
    );
    assert_eq!(
        replayed.data["provenance"]["delivery_surface"],
        "http_control_prompt"
    );
    assert!(replayed.data.get("projection").is_none());

    server.abort();
    Ok(())
}

pub async fn events_route_payload_includes_full_fields() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "stable operator event bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    runtime.storage().append_event(&AuditEvent::new(
        "message_admitted",
        serde_json::json!({
            "agent_id": "default",
            "message_id": "msg-stable",
            "origin": { "kind": "operator" },
            "authority_class": "operator_instruction",
            "delivery_surface": "http_control_prompt",
            "priority": "interject",
            "summary": "operator prompt admitted",
            "text_preview": "inspect status",
            "raw_text": "debug-only prompt body",
        }),
    ))?;
    runtime.storage().append_event(&AuditEvent::new(
        "brief_created",
        serde_json::json!({
            "agent_id": "default",
            "run_id": "run-stable",
            "work_item_id": "work-stable",
            "status": "completed",
            "summary": "work finished",
            "text_preview": "final summary",
            "raw_brief": { "debug": true },
        }),
    ))?;
    runtime.storage().append_event(&AuditEvent::new(
        "task_status_updated",
        serde_json::json!({
            "agent_id": "default",
            "task_id": "task-stable",
            "status": "completed",
            "duration_ms": 123,
            "exit_status": 0,
            "summary": "cargo check passed",
            "stdout": "debug-only output",
        }),
    ))?;

    let page: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?after_seq={after_seq}&limit=10&order=asc"
        ))
        .send()
        .await?
        .json()
        .await?;
    let events = page["events"].as_array().expect("events");

    let admitted = events
        .iter()
        .find(|event| event["type"] == "message_admitted")
        .expect("message_admitted event");
    assert_eq!(admitted["payload"]["agent_id"], "default");
    assert_eq!(admitted["payload"]["message_id"], "msg-stable");
    assert_eq!(admitted["payload"]["summary"], "operator prompt admitted");
    assert_eq!(admitted["payload"]["text_preview"], "inspect status");
    assert_eq!(admitted["payload"]["raw_text"], "debug-only prompt body");
    assert!(admitted.get("projection").is_none());

    let brief = events
        .iter()
        .find(|event| event["type"] == "brief_created")
        .expect("brief_created event");
    assert_eq!(brief["payload"]["run_id"], "run-stable");
    assert_eq!(brief["payload"]["work_item_id"], "work-stable");
    assert_eq!(brief["payload"]["summary"], "work finished");
    assert_eq!(brief["payload"]["raw_brief"]["debug"], true);
    assert!(brief.get("projection").is_none());

    let task = events
        .iter()
        .find(|event| event["type"] == "task_status_updated")
        .expect("task_status_updated event");
    assert_eq!(task["payload"]["task_id"], "task-stable");
    assert_eq!(task["payload"]["status"], "completed");
    assert_eq!(task["payload"]["duration_ms"], 123);
    assert_eq!(task["payload"]["stdout"], "debug-only output");
    assert!(task.get("projection").is_none());

    server.abort();
    Ok(())
}

pub async fn events_route_max_level_filters_with_bounded_visible_pages() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let operator_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "visible operator input".into(),
        },
    );
    runtime.storage().append_event(&AuditEvent::new(
        "message_enqueued",
        serde_json::to_value(operator_message)?,
    ))?;
    for index in 0..150 {
        runtime.storage().append_event(&AuditEvent::new(
            "callback_delivered",
            serde_json::json!({
                "waiting_intent_id": format!("wait-{index}"),
                "source": "github",
            }),
        ))?;
    }
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "visible final brief",
        None,
        None,
    );
    runtime.storage().append_event(&AuditEvent::new(
        "brief_created",
        serde_json::to_value(brief)?,
    ))?;

    let page: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/events?limit=2&order=desc&max_level=info"
        ))
        .send()
        .await?
        .json()
        .await?;
    let events = page["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "brief_created");
    assert_eq!(events[1]["type"], "message_enqueued");
    assert_eq!(page["has_older"], false);
    assert!(page["cursor_seq"].as_u64().is_some());

    let unfiltered: serde_json::Value = client
        .get(format!("{base}/agents/default/events?limit=32&order=desc"))
        .send()
        .await?
        .json()
        .await?;
    assert!(unfiltered["events"]
        .as_array()
        .expect("events")
        .iter()
        .any(|event| event["type"] == "callback_delivered"));

    server.abort();
    Ok(())
}

pub async fn events_stream_includes_tool_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "event stream bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    runtime.storage().append_event(&AuditEvent::new(
        "tool_executed",
        serde_json::json!({
            "agent_id": "default",
            "tool_name": "ExecCommand",
            "task_id": "task-tool",
            "exec_command_cmd": "cargo test --lib",
            "raw_output": "test result: ok",
            "local_path": "/tmp/output.txt",
            "artifact_ref": "artifact://test-output",
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "tool_executed").await?;
    assert_eq!(replayed.event, "tool_executed");
    assert_eq!(replayed.data["type"], "tool_executed");
    assert!(replayed.data.get("projection").is_none());
    assert_eq!(replayed.data["payload"]["tool_name"], "ExecCommand");
    assert_eq!(replayed.data["payload"]["task_id"], "task-tool");
    assert_eq!(
        replayed.data["payload"]["exec_command_cmd"],
        "cargo test --lib"
    );
    assert_eq!(replayed.data["payload"]["raw_output"], "test result: ok");
    assert_eq!(replayed.data["payload"]["local_path"], "/tmp/output.txt");
    assert_eq!(
        replayed.data["payload"]["artifact_ref"],
        "artifact://test-output"
    );

    server.abort();
    Ok(())
}

pub async fn events_stream_includes_assistant_round_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "assistant round stream bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    runtime.storage().append_event(&AuditEvent::new(
        "assistant_round_recorded",
        serde_json::json!({
            "agent_id": "default",
            "run_id": "run-1",
            "turn_index": 7,
            "round": 2,
            "stop_reason": "tool_use",
            "text_preview": "I will inspect the files.",
            "text_block_count": 1,
            "text_char_count": 25,
            "tool_call_count": 2,
            "tool_names": ["ExecCommand", "ReadFile"],
            "has_text": true,
            "has_tool_calls": true,
            "raw_text": "full assistant text included in operator replay",
            "provider_trace": { "detail": "debug-info" },
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "assistant_round_recorded").await?;

    assert_eq!(replayed.event, "assistant_round_recorded");
    assert_eq!(replayed.data["type"], "assistant_round_recorded");
    assert!(replayed.data.get("projection").is_none());
    assert_eq!(replayed.data["payload"]["agent_id"], "default");
    assert_eq!(replayed.data["payload"]["run_id"], "run-1");
    assert_eq!(replayed.data["payload"]["turn_index"], 7);
    assert_eq!(replayed.data["payload"]["round"], 2);
    assert_eq!(replayed.data["payload"]["stop_reason"], "tool_use");
    assert_eq!(
        replayed.data["payload"]["text_preview"],
        "I will inspect the files."
    );
    assert_eq!(replayed.data["payload"]["text_block_count"], 1);
    assert_eq!(replayed.data["payload"]["text_char_count"], 25);
    assert_eq!(replayed.data["payload"]["tool_call_count"], 2);
    assert_eq!(
        replayed.data["payload"]["tool_names"],
        serde_json::json!(["ExecCommand", "ReadFile"])
    );
    assert_eq!(replayed.data["payload"]["has_text"], true);
    assert_eq!(replayed.data["payload"]["has_tool_calls"], true);
    assert_eq!(
        replayed.data["payload"]["raw_text"],
        "full assistant text included in operator replay"
    );
    assert!(replayed.data["payload"].get("provider_trace").is_some());

    server.abort();
    Ok(())
}

pub async fn events_stream_preserves_raw_payload_with_control_auth() -> Result<()> {
    let config = test_config_with_paths(
        tempdir()?.keep(),
        tempdir()?.keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    let token = config
        .control_token
        .clone()
        .expect("required control auth should generate token");
    let (host, base, server) = spawn_server_with_config(config).await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "text": "workspace stream bootstrap" }))
        .send()
        .await?
        .error_for_status()?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, Some(&token)).await?;

    runtime.storage().append_event(&AuditEvent::new(
        "workspace_used",
        serde_json::json!({
            "agent_id": "default",
            "workspace_id": "ws-1",
            "workspace_label": "holon",
            "workspace_anchor": "/repo/holon",
            "execution_root": "/repo/holon/worktree",
            "projection_kind": "worktree",
            "access_mode": "shared_read",
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .bearer_auth(token)
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "workspace_used").await?;

    assert_eq!(replayed.event, "workspace_used");
    assert_eq!(replayed.data["type"], "workspace_used");
    assert!(replayed.data.get("projection").is_none());
    assert_eq!(replayed.data["payload"]["workspace_id"], "ws-1");
    assert_eq!(replayed.data["payload"]["workspace_label"], "holon");
    assert_eq!(
        replayed.data["payload"]["execution_root"],
        "/repo/holon/worktree"
    );

    server.abort();
    Ok(())
}

pub async fn events_page_cursor_seq_seeds_stream_resume() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    runtime.storage().append_event(&AuditEvent::new(
        "assistant_round_recorded",
        serde_json::json!({
            "agent_id": "default",
            "run_id": "run-state",
            "turn_index": 3,
            "round": 1,
            "stop_reason": "tool_use",
            "text_preview": null,
            "tool_names": ["ExecCommand"],
            "tool_call_count": 1,
            "has_text": false,
            "has_tool_calls": true,
            "raw_text": "debug-only assistant body",
        }),
    ))?;

    let page: serde_json::Value = client
        .get(format!("{base}/agents/default/events?limit=10&order=desc"))
        .send()
        .await?
        .json()
        .await?;
    let events_tail = page["events"]
        .as_array()
        .expect("events page should include an events array");
    let assistant_tail = events_tail
        .iter()
        .find(|event| event["type"] == "assistant_round_recorded")
        .expect("events page should include the appended assistant round");
    let after_seq = page["cursor_seq"]
        .as_u64()
        .expect("events page should include cursor_seq");
    assert!(assistant_tail.get("projection").is_none());
    assert_eq!(assistant_tail["payload"]["stop_reason"], "tool_use");
    assert_eq!(
        assistant_tail["payload"]["tool_names"],
        serde_json::json!(["ExecCommand"])
    );

    runtime.storage().append_event(&AuditEvent::new(
        "tool_executed",
        serde_json::json!({
            "agent_id": "default",
            "tool_name": "ReadFile",
            "task_id": "task-tail",
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events/stream?after_seq={after_seq}"
        ))
        .send()
        .await?;
    let replayed = next_sse_event_kind(&mut stream, "tool_executed").await?;
    assert_eq!(replayed.event, "tool_executed");
    assert_ne!(replayed._id, after_seq.to_string());

    server.abort();
    Ok(())
}

pub async fn events_stream_requires_control_auth_when_configured() -> Result<()> {
    let config = test_config_with_paths(
        tempdir()?.keep(),
        tempdir()?.keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    let (_host, base, server) = spawn_server_with_config(config).await?;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/agents/default/events/stream"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn state_snapshot_bounds_large_projection_fields() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let now = chrono::Utc::now();

    let task = TaskRecord {
        id: "large-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("large task".into()),
        detail: Some(serde_json::json!({
            "output_path": "/tmp/large-task.log",
            "output_summary": "x".repeat(12_000),
            "lines": (0..120).collect::<Vec<_>>()
        })),
        recovery: None,
    };
    runtime.storage().append_task(&task)?;
    host.runtime_db().tasks().upsert(&task)?;
    let snapshot: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;

    assert!(snapshot.get("transcript_tail").is_none());
    assert!(snapshot.get("operator_messages").is_none());
    assert!(snapshot.get("events_tail").is_none());
    assert!(snapshot.get("brief_tail").is_none());
    assert!(snapshot.get("transcript").is_none());
    assert!(snapshot.get("events").is_none());
    assert!(snapshot.get("briefs").is_none());

    let task = snapshot["tasks"]
        .as_array()
        .and_then(|tasks| tasks.iter().find(|task| task["id"] == "large-task"))
        .expect("large task should be present");
    assert_eq!(task["detail"]["output_path"], "/tmp/large-task.log");
    assert!(
        task["detail"]["output_summary"]
            .as_str()
            .expect("task summary")
            .chars()
            .count()
            <= 2048
    );
    assert_eq!(task["detail"]["lines"].as_array().expect("lines").len(), 64);

    server.abort();
    Ok(())
}

pub async fn events_stream_with_missing_cursor_returns_not_found() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/agents/default/events/stream?after_seq=999"))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "cursor_not_found");
    server.abort();
    Ok(())
}
