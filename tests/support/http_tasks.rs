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
        OperatorDeliveryStatus, TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState,
        WaitingIntentStatus, WorkItemState,
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

pub async fn create_command_task_route_rejects_continue_on_result_field() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "run route command",
            "cmd": "printf route_command_ok",
            "continue_on_result": true
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

    let body = response.text().await?;
    assert!(body.contains("unknown field `continue_on_result`"));

    server.abort();
    Ok(())
}

pub async fn create_command_task_route_accepts_integration_authority() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "integration can create task",
            "cmd": "printf integration_signal_ok",
            "authority_class": "integration_signal"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let events = runtime.storage().read_recent_events(20)?;
        Ok(events.iter().any(|event| {
            event.kind == "task_create_requested"
                && event.data["provided_trust"] == "integration_signal"
                && event.data["effective_trust"] == "integration_signal"
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
            "login": false,
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

pub async fn tasks_and_state_routes_return_active_latest_tasks_only() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();
    let now = chrono::Utc::now();
    let task = |id: &str, status: TaskStatus, offset: i64| TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: status.clone(),
        created_at: now + chrono::Duration::seconds(offset),
        updated_at: now + chrono::Duration::seconds(offset),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(format!("{id} {status:?}")),
        detail: None,
        recovery: None,
    };

    runtime
        .storage()
        .append_task(&task("task-terminal", TaskStatus::Queued, 0))?;
    runtime
        .storage()
        .append_task(&task("task-running", TaskStatus::Running, 1))?;
    runtime
        .storage()
        .append_task(&task("task-terminal", TaskStatus::Completed, 2))?;
    runtime
        .storage()
        .append_task(&task("task-cancelling", TaskStatus::Cancelling, 3))?;

    let tasks: serde_json::Value = client
        .get(format!("{base}/agents/default/tasks"))
        .send()
        .await?
        .json()
        .await?;
    let task_ids = tasks
        .as_array()
        .expect("/tasks should return an array")
        .iter()
        .map(|task| task["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(task_ids, vec!["task-cancelling", "task-running"]);

    let snapshot: serde_json::Value = client
        .get(format!("{base}/agents/default/state"))
        .send()
        .await?
        .json()
        .await?;
    let state_task_ids = snapshot["tasks"]
        .as_array()
        .expect("/state.tasks should return an array")
        .iter()
        .map(|task| task["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(state_task_ids, vec!["task-cancelling", "task-running"]);

    server.abort();
    Ok(())
}

pub async fn task_status_and_output_routes_return_task_lifecycle_snapshots() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "inspect task lifecycle",
            "cmd": "printf lifecycle_output",
            "login": false,
            "yield_time_ms": 1000
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let created: serde_json::Value = response.json().await?;
    let task_id = created["id"]
        .as_str()
        .expect("task creation returns id")
        .to_string();

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let task = runtime.storage().latest_task_record(&task_id)?;
        Ok(task.is_some_and(|task| task.status == TaskStatus::Completed))
    })
    .await?;

    let status: serde_json::Value = client
        .get(format!("{base}/agents/default/tasks/{task_id}"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(status["task_id"], task_id);
    assert_eq!(status["status"], "completed");
    assert_eq!(status["wait_policy"], "background");

    let output: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/tasks/{task_id}/output?block=false"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(output["retrieval_status"], "success");
    assert_eq!(output["task"]["task_id"], task_id);
    assert_eq!(output["task"]["output_preview"], "lifecycle_output");

    let missing = client
        .get(format!("{base}/agents/default/tasks/missing-task"))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

pub async fn create_work_item_route_persists_queued_item_without_message_ingress() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "objective": "follow up on queued runtime cleanup"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["state"], "open");
    assert_eq!(body["objective"], "follow up on queued runtime cleanup");
    let work_item_id = body["id"]
        .as_str()
        .expect("response should include work item id")
        .to_string();

    let runtime = host.default_runtime().await?;
    wait_until(|| {
        let item = runtime.storage().latest_work_item(&work_item_id)?;
        let events = runtime.storage().read_recent_events(200)?;
        Ok(item.is_some_and(|item| {
            item.objective == "follow up on queued runtime cleanup"
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

pub async fn work_item_routes_list_and_return_work_item_detail() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "objective": "inspect lifecycle work item"
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let created: serde_json::Value = response.json().await?;
    let work_item_id = created["id"]
        .as_str()
        .expect("work item creation returns id")
        .to_string();

    let list: serde_json::Value = client
        .get(format!("{base}/agents/default/work-items"))
        .send()
        .await?
        .json()
        .await?;
    assert!(list
        .as_array()
        .expect("work-items returns an array")
        .iter()
        .any(|item| item["id"] == work_item_id));

    let detail: serde_json::Value = client
        .get(format!("{base}/agents/default/work-items/{work_item_id}"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(detail["id"], work_item_id);
    assert_eq!(detail["objective"], "inspect lifecycle work item");

    let missing = client
        .get(format!("{base}/agents/default/work-items/missing-work"))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

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
            "objective": "queued follow-up after active work",
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
                item.objective == "queued follow-up after active work"
                    && item.state == WorkItemState::Open
            }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn create_work_item_route_rejects_empty_objective_with_bad_request() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/work-items"))
        .json(&serde_json::json!({
            "objective": "   ",
            "summary": "queued from control plane"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "objective must not be empty");

    server.abort();
    Ok(())
}

pub async fn timer_detail_route_returns_latest_timer_record() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let timer = runtime
        .schedule_timer(5_000, None, Some("inspect lifecycle timer".into()))
        .await?;

    let detail: serde_json::Value = client
        .get(format!("{base}/agents/default/timers/{}", timer.id))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(detail["id"], timer.id);
    assert_eq!(detail["status"], "active");
    assert_eq!(detail["summary"], "inspect lifecycle timer");

    let missing = client
        .get(format!("{base}/agents/default/timers/missing-timer"))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}
