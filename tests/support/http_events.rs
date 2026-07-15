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
        AdmissionContext, AgentStatus, AuditEvent, AuthorityClass, BriefCreatedAuditEvent,
        BriefKind, BriefRecord, CallbackDeliveryMode, CommandTaskSpec, ContinuationClass,
        ControlAction, ExternalTriggerStatus, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, OperatorDeliveryStatus, Priority, TaskKind, TaskRecord,
        TaskStatus, TodoItem, TodoItemState, ToolExecutionRecord, ToolExecutionStatus,
        WorkItemState,
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
    let mut request = client.get(format!(
        "{base}/api/agents/default/events?limit=1&order=desc"
    ));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let page: serde_json::Value = request.send().await?.json().await?;
    page["newest_seq"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("newest_seq should be present"))
}

async fn fetch_events_page_until(
    client: &Client,
    url: String,
    mut predicate: impl FnMut(&serde_json::Value) -> bool,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: serde_json::Value = client.get(&url).send().await?.json().await?;
        if predicate(&page) {
            return Ok(page);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for events page; last_page={page}");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

async fn lagged_test_host() -> Result<RuntimeHost> {
    let config = test_config();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider_and_event_bus_capacity_for_test(
        config,
        Arc::new(StubProvider::new("route result")),
        1,
    )?;
    attach_default_workspace(&host).await?;
    Ok(host)
}

async fn collect_sse_until_eof(response: reqwest::Response) -> Result<Vec<ParsedSseEvent>> {
    let body = tokio::time::timeout(Duration::from_secs(10), response.text()).await??;
    let events = body
        .split("\n\n")
        .filter_map(|frame| {
            let mut id = None;
            let mut event = None;
            let mut data = None;
            for line in frame.lines() {
                if let Some(value) = line.strip_prefix("id:") {
                    id = Some(value.trim().to_string());
                } else if let Some(value) = line.strip_prefix("event:") {
                    event = Some(value.trim().to_string());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data = serde_json::from_str(value.trim()).ok();
                }
            }
            Some(ParsedSseEvent {
                _id: id?,
                event: event?,
                data: data?,
            })
        })
        .collect();
    Ok(events)
}

fn append_lag_events(runtime: &holon::runtime::RuntimeHandle, count: usize) -> Result<()> {
    let padding = "x".repeat(64 * 1024);
    for n in 0..count {
        runtime.storage().append_event(&AuditEvent::new(
            "lag_test_event",
            serde_json::json!({ "n": n, "padding": padding }),
        ))?;
    }
    Ok(())
}

async fn assert_lagged_stream_recovers(
    base: &str,
    client: &Client,
    stream: reqwest::Response,
    expected_count: usize,
) -> Result<()> {
    let streamed = collect_sse_until_eof(stream).await?;
    let last_contiguous_seq = streamed
        .iter()
        .filter(|event| event.event == "lag_test_event")
        .filter_map(|event| event.data["event_seq"].as_u64())
        .max()
        .unwrap_or(0);
    assert!(
        streamed
            .iter()
            .filter(|event| event.event == "lag_test_event")
            .count()
            < expected_count,
        "the lagged stream should close before delivering every event"
    );

    let page: serde_json::Value = client
        .get(format!(
            "{base}/api/agents/default/events?after_seq={last_contiguous_seq}&limit=512&order=asc"
        ))
        .send()
        .await?
        .json()
        .await?;
    let recovered = page["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| event["type"] == "lag_test_event")
        .count();
    let streamed_count = streamed
        .iter()
        .filter(|event| event.event == "lag_test_event")
        .count();
    assert_eq!(streamed_count + recovered, expected_count);
    Ok(())
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
        .get(format!(
            "{base}/api/agents/default/events?limit=2&order=desc"
        ))
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
            "{base}/api/agents/default/events?before_seq={}&limit=2&order=desc",
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

    let newer = fetch_events_page_until(
        &client,
        format!(
            "{base}/api/agents/default/events?after_seq={}&limit=2&order=asc",
            older["newest_seq"].as_u64().expect("newest seq")
        ),
        |page| {
            page["events"]
                .as_array()
                .is_some_and(|events| events.len() == 2)
        },
    )
    .await?;
    let newer_events = newer["events"].as_array().expect("events");
    assert_eq!(newer_events.len(), 2);
    let newer_oldest = newer_events[0]["event_seq"]
        .as_u64()
        .expect("newer oldest seq");
    let newer_newest = newer_events[1]["event_seq"]
        .as_u64()
        .expect("newer newest seq");
    assert!(newer_oldest > older_newest);
    assert!(newer_newest > newer_oldest);
    assert_eq!(newer["oldest_seq"], newer_oldest);
    assert_eq!(newer["newest_seq"], newer_newest);
    assert_eq!(newer["has_older"], false);
    assert!(newer["has_newer"].as_bool().is_some());

    let bounded_newer: serde_json::Value = client
        .get(format!(
            "{base}/api/agents/default/events?after_seq={latest_oldest}&before_seq={}&limit=10&order=desc",
            latest_newest + 1
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
            "{base}/api/agents/default/events?after_seq={older_newest}&before_seq={latest_newest}&limit=10&order=asc"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(bounded_older["events"].as_array().expect("events").len(), 1);
    assert_eq!(bounded_older["has_older"], false);
    assert_eq!(bounded_older["has_newer"], false);

    let empty_cursor: serde_json::Value = client
        .get(format!(
            "{base}/api/agents/default/events?limit=2&order=desc"
        ))
        .send()
        .await?
        .json()
        .await?;
    let empty_cursor_events = empty_cursor["events"].as_array().expect("events");
    assert_eq!(empty_cursor_events.len(), 2);
    assert_eq!(
        empty_cursor["newest_seq"],
        empty_cursor_events[0]["event_seq"]
    );
    assert_eq!(
        empty_cursor["oldest_seq"],
        empty_cursor_events[1]["event_seq"]
    );

    server.abort();
    Ok(())
}

pub async fn events_route_supports_cursor_replay() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "first message" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "second message" }))
        .send()
        .await?;
    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "cursor bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "cursor replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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

pub async fn events_stream_receives_live_events_without_polling_replay() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let mut stream = client
        .get(format!("{base}/api/agents/default/events/stream"))
        .send()
        .await?;
    runtime.storage().append_event(&AuditEvent::new(
        "live_test_event",
        serde_json::json!({ "live": true }),
    ))?;

    let event = next_sse_event_kind(&mut stream, "live_test_event").await?;
    assert_eq!(event.data["agent_id"], "default");
    assert_eq!(event.data["payload"]["live"], true);

    server.abort();
    Ok(())
}

pub async fn global_events_stream_receives_live_agent_events() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let mut stream = client
        .get(format!("{base}/api/events/stream"))
        .send()
        .await?;
    runtime.storage().append_event(&AuditEvent::new(
        "global_live_test_event",
        serde_json::json!({ "global": true }),
    ))?;

    let event = next_sse_event_kind(&mut stream, "global_live_test_event").await?;
    assert_eq!(event.data["agent_id"], "default");
    assert_eq!(
        event.data["event_seq"].as_u64(),
        Some(event._id.parse::<u64>()?)
    );
    assert_eq!(event.data["payload"]["global"], true);

    server.abort();
    Ok(())
}

pub async fn events_stream_closes_on_lag_and_recovers_from_contiguous_cursor() -> Result<()> {
    let host = lagged_test_host().await?;
    let runtime = host.default_runtime().await?;
    let (base, server) = spawn_server_for_host(host).await?;
    let client = reqwest::Client::new();

    let stream = client
        .get(format!("{base}/api/agents/default/events/stream"))
        .send()
        .await?;
    append_lag_events(&runtime, 256)?;
    assert_lagged_stream_recovers(&base, &client, stream, 256).await?;

    server.abort();
    Ok(())
}

pub async fn global_events_stream_closes_on_lag_and_recovers_per_agent() -> Result<()> {
    let host = lagged_test_host().await?;
    let runtime = host.default_runtime().await?;
    let (base, server) = spawn_server_for_host(host).await?;
    let client = reqwest::Client::new();

    let stream = client
        .get(format!("{base}/api/events/stream"))
        .send()
        .await?;
    append_lag_events(&runtime, 256)?;
    assert_lagged_stream_recovers(&base, &client, stream, 256).await?;

    server.abort();
    Ok(())
}

pub async fn events_route_preserves_replay_provenance() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "provenance bootstrap" }))
        .send()
        .await?;
    wait_until(|| Ok(runtime.storage().read_recent_events(1)?.first().is_some())).await?;

    let after_seq = newest_event_seq(&base, &client, None).await?;

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "provenance replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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
        .post(format!("{base}/api/control/agents/default/prompt"))
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
            "raw_text": "debug-only prompt body"
        }),
    ))?;
    let mut brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "work finished",
        Some("msg-stable".into()),
        Some("task-stable".into()),
    );
    brief.work_item_id = Some("work-stable".into());
    let brief_id = brief.id.clone();
    runtime.storage().append_event(&AuditEvent::new(
        "brief_created",
        serde_json::to_value(BriefCreatedAuditEvent::from_brief(&brief))?,
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
            "stdout": "debug-only output"
        }),
    ))?;

    let page = fetch_events_page_until(
        &client,
        format!("{base}/api/agents/default/events?after_seq={after_seq}&limit=10&order=asc"),
        |page| {
            page["events"].as_array().is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["type"] == "message_admitted")
                    && events.iter().any(|event| event["type"] == "brief_created")
                    && events
                        .iter()
                        .any(|event| event["type"] == "task_status_updated")
            })
        },
    )
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

    let brief_event = events
        .iter()
        .find(|event| event["type"] == "brief_created")
        .expect("brief_created event");
    assert_eq!(brief_event["payload"]["brief_id"], brief_id);
    assert_eq!(brief_event["payload"]["work_item_id"], "work-stable");
    assert_eq!(brief_event["payload"]["related_message_id"], "msg-stable");
    assert_eq!(brief_event["payload"]["related_task_id"], "task-stable");
    assert_eq!(
        brief_event["payload"]["content_char_count"].as_u64(),
        Some(13)
    );
    assert!(brief_event["payload"].get("text").is_none());
    assert!(brief_event["payload"].get("attachments").is_none());
    assert!(brief_event.get("projection").is_none());

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
                "source": "github"
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
        serde_json::to_value(BriefCreatedAuditEvent::from_brief(&brief))?,
    ))?;

    let page = fetch_events_page_until(
        &client,
        format!("{base}/api/agents/default/events?limit=2&order=desc&max_level=info"),
        |page| {
            page["events"].as_array().is_some_and(|events| {
                events.len() == 2
                    && events[0]["type"] == "brief_created"
                    && events[1]["type"] == "message_enqueued"
            })
        },
    )
    .await?;
    let events = page["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "brief_created");
    assert_eq!(events[1]["type"], "message_enqueued");
    assert_eq!(page["has_older"], false);
    assert!(page["cursor_seq"].as_u64().is_some());

    let unfiltered: serde_json::Value = client
        .get(format!(
            "{base}/api/agents/default/events?limit=32&order=desc"
        ))
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
        .post(format!("{base}/api/control/agents/default/prompt"))
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
            "artifact_ref": "artifact://test-output"
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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

pub async fn tool_execution_route_returns_canonical_output() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    let record = ToolExecutionRecord {
        id: "tool-http-detail".into(),
        agent_id: "default".into(),
        work_item_id: Some("work-1".into()),
        turn_index: 3,
        turn_id: Some("turn-1".into()),
        tool_name: "ExecCommand".into(),
        created_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: 12,
        authority_class: AuthorityClass::OperatorInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd": "printf full-output"}),
        output: serde_json::json!({
            "disposition": "completed",
            "exit_status": 0,
            "stdout_preview": "full-output".repeat(128)
        }),
        summary: "command completed".into(),
        invocation_surface: None,
    };
    runtime.storage().append_tool_execution(&record)?;

    let response = client
        .get(format!(
            "{base}/api/agents/default/tool-executions/tool-http-detail"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: ToolExecutionRecord = response.json().await?;
    assert_eq!(body.id, record.id);
    assert_eq!(body.output, record.output);

    let missing = client
        .get(format!(
            "{base}/api/agents/default/tool-executions/missing-tool"
        ))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

pub async fn tool_execution_artifact_route_reads_scoped_content() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    let artifact_dir = runtime.storage().data_dir().join("tool-artifacts");
    std::fs::create_dir_all(&artifact_dir)?;
    let artifact_path = artifact_dir.join("http-artifact.log");
    let artifact_content = "complete stdout\nsecond line\n";
    std::fs::write(&artifact_path, artifact_content)?;
    let record = ToolExecutionRecord {
        id: "tool-http-artifact".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: None,
        tool_name: "ExecCommand".into(),
        created_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: 1,
        authority_class: AuthorityClass::OperatorInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd": "printf complete"}),
        output: serde_json::json!({
            "envelope": {
                "result": {
                    "disposition": "completed",
                    "truncated": true,
                    "stdout_preview": "complete stdout",
                    "artifacts": [{"path": artifact_path}],
                    "stdout_artifact": 0
                }
            }
        }),
        summary: "command completed".into(),
        invocation_surface: None,
    };
    runtime.storage().append_tool_execution(&record)?;

    let response = client
        .get(format!(
            "{base}/api/agents/default/tool-executions/tool-http-artifact/artifacts/0"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await?;
    assert_eq!(payload["artifact_index"], 0);
    assert_eq!(payload["content"], artifact_content);
    assert_eq!(payload["size"], artifact_content.len());

    let missing = client
        .get(format!(
            "{base}/api/agents/default/tool-executions/tool-http-artifact/artifacts/1"
        ))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

pub async fn tool_execution_artifact_route_rejects_outside_data_dir() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    let outside_dir = tempdir()?;
    let artifact_path = outside_dir.path().join("outside.log");
    std::fs::write(&artifact_path, "must not be readable")?;
    let record = ToolExecutionRecord {
        id: "tool-http-artifact-outside".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: None,
        tool_name: "ExecCommand".into(),
        created_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: 1,
        authority_class: AuthorityClass::OperatorInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd": "cat outside"}),
        output: serde_json::json!({
            "disposition": "completed",
            "truncated": true,
            "stdout_preview": "must not",
            "artifacts": [{"path": artifact_path}],
            "stdout_artifact": 0
        }),
        summary: "command completed".into(),
        invocation_surface: None,
    };
    runtime.storage().append_tool_execution(&record)?;

    let response = client
        .get(format!(
            "{base}/api/agents/default/tool-executions/tool-http-artifact-outside/artifacts/0"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn events_stream_includes_assistant_round_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/control/agents/default/prompt"))
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
            "provider_trace": { "detail": "debug-info" }
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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
        .post(format!("{base}/api/control/agents/default/prompt"))
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
            "access_mode": "shared_read"
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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
            "raw_text": "debug-only assistant body"
        }),
    ))?;

    let page: serde_json::Value = client
        .get(format!(
            "{base}/api/agents/default/events?limit=10&order=desc"
        ))
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
            "task_id": "task-tail"
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq={after_seq}"
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
        .get(format!("{base}/api/agents/default/events/stream"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

pub async fn state_snapshot_bounds_large_projection_fields() -> Result<()> {
    let (host, _base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let config = (*host.config()).clone();
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

    server.abort();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    let (base, server) = spawn_server_for_host(host).await?;

    let snapshot: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/agents/default/state"))
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
    // The slimmed snapshot strips detail entirely from task records.
    assert!(task["detail"].is_null());

    server.abort();
    Ok(())
}

pub async fn events_stream_with_missing_cursor_returns_not_found() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{base}/api/agents/default/events/stream?after_seq=999"
        ))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "cursor_not_found");
    server.abort();
    Ok(())
}
