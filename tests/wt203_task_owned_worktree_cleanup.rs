use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{TaskStatus, TrustLevel},
};
use tempfile::tempdir;
use tokio::time::{sleep, Duration};

fn test_config() -> AppConfig {
    let home_dir = tempdir().unwrap().keep();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        home_dir: home_dir.clone(),
        data_dir: home_dir.clone(),
        socket_path: home_dir.join("run").join("holon.sock"),
        workspace_dir: tempdir().unwrap().keep(),
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: ControlAuthMode::Auto,
        config_file_path: home_dir.join("config.json"),
        stored_config: Default::default(),
        default_model: holon::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        fallback_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        disable_provider_fallback: false,
        tui_alternate_screen: holon::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        providers: holon::config::provider_registry_for_tests(
            None,
            Some("dummy"),
            home_dir.join(".codex"),
        ),
    }
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
            blocks: vec![ModelBlock::Text {
                text: "Made changes in worktree".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

struct SlowTextProvider;

#[async_trait]
impl AgentProvider for SlowTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        sleep(Duration::from_secs(2)).await;
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "Slow worktree result".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
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
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a clean task-owned worktree".into(),
            "Return without changing files".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;
    wait_for_task_status(&runtime, &task.id, TaskStatus::Completed).await?;

    assert!(!expected_worktree.exists());
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
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a manually removed worktree".into(),
            "Return after the worktree is removed externally".into(),
            TrustLevel::TrustedOperator,
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
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a mismatched worktree".into(),
            "Return after branch metadata is made stale".into(),
            TrustLevel::TrustedOperator,
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
async fn wt203_task_stop_cleans_clean_task_owned_worktree() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(SlowTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "Create a stopped worktree".into(),
            "Keep running until stopped".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let expected_worktree = wait_for_worktree(&runtime, &workspace, &task.id).await?;

    let stopped = runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;
    assert_eq!(stopped.status, TaskStatus::Cancelled);
    assert!(!expected_worktree.exists());
    assert!(git(
        &workspace,
        &["branch", "--list", &format!("task-{}", task.id)]
    )?
    .is_empty());

    let metadata = task_worktree_metadata(&runtime, &task.id)?;
    assert_eq!(metadata["cleanup_status"].as_str(), Some("cleaned"));
    assert_eq!(metadata["branch_cleanup_status"].as_str(), Some("deleted"));
    assert_eq!(metadata["auto_cleaned_up"].as_bool(), Some(true));

    Ok(())
}
