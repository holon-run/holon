// HTTP workspace route integration tests.

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
    attach_default_workspace, connect_addr, git, init_git_repo, read_next_sse_event, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config,
    spawn_unix_server, tempdir, test_config, test_config_with_paths, unix_request, wait_until,
    ParsedSseEvent, RuntimeFailureProvider,
};

pub async fn workspace_enter_control_route_is_not_exposed() -> Result<()> {
    let config = test_config();
    let (_host, socket_path, server) = spawn_unix_server(config).await?;

    let response = unix_request(
        &socket_path,
        "POST",
        "/control/agents/default/workspace/enter",
        &[("Content-Type", "application/json")],
        Some(br#"{}"#),
    )
    .await?;

    assert_eq!(response.status, 404);

    server.abort();
    Ok(())
}

pub async fn detach_workspace_route_removes_stale_non_active_binding() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let stale_dir = tempdir()?.keep();
    std::fs::create_dir_all(&stale_dir)?;
    let stale_workspace = host.ensure_workspace_entry(stale_dir.clone())?;
    runtime.attach_workspace(&stale_workspace).await?;
    std::fs::remove_dir_all(&stale_dir)?;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/control/agents/default/workspace/detach"))
        .json(&serde_json::json!({
            "workspace_id": stale_workspace.workspace_id.clone(),
        }))
        .send()
        .await?;

    assert!(response.status().is_success(), "{}", response.text().await?);
    let state = runtime.agent_state().await?;
    assert!(!state
        .attached_workspaces
        .contains(&stale_workspace.workspace_id));
    assert!(host
        .workspace_entries()?
        .iter()
        .any(|entry| entry.workspace_id == stale_workspace.workspace_id));

    server.abort();
    Ok(())
}

pub async fn detach_workspace_route_rejects_active_binding() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let active_workspace_id = runtime
        .agent_state()
        .await?
        .active_workspace_entry
        .as_ref()
        .map(|e| e.workspace_id.clone())
        .expect("default workspace should be active");

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/control/agents/default/workspace/detach"))
        .json(&serde_json::json!({
            "workspace_id": active_workspace_id.clone(),
        }))
        .send()
        .await?;

    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body = response.text().await?;
    assert!(
        body.contains("UseWorkspace with another workspace_id"),
        "{body}"
    );
    let state = runtime.agent_state().await?;
    assert!(state.attached_workspaces.contains(&active_workspace_id));

    server.abort();
    Ok(())
}

pub async fn worktree_summary_route_returns_reviewable_candidate_summary() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();
    let runtime = host.default_runtime().await?;

    runtime
        .schedule_child_agent_task(
            "compare worktree candidate".into(),
            "return a worktree result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|task| {
            task.is_worktree_child_agent_task()
                && matches!(task.status, holon::types::TaskStatus::Completed)
        }))
    })
    .await?;

    let response = client
        .get(format!("{base}/agents/default/worktree-summary"))
        .send()
        .await?;
    assert!(response.status().is_success());

    let payload: serde_json::Value = response.json().await?;
    let summary = payload["summary"].as_str().unwrap_or_default();
    assert!(summary.contains("Worktree Task Summary"));
    assert!(summary.contains("Total tasks: 1"));
    assert!(summary.contains("compare worktree candidate"));
    assert!(summary.contains("Worktree path:"));

    server.abort();
    Ok(())
}
