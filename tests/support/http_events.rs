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
        ExternalTriggerStatus, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        OperatorDeliveryStatus, TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState,
        TranscriptEntry, TranscriptEntryKind, TrustLevel, WaitingIntentStatus, WorkItemState,
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
        .json(&serde_json::json!({ "text": "provenance replay" }))
        .send()
        .await?;

    let mut stream = client
        .get(format!("{base}/agents/default/events?since={cursor}"))
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;
    assert_eq!(replayed.event, "message_admitted");
    assert_eq!(replayed.data["type"], "message_admitted");
    assert_eq!(replayed.data["provenance"]["origin"]["kind"], "operator");
    assert_eq!(replayed.data["provenance"]["trust"], "trusted_operator");
    assert_eq!(
        replayed.data["provenance"]["authority_class"],
        "operator_instruction"
    );
    assert_eq!(
        replayed.data["provenance"]["delivery_surface"],
        "http_control_prompt"
    );
    assert_eq!(
        replayed.data["projection"]["name"],
        serde_json::json!("operator")
    );
    assert_eq!(
        replayed.data["projection"]["raw_payload_included"],
        serde_json::json!(true)
    );

    server.abort();
    Ok(())
}

pub async fn events_route_operator_projection_preserves_tool_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "operator projection bootstrap" }))
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

    runtime.storage().append_event(&AuditEvent::new(
        "tool_executed",
        serde_json::json!({
            "agent_id": "default",
            "tool_name": "ExecCommand",
            "task_id": "task-secret",
            "exec_command_cmd": "cat /private/tmp/secret-command.txt",
            "raw_output": "secret command output",
            "local_path": "/private/tmp/secret-output.txt",
            "artifact_ref": "artifact://secret",
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events?since={cursor}&projection=operator"
        ))
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;
    assert_eq!(replayed.event, "tool_executed");
    assert_eq!(replayed.data["type"], "tool_executed");
    assert_eq!(replayed.data["projection"]["name"], "operator");
    assert_eq!(replayed.data["projection"]["raw_payload_included"], true);
    assert_eq!(replayed.data["payload"]["tool_name"], "ExecCommand");
    assert_eq!(replayed.data["payload"]["task_id"], "task-secret");
    assert_eq!(
        replayed.data["payload"]["exec_command_cmd"],
        "cat /private/tmp/secret-command.txt"
    );
    assert_eq!(
        replayed.data["payload"]["raw_output"],
        "secret command output"
    );
    assert_eq!(
        replayed.data["payload"]["local_path"],
        "/private/tmp/secret-output.txt"
    );
    assert_eq!(
        replayed.data["payload"]["artifact_ref"],
        "artifact://secret"
    );

    server.abort();
    Ok(())
}

pub async fn events_route_operator_projection_preserves_assistant_round_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "assistant round projection bootstrap" }))
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
            "raw_text": "full assistant text should stay in operator replay",
            "provider_trace": { "secret": "debug-only" },
        }),
    ))?;

    let mut stream = client
        .get(format!(
            "{base}/agents/default/events?since={cursor}&projection=operator"
        ))
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;

    assert_eq!(replayed.event, "assistant_round_recorded");
    assert_eq!(replayed.data["type"], "assistant_round_recorded");
    assert_eq!(replayed.data["projection"]["name"], "operator");
    assert_eq!(
        replayed.data["projection"]["raw_payload_included"],
        serde_json::json!(true)
    );
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
        "full assistant text should stay in operator replay"
    );
    assert_eq!(
        replayed.data["payload"]["provider_trace"]["secret"],
        "debug-only"
    );

    server.abort();
    Ok(())
}

pub async fn events_route_operator_projection_preserves_workspace_payload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/control/agents/default/prompt"))
        .json(&serde_json::json!({ "text": "workspace projection bootstrap" }))
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
            "{base}/agents/default/events?since={cursor}&projection=operator"
        ))
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;

    assert_eq!(replayed.event, "workspace_used");
    assert_eq!(replayed.data["type"], "workspace_used");
    assert_eq!(replayed.data["projection"]["name"], "operator");
    assert_eq!(
        replayed.data["projection"]["raw_payload_included"],
        serde_json::json!(true)
    );
    assert_eq!(replayed.data["payload"]["workspace_id"], "ws-1");
    assert_eq!(replayed.data["payload"]["workspace_label"], "holon");
    assert_eq!(
        replayed.data["payload"]["execution_root"],
        "/repo/holon/worktree"
    );

    server.abort();
    Ok(())
}

pub async fn state_snapshot_seeds_projected_events_tail_and_stream_resumes_after_cursor(
) -> Result<()> {
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

    let snapshot: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    let events_tail = snapshot["events_tail"]
        .as_array()
        .expect("events_tail should be an array");
    let tail_cursor = events_tail
        .last()
        .and_then(|event| event["id"].as_str())
        .expect("events_tail should include at least one event");
    assert_eq!(snapshot["cursor"], tail_cursor);
    let assistant_tail = events_tail
        .iter()
        .find(|event| event["type"] == "assistant_round_recorded")
        .expect("events_tail should include the appended assistant round");
    assert_eq!(assistant_tail["projection"]["name"], "operator");
    assert_eq!(
        assistant_tail["projection"]["raw_payload_included"],
        serde_json::json!(true)
    );
    assert_eq!(assistant_tail["payload"]["stop_reason"], "tool_use");
    assert_eq!(
        assistant_tail["payload"]["tool_names"],
        serde_json::json!(["ExecCommand"])
    );
    assert_eq!(
        assistant_tail["payload"]["raw_text"],
        "debug-only assistant body"
    );

    let cursor = snapshot["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();
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
            "{base}/agents/default/events?since={cursor}&projection=operator"
        ))
        .send()
        .await?;
    let replayed =
        tokio::time::timeout(Duration::from_secs(5), read_next_sse_event(&mut stream)).await??;
    assert_eq!(replayed.event, "tool_executed");
    assert_ne!(replayed._id, cursor);

    server.abort();
    Ok(())
}

pub async fn events_route_local_debug_projection_requires_control_auth() -> Result<()> {
    let config = test_config_with_paths(
        tempdir()?.keep(),
        tempdir()?.keep(),
        "127.0.0.1:0".into(),
        ControlAuthMode::Required,
    );
    let (_host, base, server) = spawn_server_with_config(config).await?;
    let client = reqwest::Client::new();

    let response = client
        .get(format!(
            "{base}/agents/default/events?projection=local_debug"
        ))
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

    runtime.storage().append_task(&TaskRecord {
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
    })?;
    runtime
        .storage()
        .append_transcript_entry(&TranscriptEntry::new(
            "default",
            TranscriptEntryKind::ToolResults,
            Some(1),
            None,
            serde_json::json!({ "content": "y".repeat(20_000) }),
        ))?;

    let snapshot: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;

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

    let transcript = snapshot["transcript_tail"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["data"]["content"].as_str().is_some())
        })
        .expect("large transcript should be present");
    assert!(
        transcript["data"]["content"]
            .as_str()
            .expect("transcript content")
            .chars()
            .count()
            <= 8192
    );

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
