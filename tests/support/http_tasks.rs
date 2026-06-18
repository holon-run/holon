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
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use super::{
    attach_default_workspace, connect_addr, eventually_async, git, init_git_repo,
    read_next_sse_event, spawn_server, spawn_server_for_host, spawn_server_with_config,
    spawn_server_with_runtime_config, tempdir, test_config, test_config_with_paths, test_work_item,
    wait_until, ParsedSseEvent,
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
    let client = reqwest::Client::new();

    let create_task = |summary: &str, cmd: &str, yield_time_ms: u64| {
        client
            .post(format!("{base}/control/agents/default/tasks"))
            .json(&serde_json::json!({
                "summary": summary,
                "cmd": cmd,
                "login": false,
                "yield_time_ms": yield_time_ms
            }))
    };

    let first: serde_json::Value = create_task("active one", "sleep 30", 1)
        .send()
        .await?
        .json()
        .await?;
    let first_id = first["id"].as_str().expect("task id").to_string();
    let second: serde_json::Value = create_task("active two", "sleep 30", 1)
        .send()
        .await?
        .json()
        .await?;
    let second_id = second["id"].as_str().expect("task id").to_string();
    let terminal: serde_json::Value = create_task("terminal task", "printf done", 1000)
        .send()
        .await?
        .json()
        .await?;
    let terminal_id = terminal["id"].as_str().expect("task id").to_string();

    let runtime = host.default_runtime().await?;
    eventually_async(|| {
        let runtime = runtime.clone();
        let first_id = first_id.clone();
        let second_id = second_id.clone();
        let terminal_id = terminal_id.clone();
        async move {
            let tasks = runtime.active_tasks(10).await?;
            Ok(tasks.iter().any(|task| task.id == first_id)
                && tasks.iter().any(|task| task.id == second_id)
                && tasks.iter().all(|task| task.id != terminal_id))
        }
    })
    .await?;

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
    assert!(task_ids.contains(&first_id.as_str()));
    assert!(task_ids.contains(&second_id.as_str()));

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
    assert_eq!(state_task_ids, task_ids);

    client
        .post(format!(
            "{base}/control/agents/default/tasks/{first_id}/stop"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!(
            "{base}/control/agents/default/tasks/{second_id}/stop"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?
        .error_for_status()?;

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

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "inspect delayed task output",
            "cmd": "sleep 0.2; printf delayed_lifecycle_output",
            "login": false,
            "yield_time_ms": 1
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let delayed: serde_json::Value = response.json().await?;
    let delayed_task_id = delayed["id"]
        .as_str()
        .expect("task creation returns id")
        .to_string();

    let delayed_output: serde_json::Value = client
        .get(format!(
            "{base}/agents/default/tasks/{delayed_task_id}/output?block=true"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(delayed_output["retrieval_status"], "success");
    assert_eq!(
        delayed_output["task"]["output_preview"],
        "delayed_lifecycle_output"
    );

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

pub async fn task_input_and_stop_routes_manage_task_lifecycle() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "accept task input",
            "cmd": "cat",
            "login": false,
            "accepts_input": true,
            "yield_time_ms": 1
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let created: serde_json::Value = response.json().await?;
    let input_task_id = created["id"]
        .as_str()
        .expect("task creation returns id")
        .to_string();

    let input: serde_json::Value = client
        .post(format!(
            "{base}/control/agents/default/tasks/{input_task_id}/input"
        ))
        .json(&serde_json::json!({
            "text": "managed input\n"
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(input["task"]["task_id"], input_task_id);
    assert_eq!(input["accepted_input"], true);

    let response = client
        .post(format!("{base}/control/agents/default/tasks"))
        .json(&serde_json::json!({
            "summary": "stop task through route",
            "cmd": "sleep 30",
            "login": false,
            "yield_time_ms": 1
        }))
        .send()
        .await?;
    assert!(response.status().is_success());
    let created: serde_json::Value = response.json().await?;
    let stop_task_id = created["id"]
        .as_str()
        .expect("task creation returns id")
        .to_string();

    let stop: serde_json::Value = client
        .post(format!(
            "{base}/control/agents/default/tasks/{stop_task_id}/stop"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(stop["task"]["task_id"], stop_task_id);
    assert_eq!(stop["stop_requested"], true);

    let missing = client
        .post(format!(
            "{base}/control/agents/default/tasks/missing-task/stop"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

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

pub async fn work_item_mutation_routes_pick_update_and_complete() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let runtime = host.default_runtime().await?;

    let first = runtime
        .create_work_item("first queued item".into(), None, None, Vec::new())
        .await?;
    let second = runtime
        .create_work_item("second queued item".into(), None, None, Vec::new())
        .await?;
    runtime.pick_work_item(first.id.clone()).await?;

    let pick: serde_json::Value = client
        .post(format!(
            "{base}/control/agents/default/work-items/{second_id}/pick",
            second_id = second.id
        ))
        .json(&serde_json::json!({
            "reason": "HTTP client selected next work",
            "authority_class": "integration_signal"
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(pick["current_work_item_id"], second.id);
    assert_eq!(pick["previous_work_item"]["id"], first.id);
    assert_eq!(
        pick["transition"]["reason"],
        "HTTP client selected next work"
    );

    let update: serde_json::Value = client
        .patch(format!(
            "{base}/control/agents/default/work-items/{second_id}",
            second_id = second.id
        ))
        .json(&serde_json::json!({
            "objective": "refined via HTTP",
            "plan_status": "ready",
            "todo_list": [
                {"text": "wire HTTP endpoint", "state": "completed"},
                {"text": "verify behavior", "state": "in_progress"}
            ],
            "blocked_by": "waiting on review",
            "recheck_after": 60000,
            "authority_class": "integration_signal"
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(update["id"], second.id);
    assert_eq!(update["objective"], "refined via HTTP");
    assert_eq!(update["plan_status"], "ready");
    assert_eq!(update["blocked_by"], "waiting on review");
    assert!(update["recheck_at"].is_string());
    assert_eq!(update["todo_list"].as_array().expect("todo array").len(), 2);

    let complete: serde_json::Value = client
        .post(format!(
            "{base}/control/agents/default/work-items/{second_id}/complete",
            second_id = second.id
        ))
        .json(&serde_json::json!({
            "authority_class": "integration_signal"
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(complete["id"], second.id);
    assert_eq!(complete["state"], "completed");
    assert!(complete.get("blocked_by").is_none());

    wait_until(|| {
        let events = runtime.storage().read_recent_events(200)?;
        Ok([
            "work_item_pick_requested",
            "work_item_update_requested",
            "work_item_complete_requested",
        ]
        .into_iter()
        .all(|kind| {
            events.iter().any(|event| {
                event.kind == kind && event.data["provided_trust"] == "integration_signal"
            })
        }))
    })
    .await?;

    server.abort();
    Ok(())
}

pub async fn work_item_mutation_routes_validate_bad_requests() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let runtime = host.default_runtime().await?;
    let work = runtime
        .create_work_item(
            "validate HTTP work item mutation".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;

    let empty_update = client
        .patch(format!(
            "{base}/control/agents/default/work-items/{}",
            work.id
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert_eq!(empty_update.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = empty_update.json().await?;
    assert_eq!(
        body["error"],
        "request must include at least one mutation field"
    );

    let recheck_without_blocker = client
        .patch(format!(
            "{base}/control/agents/default/work-items/{}",
            work.id
        ))
        .json(&serde_json::json!({
            "blocked_by": null,
            "recheck_after": 1000
        }))
        .send()
        .await?;
    assert_eq!(
        recheck_without_blocker.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let missing = client
        .post(format!(
            "{base}/control/agents/default/work-items/missing-work/complete"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

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

pub async fn timer_cancel_route_is_idempotent_and_updates_projection() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let client = reqwest::Client::new();

    let timer = runtime
        .schedule_timer(5_000, None, Some("cancel lifecycle timer".into()))
        .await?;

    let cancelled_response = client
        .post(format!(
            "{base}/control/agents/default/timers/{}/cancel",
            timer.id
        ))
        .json(&serde_json::json!({"authority_class": "integration_signal"}))
        .send()
        .await?;
    assert_eq!(cancelled_response.status(), reqwest::StatusCode::OK);
    let cancelled_body = cancelled_response.text().await?;
    let cancelled: serde_json::Value = serde_json::from_str(&cancelled_body)?;
    assert_eq!(cancelled["id"], timer.id);
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["next_fire_at"].is_null());

    let idempotent: serde_json::Value = client
        .post(format!(
            "{base}/control/agents/default/timers/{}/cancel",
            timer.id
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(idempotent["status"], "cancelled");

    let detail: serde_json::Value = client
        .get(format!("{base}/agents/default/timers/{}", timer.id))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(detail["status"], "cancelled");

    let missing = client
        .post(format!(
            "{base}/control/agents/default/timers/missing-timer/cancel"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let completed = runtime
        .schedule_timer(1, None, Some("already completed timer".into()))
        .await?;
    eventually_async({
        let client = client.clone();
        let base = base.clone();
        let completed_id = completed.id.clone();
        move || {
            let client = client.clone();
            let base = base.clone();
            let completed_id = completed_id.clone();
            async move {
                let detail: serde_json::Value = client
                    .get(format!("{base}/agents/default/timers/{completed_id}"))
                    .send()
                    .await?
                    .json()
                    .await?;
                Ok(detail["status"] == "completed")
            }
        }
    })
    .await?;
    let completed_cancel = client
        .post(format!(
            "{base}/control/agents/default/timers/{}/cancel",
            completed.id
        ))
        .json(&serde_json::json!({}))
        .send()
        .await?;
    assert_eq!(completed_cancel.status(), reqwest::StatusCode::BAD_REQUEST);

    let events = runtime.all_events()?;
    assert!(events
        .iter()
        .any(|event| { event.kind == "timer_cancelled" && event.data["timer_id"] == timer.id }));
    assert!(events.iter().any(|event| {
        event.kind == "timer_cancel_requested"
            && event.data["timer_id"] == timer.id
            && event.data["provided_trust"] == "integration_signal"
    }));

    server.abort();
    Ok(())
}
