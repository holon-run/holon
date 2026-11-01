use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use holon::{
    host::RuntimeHost,
    provider::{AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{AgentRegistryStatus, AuthorityClass, TaskStatus},
};
use tokio::time::{sleep, Duration};

mod support;

use support::{TestConfig, TestConfigBuilder};

fn test_config() -> TestConfig {
    TestConfigBuilder::new().build()
}

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn init_git_repo(path: &Path) -> Result<()> {
    git(path, &["init"])?;
    git(path, &["config", "user.email", "holon@example.com"])?;
    git(path, &["config", "user.name", "Holon Test"])?;
    std::fs::write(path.join("README.md"), "holon\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-m", "init"])?;
    Ok(())
}

async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    for _ in 0..50 {
        if predicate().await? {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

async fn attach_default_workspace(host: &RuntimeHost) -> Result<()> {
    let runtime = host.default_runtime().await?;
    let workspace = host.ensure_workspace_entry(host.config().workspace_dir.clone())?;
    runtime.attach_workspace(&workspace).await?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(host.config().workspace_dir.clone()),
            None,
        )
        .await?;
    Ok(())
}

struct DelayedTextProvider;

#[async_trait]
impl AgentProvider for DelayedTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        sleep(Duration::from_millis(250)).await;
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "Made changes in worktree".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

struct PendingTextProvider;

#[async_trait]
impl AgentProvider for PendingTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        std::future::pending().await
    }
}

fn expected_worktree_path(workspace: &Path, task_id: &str) -> std::path::PathBuf {
    workspace.parent().unwrap_or(workspace).join(format!(
        ".holon-worktrees-{}/task-{}",
        workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo"),
        task_id
    ))
}

async fn wait_for_worktree(
    runtime: &holon::runtime::RuntimeHandle,
    workspace: &Path,
    task_id: &str,
) -> Result<std::path::PathBuf> {
    let expected_worktree = expected_worktree_path(workspace, task_id);
    wait_until_async(|| async {
        let events = runtime.storage().read_recent_events(50)?;
        Ok(events.iter().any(|event| {
            event.kind == "worktree_created_for_task"
                && event.data["task_id"] == task_id
                && event.data["worktree_path"].as_str() == Some(expected_worktree.to_str().unwrap())
        }))
    })
    .await?;
    Ok(expected_worktree)
}

async fn wait_for_task_status(
    runtime: &holon::runtime::RuntimeHandle,
    task_id: &str,
    status: TaskStatus,
) -> Result<()> {
    wait_until_async(|| async {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks
            .iter()
            .any(|record| record.id == task_id && record.status == status))
    })
    .await
}

fn task_worktree_metadata(
    runtime: &holon::runtime::RuntimeHandle,
    task_id: &str,
) -> Result<serde_json::Value> {
    let task = runtime
        .storage()
        .latest_task_record(task_id)?
        .ok_or_else(|| anyhow::anyhow!("missing task {task_id}"))?;
    task.detail
        .and_then(|detail| detail.get("worktree").cloned())
        .ok_or_else(|| anyhow::anyhow!("missing worktree metadata for task {task_id}"))
}

#[tokio::test]
async fn wt203_task_owned_cleanup_removes_clean_terminal_worktree_and_branch() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir().to_path_buf();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host =
        RuntimeHost::new_with_provider(config.config().clone(), Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a clean task-owned worktree".into(),
            "Return without changing files".into(),
            AuthorityClass::OperatorInstruction,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;
    wait_for_task_status(&runtime, &task.id, TaskStatus::Completed).await?;

    assert!(
        !expected_worktree.exists(),
        "cleanup metadata: {}",
        task_worktree_metadata(&runtime, &task.id)?
    );
    assert!(git(
        &workspace,
        &["branch", "--list", &format!("task-{}", task.id)]
    )?
    .is_empty());

    let metadata = task_worktree_metadata(&runtime, &task.id)?;
    assert_eq!(metadata["cleanup_status"].as_str(), Some("cleaned"));
    assert_eq!(metadata["branch_cleanup_status"].as_str(), Some("deleted"));
    assert_eq!(metadata["auto_cleaned_up"].as_bool(), Some(true));
    assert_eq!(metadata["changed_files"].as_array().unwrap().len(), 0);

    Ok(())
}

#[tokio::test]
async fn wt203_task_owned_cleanup_treats_already_removed_worktree_as_completed() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir().to_path_buf();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host =
        RuntimeHost::new_with_provider(config.config().clone(), Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a manually removed worktree".into(),
            "Return after the worktree is removed externally".into(),
            AuthorityClass::OperatorInstruction,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;

    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&expected_worktree)
        .current_dir(&workspace)
        .output()?;
    assert!(
        output.status.success(),
        "manual worktree removal should succeed"
    );

    wait_for_task_status(&runtime, &task.id, TaskStatus::Completed).await?;

    let metadata = task_worktree_metadata(&runtime, &task.id)?;
    assert_eq!(metadata["cleanup_status"].as_str(), Some("already_removed"));
    assert_eq!(
        metadata["cleanup_reason"].as_str(),
        Some("worktree_path_missing")
    );

    let events = runtime.storage().read_recent_events(100)?;
    assert!(events.iter().any(
        |event| event.kind == "task_worktree_cleanup_already_removed"
            && event.data["task_id"] == task.id
    ));

    Ok(())
}

#[tokio::test]
async fn wt203_task_owned_cleanup_records_branch_mismatch_without_blocking() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir().to_path_buf();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host =
        RuntimeHost::new_with_provider(config.config().clone(), Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a mismatched worktree".into(),
            "Return after branch metadata is made stale".into(),
            AuthorityClass::OperatorInstruction,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;
    git(
        &expected_worktree,
        &["branch", "-m", "task-branch-mismatch"],
    )?;

    wait_for_task_status(&runtime, &task.id, TaskStatus::Completed).await?;

    assert!(expected_worktree.exists());
    let metadata = task_worktree_metadata(&runtime, &task.id)?;
    assert_eq!(metadata["cleanup_status"].as_str(), Some("retained"));
    assert_eq!(metadata["cleanup_reason"].as_str(), Some("branch_mismatch"));
    assert_eq!(
        metadata["actual_branch"].as_str(),
        Some("task-branch-mismatch")
    );

    let events = runtime.storage().read_recent_events(100)?;
    assert!(events
        .iter()
        .any(|event| event.kind == "task_worktree_cleanup_retained"
            && event.data["task_id"] == task.id
            && event.data["reason"] == "branch_mismatch"));

    Ok(())
}

#[tokio::test]
async fn wt203_task_stop_removes_original_active_child_worktree_and_branch() -> Result<()> {
    assert_task_stop_child_worktree_cleanup(false).await
}

#[tokio::test]
async fn wt203_task_stop_retains_deleted_child_worktree_for_review() -> Result<()> {
    assert_task_stop_child_worktree_cleanup(true).await
}

async fn assert_task_stop_child_worktree_cleanup(deleted_before_stop: bool) -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir().to_path_buf();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host =
        RuntimeHost::new_with_provider(config.config().clone(), Arc::new(PendingTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a stopped worktree".into(),
            "Keep running until stopped".into(),
            AuthorityClass::OperatorInstruction,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;
    wait_for_task_status(&runtime, &task.id, TaskStatus::Running).await?;
    let running = runtime
        .storage()
        .latest_task_records()?
        .into_iter()
        .find(|record| record.id == task.id)
        .context("original running task")?;
    assert_eq!(running.status, TaskStatus::Running);
    let target = running.target().context("original child target")?;
    let identities = host.runtime_db().agent_identities();
    let child = identities
        .latest(&target.agent_id)?
        .context("original child identity")?;
    assert_eq!(child.status, AgentRegistryStatus::Active);
    assert_eq!(Some(child.incarnation), target.incarnation);
    let incarnation = runtime.agent_summary().await?.identity.incarnation;
    assert!(expected_worktree.exists());
    assert!(!git(
        &workspace,
        &["branch", "--list", &format!("task-{}", task.id)]
    )?
    .is_empty());
    let mut audit_cursor = runtime.storage().latest_event_seq()?.unwrap_or(0);
    if deleted_before_stop {
        // Reuse the Host AlreadyDeletedChild fixture's native identity writer.
        identities.tombstone_with_closed_supervision(&child.agent_id)?;
    }
    let before_stop = identities
        .latest(&child.agent_id)?
        .context("child identity before Stop")?;
    assert_eq!(
        before_stop.status,
        if deleted_before_stop {
            AgentRegistryStatus::Deleted
        } else {
            AgentRegistryStatus::Active
        }
    );
    assert_eq!(before_stop.incarnation, child.incarnation);
    if deleted_before_stop {
        assert!(before_stop.revision > child.revision);
    } else {
        assert_eq!(before_stop.revision, child.revision);
    }
    let stopped = runtime
        .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
        .await?;
    assert_eq!(stopped.status, TaskStatus::Cancelled);
    let association = &stopped.detail.as_ref().expect("original task detail")["worktree"];
    assert_eq!(association["original_cwd"], workspace.to_str().unwrap());
    assert_eq!(association["projection_kind"], "git_worktree_root");
    assert!(association["original_branch"].is_string());

    // The initial ACK freezes the Task; cleanup lives in the original Stop.
    let db = rusqlite::Connection::open_with_flags(
        host.runtime_db().path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let read_stop = || -> Result<serde_json::Value> {
        let mut query = db.prepare(
            "SELECT json_object(
                'effect_seq', effect_seq, 'command_nonce', command_nonce,
                'identity_incarnation', identity_incarnation,
                'task_created_at', task_created_at,
                'generation_digest', generation_digest, 'intent_digest', intent_digest,
                'ack_task_digest', ack_task_digest, 'ack_task', json(ack_task_json),
                'intent', json(intent_json), 'progress', json(progress_json))
             FROM task_stop_effects
             WHERE agent_id = ?1 AND identity_incarnation = ?2 AND task_id = ?3
             LIMIT 2",
        )?;
        let rows = query
            .query_map(rusqlite::params!["default", incarnation, task.id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            rows.len() == 1,
            "expected exactly one original Stop: {rows:?}"
        );
        Ok(serde_json::from_str(&rows[0])?)
    };
    let original = read_stop()?;
    assert_eq!(original["ack_task"], serde_json::to_value(&stopped)?);
    assert_eq!(original["identity_incarnation"], incarnation);
    assert_eq!(
        original["intent"]["created_at"],
        serde_json::to_value(task.created_at)?
    );
    assert_eq!(original["intent"]["task"]["key"]["id"], task.id);
    assert_eq!(
        original["intent"]["child"]["scope"]["agent_id"],
        child.agent_id
    );
    assert_eq!(
        original["intent"]["child"]["scope"]["identity_incarnation"],
        child.incarnation
    );
    assert_eq!(
        original["intent"]["worktree"]["path"],
        expected_worktree.to_str().unwrap()
    );
    assert_eq!(
        original["intent"]["worktree"]["branch"],
        format!("task-{}", task.id)
    );
    wait_until_async(|| async {
        let current = read_stop()?;
        Ok(["physical", "child", "worktree", "result"]
            .iter()
            .all(|phase| {
                current["progress"][*phase]
                    .as_str()
                    .is_some_and(|state| state != "pending")
            }))
    })
    .await
    .map_err(|error| error.context(format!("original Stop did not settle: {:?}", read_stop())))?;
    let settled = read_stop()?;
    for field in [
        "effect_seq",
        "command_nonce",
        "identity_incarnation",
        "task_created_at",
        "generation_digest",
        "intent_digest",
        "ack_task_digest",
        "ack_task",
        "intent",
    ] {
        assert_eq!(
            settled[field], original[field],
            "Stop binding changed: {field}"
        );
    }
    assert_eq!(settled["progress"]["child"], "retired_joined", "{settled}");
    assert_eq!(
        settled["progress"]["worktree"],
        if deleted_before_stop {
            "retained"
        } else {
            "removed"
        },
        "{settled}"
    );
    assert_eq!(settled["progress"]["result"], "completed", "{settled}");
    let after_stop = identities
        .latest(&child.agent_id)?
        .context("child identity after Stop")?;
    assert_eq!(after_stop.status, AgentRegistryStatus::Deleted);
    assert_eq!(after_stop.incarnation, before_stop.incarnation);
    if deleted_before_stop {
        assert_eq!(after_stop.revision, before_stop.revision);
    } else {
        assert!(after_stop.revision > before_stop.revision);
    }

    let audit_head = runtime.storage().latest_event_seq()?.unwrap_or(0);
    let cleanup_kind = if deleted_before_stop {
        "task_worktree_cleanup_retained"
    } else {
        "worktree_auto_cleaned_up"
    };
    let mut cleanup = Vec::new();
    loop {
        let page = host.runtime_db().audit_events().range(
            Some("default"),
            audit_head.checked_add(1),
            Some(audit_cursor),
            false,
            100,
        )?;
        let Some(last) = page.last() else {
            break;
        };
        audit_cursor = last.event_seq;
        cleanup.extend(page.into_iter().filter(|event| {
            event.kind == cleanup_kind
                && event.data["task_id"] == task.id
                && if deleted_before_stop {
                    event.data["cleanup_trigger"] == "task_stop"
                } else {
                    event.data["reason"] == "task_stop"
                }
        }));
    }
    assert_eq!(
        cleanup.len(),
        1,
        "exact original cleanup audit: {cleanup:?}"
    );
    let branch = git(
        &workspace,
        &["branch", "--list", &format!("task-{}", task.id)],
    )?;
    if deleted_before_stop {
        assert_eq!(
            cleanup[0].data["reason"],
            "original_child_teardown_unproven"
        );
        let metadata = &cleanup[0].data["worktree"];
        assert_eq!(
            metadata["worktree_path"],
            expected_worktree.to_str().unwrap()
        );
        assert_eq!(metadata["worktree_branch"], format!("task-{}", task.id));
        assert_eq!(metadata["original_cwd"], association["original_cwd"]);
        assert_eq!(metadata["original_branch"], association["original_branch"]);
        assert_eq!(metadata["projection_kind"], association["projection_kind"]);
        assert!(expected_worktree.exists());
        assert!(!branch.is_empty());
        assert_eq!(metadata["cleanup_status"].as_str(), Some("retained"));
        assert_eq!(
            metadata["cleanup_reason"].as_str(),
            Some("original_child_teardown_unproven")
        );
        assert_eq!(
            metadata["cleanup_owner_task_id"].as_str(),
            Some(task.id.as_str())
        );
        assert_eq!(metadata["retained_for_review"].as_bool(), Some(true));
        assert_eq!(metadata["auto_cleaned_up"].as_bool(), Some(false));
    } else {
        assert_eq!(
            cleanup[0].data["worktree_path"],
            expected_worktree.to_str().unwrap()
        );
        assert_eq!(
            cleanup[0].data["worktree_branch"],
            format!("task-{}", task.id)
        );
        assert_eq!(cleanup[0].data["branch_cleanup_status"], "deleted");
        assert!(!expected_worktree.exists());
        assert!(branch.is_empty());
    }

    let read_cleanup_receipts = || -> Result<Vec<(String, String)>> {
        let mut query = db.prepare(
            "SELECT command_nonce, outcome_json FROM input_mutation_receipts
             WHERE command_nonce LIKE 'stop-worktree-%' ORDER BY command_nonce",
        )?;
        let receipts = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(receipts)
    };
    let receipts = read_cleanup_receipts()?;
    assert_eq!(receipts.len(), 1, "one original worktree phase receipt");
    // A terminal retry cannot mint another Stop or repeat its FS cleanup.
    let marker = expected_worktree.join("successor-marker");
    if !deleted_before_stop {
        std::fs::create_dir(&expected_worktree)?;
        std::fs::write(&marker, "preserve")?;
    }
    assert!(runtime
        .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
        .await
        .is_err());
    assert_eq!(read_stop()?, settled);
    assert_eq!(read_cleanup_receipts()?, receipts);
    let cleanup_audits: u64 = db.query_row(
        "SELECT COUNT(*) FROM audit_events
         WHERE kind = ?1 AND json_extract(data_json, '$.data.task_id') = ?2",
        rusqlite::params![cleanup_kind, task.id],
        |row| row.get(0),
    )?;
    assert_eq!(cleanup_audits, 1, "terminal retry repeated cleanup audit");
    assert!(expected_worktree.exists());
    if !deleted_before_stop {
        assert_eq!(std::fs::read_to_string(&marker)?, "preserve");
    }
    assert_eq!(
        git(
            &workspace,
            &["branch", "--list", &format!("task-{}", task.id)]
        )?,
        branch
    );

    Ok(())
}
