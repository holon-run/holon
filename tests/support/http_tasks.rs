// HTTP tasks route integration tests.

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
    test_config, test_config_with_paths, test_work_item, wait_until, ParsedSseEvent,
};

pub async fn create_command_task_route_rejects_legacy_kind_field() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    for kind in ["subagent_task", "worktree_subagent_task"] {
        let response = client
            .post(format!("{base}/control/agents/default/tasks"))
            .json(&serde_json::json!({
                "kind": kind,
                "summary": "delegate through deprecated control task path",
                "cmd": "printf should_not_run"
            }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

        let body = response.text().await?;
        assert!(body.contains("unknown field `kind`"));
    }

    server.abort();
    Ok(())
}

pub async fn create_task_route_rejects_unknown_prompt_field() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "run route command",
            "cmd": "printf route_command_ok",
            "prompt": "should be rejected"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

    let body = response.text().await?;
    assert!(body.contains("unknown field `prompt`"));

    server.abort();
    Ok(())
}

pub async fn create_command_task_route_no_longer_denies_integration_trust() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "integration can create task",
            "cmd": "printf trusted_integration_ok",
            "trust": "trusted_integration"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let events = runtime.storage().read_recent_events(20)?;
        Ok(events.iter().any(|event| {
            event.kind == "task_create_requested"
                && event.data["provided_trust"] == "trusted_integration"
                && event.data["effective_trust"] == "trusted_integration"
        }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn create_command_task_route_accepts_command_request() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "run route command",
            "cmd": "printf route_command_ok",
            "yield_time_ms": 1000
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|task| {
            task.kind.as_str() == "command_task"
                && matches!(task.status, holon::types::TaskStatus::Completed)
        }))
    })
    .await?;

    let task = runtime
        .storage()
        .latest_task_records()?
        .into_iter()
        .find(|task| task.kind.as_str() == "command_task")
        .expect("command_task should exist");
    let detail = task.detail.unwrap_or_default();
    assert_eq!(detail["cmd"], "printf route_command_ok");
    assert!(detail["output_path"].as_str().is_some());
    server.abort();
    Ok(())
}

pub async fn create_work_item_route_persists_queued_item_without_message_ingress() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "delivery_target": "follow up on queued runtime cleanup"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["state"], "open");
    assert_eq!(
        body["delivery_target"],
        "follow up on queued runtime cleanup"
    );
    let work_item_id = body["id"]
        .as_str()
        .expect("response should include work item id")
        .to_string();

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let item = runtime.storage().latest_work_item(&work_item_id)?;
        let events = runtime.storage().read_recent_events(200)?;
        Ok(item.is_some_and(|item| {
            item.delivery_target == "follow up on queued runtime cleanup"
                && item.state == WorkItemState::Open
        }) && events.iter().any(|event| {
            event.kind == "work_item_enqueue_requested"
                && event.data["work_item_id"] == work_item_id
                && event.data["target_agent_id"] == "default"
        }))
    })
    .await?;

    let messages = runtime.storage().read_recent_messages(10)?;
    assert!(messages
        .iter()
        .all(|message| { matches!(message.kind, holon::types::MessageKind::SystemTick) }));

    server.abort();
    Ok(())
}

pub async fn create_work_item_route_does_not_replace_existing_active_item() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let runtime = host.default_runtime().await?;

    let active = test_work_item(
        &runtime,
        "finish current active work",
        WorkItemState::Open,
        true,
        None,
    )
    .await?;

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "delivery_target": "queued follow-up after active work",
            "summary": "queued from route"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    wait_until(|| {
        let work_items = runtime.storage().latest_work_items()?;
        Ok(work_items
            .iter()
            .any(|item| item.id == active.id && item.state == WorkItemState::Open)
            && work_items.iter().any(|item| {
                item.delivery_target == "queued follow-up after active work"
                    && item.state == WorkItemState::Open
            }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn create_work_item_route_rejects_empty_delivery_target_with_bad_request() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "delivery_target": "   ",
            "summary": "queued from control plane"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "delivery_target must not be empty");

    server.abort();
    Ok(())
}
