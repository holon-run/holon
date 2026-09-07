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
    RuntimeFailureProvider, TempDir,
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

pub async fn detach_workspace_route_falls_back_from_active_binding() -> Result<()> {
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

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let state = runtime.agent_state().await?;
    assert!(!state.attached_workspaces.contains(&active_workspace_id));
    assert_eq!(
        state.active_workspace_entry.unwrap().workspace_id,
        holon::types::agent_home_workspace_id("default")
    );

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

/// Spawn a server with an extra workspace containing the given files, and
/// return `(host, base, server, workspace_id, temp_dir)`. The caller must keep
/// the returned `TempDir` alive for the duration of the test.
async fn spawn_workspace_with_files(
    files: &[(&str, Vec<u8>)],
) -> Result<(
    RuntimeHost,
    String,
    super::TestServerHandle,
    String,
    TempDir,
)> {
    let (host, base, server) = spawn_server().await?;
    let runtime = host.default_runtime().await?;
    let dir = tempdir()?;
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents)?;
    }
    let workspace = host.ensure_workspace_entry(dir.path().to_path_buf())?;
    runtime.attach_workspace(&workspace).await?;
    Ok((host, base, server, workspace.workspace_id, dir))
}

pub async fn workspace_files_serves_range_requests() -> Result<()> {
    let data = b"abcdefghijklmnopqrstuvwxyz";
    let (_host, base, server, workspace_id, _dir) =
        spawn_workspace_with_files(&[("data.bin", data.to_vec())]).await?;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/workspaces/{workspace_id}/files/data.bin");

    // Full response advertises range support and cache validators.
    let response = client.get(&url).send().await?;
    assert_eq!(response.status(), 200, "{}", response.text().await?);
    let headers = response.headers().clone();
    assert_eq!(headers["accept-ranges"], "bytes");
    assert!(headers.contains_key("etag"));
    assert!(headers.contains_key("last-modified"));
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(response.bytes().await?, data.as_ref());

    // Single inclusive range.
    let response = client.get(&url).header("Range", "bytes=0-3").send().await?;
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 0-3/26");
    assert_eq!(&response.bytes().await?[..], b"abcd");

    // Open-ended range.
    let response = client.get(&url).header("Range", "bytes=20-").send().await?;
    assert_eq!(response.status(), 206);
    assert_eq!(&response.bytes().await?[..], b"uvwxyz");

    // Suffix range.
    let response = client.get(&url).header("Range", "bytes=-4").send().await?;
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 22-25/26");
    assert_eq!(&response.bytes().await?[..], b"wxyz");

    // Out-of-bounds range.
    let response = client
        .get(&url)
        .header("Range", "bytes=100-200")
        .send()
        .await?;
    assert_eq!(response.status(), 416);
    assert_eq!(response.headers()["content-range"], "bytes */26");

    // Multi-range requests fall back to the full representation.
    let response = client
        .get(&url)
        .header("Range", "bytes=0-1,3-4")
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?, data.as_ref());

    // Reversed ranges are invalid specs (RFC 9110 §14.1.2); the full
    // representation is served instead of underflowing the length.
    let response = client.get(&url).header("Range", "bytes=5-3").send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?, data.as_ref());

    server.abort();
    Ok(())
}

pub async fn workspace_files_conditional_requests() -> Result<()> {
    let data = b"abcdefghijklmnopqrstuvwxyz";
    let (_host, base, server, workspace_id, _dir) =
        spawn_workspace_with_files(&[("data.bin", data.to_vec())]).await?;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/workspaces/{workspace_id}/files/data.bin");

    let response = client.get(&url).send().await?;
    let etag = response.headers()["etag"]
        .to_str()
        .expect("etag header")
        .to_string();

    // Matching If-None-Match returns 304 without a body.
    let response = client
        .get(&url)
        .header("If-None-Match", &etag)
        .send()
        .await?;
    assert_eq!(response.status(), 304);
    assert_eq!(response.headers()["etag"], etag.as_str());
    assert!(response.bytes().await?.is_empty());

    // Non-matching If-None-Match returns the full representation.
    let response = client
        .get(&url)
        .header("If-None-Match", "\"stale-tag\"")
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?, data.as_ref());

    // If-Range mismatch serves the full representation.
    let response = client
        .get(&url)
        .header("If-Range", "\"stale-tag\"")
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?, data.as_ref());

    // If-Range match authorizes the range.
    let response = client
        .get(&url)
        .header("If-Range", &etag)
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(response.status(), 206);
    assert_eq!(&response.bytes().await?[..], b"abcd");

    // Weak tags never authorize a range: If-Range requires strong
    // comparison (RFC 9110 §13.1.5), so the full body is served.
    let response = client
        .get(&url)
        .header("If-Range", format!("W/{etag}"))
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?, data.as_ref());

    server.abort();
    Ok(())
}

pub async fn workspace_files_download_and_inline_safety() -> Result<()> {
    let big_text = vec![b'x'; 1024 * 1024 + 1000];
    let (_host, base, server, workspace_id, _dir) = spawn_workspace_with_files(&[
        (
            "page.html",
            b"<html><body><script>alert(1)</script></body></html>".to_vec(),
        ),
        ("notes.txt", b"hello notes\n".to_vec()),
        ("big.txt", big_text),
    ])
    .await?;
    let client = reqwest::Client::new();

    // Explicit download requests an attachment with the file name.
    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/notes.txt?download=true"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let disposition = response.headers()["content-disposition"]
        .to_str()
        .expect("content-disposition header");
    assert!(disposition.starts_with("attachment"));
    assert!(disposition.contains("filename=\"notes.txt\""));

    // Direct navigation to active content is sandboxed.
    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/page.html"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-security-policy"], "sandbox");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");

    // Direct-link text access streams the full file instead of truncating.
    let response = client
        .get(format!(
            "{base}/api/workspaces/{workspace_id}/files/big.txt"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let body = response.bytes().await?;
    assert_eq!(
        body.len(),
        1024 * 1024 + 1000,
        "direct-link text access must not truncate"
    );

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
            worktree: None,
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

pub async fn workspace_files_distinguishes_multiple_execution_roots() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let client = reqwest::Client::new();

    let workspace_id = "agent_home:default";
    let first_root_id = "test-worktree-root-first";
    let second_root_id = "test-worktree-root-second";
    let first_root = tempdir()?;
    let second_root = tempdir()?;
    std::fs::write(first_root.path().join("same-path.txt"), "first root")?;
    std::fs::write(second_root.path().join("same-path.txt"), "second root")?;

    for (execution_root_id, filesystem_path) in [
        (first_root_id, first_root.path()),
        (second_root_id, second_root.path()),
    ] {
        host.runtime_db()
            .execution_root_entries()
            .upsert(&ExecutionRootEntry {
                execution_root_id: execution_root_id.into(),
                workspace_id: workspace_id.into(),
                filesystem_path: filesystem_path.to_path_buf(),
                root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                worktree: None,
                created_at: Utc::now(),
                removed_at: None,
            })?;
    }

    for (execution_root_id, expected_contents) in [
        (first_root_id, "first root"),
        (second_root_id, "second root"),
    ] {
        let response = client
            .get(format!(
                "{base}/api/workspaces/{workspace_id}/files/same-path.txt?root={execution_root_id}"
            ))
            .send()
            .await?;
        assert_eq!(response.status(), 200, "{}", response.text().await?);
        assert_eq!(response.text().await?, expected_contents);
    }

    server.abort();
    Ok(())
}
