// HTTP workspace route integration tests.

#![allow(dead_code, unused_imports)]

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use chrono::Utc;
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
        ExecutionRootEntry, ExternalTriggerStatus, MessageBody, MessageDeliverySurface,
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

use super::runtime_helpers::wait_until_async_for;
use super::{
    attach_default_workspace, connect_addr, git, init_git_repo, read_next_sse_event, spawn_server,
    spawn_server_for_host, spawn_server_with_config, spawn_server_with_runtime_config,
    spawn_unix_server, tempdir, test_config, test_config_with_paths, unix_request, ParsedSseEvent,
    RuntimeFailureProvider,
};

pub async fn workspace_enter_control_route_is_not_exposed() -> Result<()> {
    let config = test_config();
    let (_host, socket_path, server) = spawn_unix_server(config).await?;

    let response = unix_request(
        &socket_path,
        "POST",
        "/api/control/agents/default/workspace/enter",
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
        .post(format!(
            "{base}/api/control/agents/default/workspace/detach"
        ))
        .json(&serde_json::json!({
            "workspace_id": stale_workspace.workspace_id.clone()
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
        .post(format!(
            "{base}/api/control/agents/default/workspace/detach"
        ))
        .json(&serde_json::json!({
            "workspace_id": active_workspace_id.clone()
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
            AuthorityClass::OperatorInstruction,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    wait_until_async_for(Duration::from_secs(10), || async {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|task| {
            task.is_worktree_child_agent_task()
                && matches!(task.status, holon::types::TaskStatus::Completed)
        }))
    })
    .await?;

    let response = client
        .get(format!("{base}/api/agents/default/worktree-summary"))
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

pub async fn workspace_files_lists_directory() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let response = client
        .get(format!("{base}/api/workspaces/{workspace_id}/files"))
        .send()
        .await?;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["type"], "directory");
    let entries = body["entries"].as_array().expect("entries array");
    assert!(!entries.is_empty(), "root listing should not be empty");
    for entry in entries {
        assert!(entry["name"].is_string(), "entry has name");
        assert!(entry["type"].is_string(), "entry has type");
    }

    server.abort();
    Ok(())
}

pub async fn workspace_files_reads_text_file() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let listing: serde_json::Value = client
        .get(format!("{base}/api/workspaces/{workspace_id}/files"))
        .send()
        .await?
        .json()
        .await?;
    let entries = listing["entries"].as_array().unwrap();
    let target = entries
        .iter()
        .find(|e| e["type"] == "file")
        .expect("should have at least one file in workspace root");
    let filename = target["name"].as_str().unwrap();

    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/{filename}"
        ))
        .header("Accept", "application/json")
        .send()
        .await?;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["type"], "file");
    assert!(body["content"].is_string(), "content field present");
    assert!(body["mime_type"].is_string(), "mime_type field present");
    assert_eq!(body["truncated"], false);

    server.abort();
    Ok(())
}

pub async fn workspace_files_path_traversal_rejected() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/../../../etc/passwd"
        ))
        .send()
        .await?;
    assert_ne!(response.status(), 200, "path traversal must not return 200");

    server.abort();
    Ok(())
}

pub async fn workspace_files_returns_404_for_missing_file() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/nonexistent_file_12345.txt"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 404);

    server.abort();
    Ok(())
}

pub async fn workspace_files_metadata_only() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let listing: serde_json::Value = client
        .get(format!("{base}/api/workspaces/{workspace_id}/files"))
        .send()
        .await?
        .json()
        .await?;
    let entries = listing["entries"].as_array().unwrap();
    let target = entries
        .iter()
        .find(|e| e["type"] == "file")
        .expect("should have at least one file");
    let filename = target["name"].as_str().unwrap();

    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/{filename}?meta=true"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["type"], "file");
    assert!(body["size"].is_number(), "size present");
    assert!(body["mime_type"].is_string(), "mime_type present");
    assert!(
        body.get("content").is_none(),
        "content must be absent in meta mode"
    );

    server.abort();
    Ok(())
}

pub async fn workspace_files_unknown_workspace_404() -> Result<()> {
    let (_host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/api/workspaces/ws_nonexistent_12345/files"))
        .send()
        .await?;
    assert_eq!(response.status(), 404);

    server.abort();
    Ok(())
}

pub async fn workspace_files_symlink_escape_rejected() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    // Find the default workspace root so we can plant a symlink inside it.
    let workspace_id = "agent_home:default";
    let entries = host.workspace_entries()?;
    let workspace = entries
        .iter()
        .find(|e| e.workspace_id == workspace_id)
        .expect("default workspace should exist");

    // Create a symlink that points outside the workspace root.
    let link_path = workspace.workspace_anchor.join("escape_link");
    let _ = std::fs::remove_file(&link_path);
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/passwd", &link_path)?;

    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/escape_link"
        ))
        .send()
        .await?;
    // Must not return 200 — symlink escape should be blocked.
    assert_ne!(response.status(), 200, "symlink escape must not return 200");

    let _ = std::fs::remove_file(&link_path);
    server.abort();
    Ok(())
}

pub async fn workspace_files_execution_root_id_resolves_registered_root() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let execution_root_id = "test-worktree-root";
    let execution_root = tempdir()?;
    std::fs::write(execution_root.path().join("from-worktree.txt"), "worktree")?;
    host.runtime_db()
        .execution_root_entries()
        .upsert(&ExecutionRootEntry {
            execution_root_id: execution_root_id.into(),
            workspace_id: workspace_id.into(),
            filesystem_path: execution_root.path().to_path_buf(),
            root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            created_at: Utc::now(),
            removed_at: None,
        })?;

    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/from-worktree.txt?root={execution_root_id}"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 200, "{}", response.text().await?);

    server.abort();
    Ok(())
}
