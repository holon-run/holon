mod support;

use anyhow::Result;
use chrono::Utc;
use holon::{system::WorkspaceProjectionKind, types::ExecutionRootEntry};

#[tokio::test]
async fn workspace_files_rejects_conflicting_root_selectors() -> Result<()> {
    let (_host, base, server) = support::spawn_server().await?;
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/workspaces/agent_home:default/files?root=first&execution_root_id=second"
        ))
        .send()
        .await?;

    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await?;
    assert_eq!(
        body["error"],
        "root and execution_root_id selectors conflict"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn file_references_resolve_complete_locations() -> Result<()> {
    let (host, base, server) = support::spawn_server().await?;
    let client = reqwest::Client::new();
    let workspace_id = "agent_home:default";
    let execution_root_id = "test-resolve-root";
    let execution_root = support::tempdir()?;
    std::fs::create_dir_all(execution_root.path().join("docs"))?;
    std::fs::create_dir_all(execution_root.path().join("images"))?;
    std::fs::write(execution_root.path().join("docs/base.md"), "base")?;
    std::fs::write(execution_root.path().join("images/image one.png"), "image")?;
    std::fs::write(
        execution_root.path().join("images/literal%25.txt"),
        "literal",
    )?;
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

    let absolute_path = execution_root.path().join("images/image one.png");
    let response = client
        .post(format!("{base}/api/file-references/resolve"))
        .json(&serde_json::json!({
            "references": [
                {
                    "type": "absolute_path",
                    "absolute_path": absolute_path,
                },
                {
                    "type": "workspace_uri",
                    "workspace_uri": format!(
                        "workspace://{workspace_id}/images/image%20one.png?root={execution_root_id}"
                    ),
                },
                {
                    "type": "relative_path",
                    "relative_path": "../images/literal%25.txt",
                    "base_file": {
                        "workspace_id": workspace_id,
                        "execution_root_id": execution_root_id,
                        "path": "docs/base.md",
                        "absolute_path": execution_root.path().join("docs/base.md"),
                        "kind": "file",
                        "root_kind": "git_worktree_root",
                    },
                },
                {
                    "type": "workspace_uri",
                    "workspace_uri": "workspace://missing/file.txt",
                },
                {
                    "type": "future_reference",
                    "value": "not-supported",
                },
                {
                    "type": "relative_path",
                    "relative_path": "../images/image one.png",
                    "base_file": {
                        "workspace_id": workspace_id,
                        "execution_root_id": execution_root_id,
                        "path": "docs/base.md",
                        "absolute_path": "/forged/base.md",
                        "kind": "file",
                        "root_kind": "git_worktree_root",
                    },
                },
            ],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), 200, "{}", response.text().await?);

    let body: serde_json::Value = response.json().await?;
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 6);
    for result in &results[..3] {
        assert_eq!(result["status"], "resolved");
        assert_eq!(result["location"]["workspace_id"], workspace_id);
        assert_eq!(result["location"]["execution_root_id"], execution_root_id);
        assert_eq!(result["location"]["kind"], "file");
        assert_eq!(result["location"]["root_kind"], "git_worktree_root");
        assert!(result["location"]["absolute_path"].is_string());
    }
    assert_eq!(results[0]["location"]["path"], "images/image one.png");
    assert_eq!(results[1]["location"]["path"], "images/image one.png");
    assert_eq!(results[2]["location"]["path"], "images/literal%25.txt");
    assert_eq!(results[3]["status"], "unresolved");
    assert_eq!(results[3]["reason"], "not_found");
    assert_eq!(results[4]["status"], "unresolved");
    assert_eq!(results[4]["reason"], "unsupported_reference");
    assert_eq!(results[5]["status"], "unresolved");
    assert_eq!(results[5]["reason"], "invalid_reference");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn file_references_reject_removed_matching_root() -> Result<()> {
    let (host, base, server) = support::spawn_server().await?;
    let client = reqwest::Client::new();
    let workspace_id = "agent_home:default";
    let execution_root_id = "test-removed-root";
    let canonical_root = host
        .workspace_entries()?
        .into_iter()
        .find(|entry| entry.workspace_id == workspace_id)
        .expect("agent home workspace")
        .workspace_anchor;
    let removed_root = canonical_root.join("removed-nested-root");
    std::fs::create_dir_all(&removed_root)?;
    let removed_file = removed_root.join("removed.txt");
    std::fs::write(&removed_file, "removed")?;
    host.runtime_db()
        .execution_root_entries()
        .upsert(&ExecutionRootEntry {
            execution_root_id: execution_root_id.into(),
            workspace_id: workspace_id.into(),
            filesystem_path: removed_root.clone(),
            root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: Some(Utc::now()),
        })?;

    let response = client
        .post(format!("{base}/api/file-references/resolve"))
        .json(&serde_json::json!({
            "references": [
                {
                    "type": "absolute_path",
                    "absolute_path": removed_file,
                },
                {
                    "type": "workspace_uri",
                    "workspace_uri": format!(
                        "workspace://{workspace_id}/removed.txt?root={execution_root_id}"
                    ),
                },
            ],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await?;
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results[0]["status"], "unresolved");
    assert_eq!(results[0]["reason"], "root_removed");
    assert_eq!(results[1]["status"], "unresolved");
    assert_eq!(results[1]["reason"], "root_removed");

    std::fs::remove_dir_all(removed_root)?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn file_references_resolve_duplicate_anchor_prefers_live_workspace() -> Result<()> {
    let (host, base, server) = support::spawn_server().await?;
    let client = reqwest::Client::new();
    let workspace_id = "agent_home:default";
    let anchor = host
        .workspace_entries()?
        .into_iter()
        .find(|entry| entry.workspace_id == workspace_id)
        .expect("agent home workspace")
        .workspace_anchor;
    let file = anchor.join("notes/duplicate-anchor.txt");
    std::fs::create_dir_all(anchor.join("notes"))?;
    std::fs::write(&file, "duplicate anchor")?;

    // Legacy shared `agent_home` alias pointing at the same anchor as the
    // canonical agent-home workspace (issue #3088 systematic collision).
    host.runtime_db()
        .workspace_entries()
        .upsert(&holon::types::WorkspaceEntry::new(
            "agent_home",
            anchor.clone(),
            None,
        ))?;
    // Stale canonical-root backfill whose workspace entry no longer exists
    // (issue #3088 orphan collision).
    host.runtime_db()
        .execution_root_entries()
        .upsert(&ExecutionRootEntry {
            execution_root_id: "canonical_root:ws-orphan".into(),
            workspace_id: "ws-orphan".into(),
            filesystem_path: anchor.clone(),
            root_kind: WorkspaceProjectionKind::CanonicalRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: None,
        })?;

    let response = client
        .post(format!("{base}/api/file-references/resolve"))
        .json(&serde_json::json!({
            "references": [
                {
                    "type": "absolute_path",
                    "absolute_path": file,
                },
            ],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), 200, "{}", response.text().await?);
    let body: serde_json::Value = response.json().await?;
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results[0]["status"], "resolved");
    assert_eq!(results[0]["location"]["workspace_id"], workspace_id);
    assert_eq!(
        results[0]["location"]["execution_root_id"],
        format!("canonical_root:{workspace_id}")
    );
    assert_eq!(results[0]["location"]["path"], "notes/duplicate-anchor.txt");

    server.abort();
    Ok(())
}
