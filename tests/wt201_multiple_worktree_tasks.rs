//! Test for WT-201: One session coordinating multiple worktree tasks

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{MessageKind, TaskStatus, TrustLevel},
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
        default_tool_output_tokens: 2_000,
        max_tool_output_tokens: 10_000,
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

fn init_git_repo(path: &Path) -> Result<String> {
    git(path, &["init"])?;
    git(path, &["config", "user.email", "holon@example.com"])?;
    git(path, &["config", "user.name", "Holon Test"])?;
    std::fs::write(path.join("README.md"), "holon\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-m", "init"])?;
    git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Provider that simulates different work in each worktree task
struct ParallelWorktreeProvider {
    task_id: std::sync::atomic::AtomicUsize,
}

impl ParallelWorktreeProvider {
    fn new() -> Self {
        Self {
            task_id: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for ParallelWorktreeProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        // Simulate some async work with different delays based on task
        let id = self
            .task_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let delay = 100 + (id * 50); // Different delays for each task
        sleep(Duration::from_millis(delay as u64)).await;

        Ok(ProviderTurnResponse {
            blocks: vec![holon::provider::ModelBlock::Text {
                text: format!("Completed worktree task {}", id),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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

#[tokio::test]
async fn wt201_one_session_can_coordinate_multiple_worktree_tasks() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(ParallelWorktreeProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    // Create multiple worktree tasks concurrently
    let task1 = runtime
        .schedule_child_agent_task(
            "Implement approach A in worktree".into(),
            "Implement the first approach".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let task2 = runtime
        .schedule_child_agent_task(
            "Implement approach B in worktree".into(),
            "Implement the second approach".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let task3 = runtime
        .schedule_child_agent_task(
            "Implement approach C in worktree".into(),
            "Implement the third approach".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    // Verify all tasks were created and are tracked
    let state = runtime.agent_state().await?;
    assert_eq!(state.active_task_ids.len(), 3);
    assert!(state.active_task_ids.contains(&task1.id));
    assert!(state.active_task_ids.contains(&task2.id));
    assert!(state.active_task_ids.contains(&task3.id));

    // Wait for all tasks to complete
    wait_until_async(|| async {
        let state = runtime.agent_state().await?;
        Ok(state.active_task_ids.is_empty())
    })
    .await?;

    // Verify all tasks completed successfully
    let tasks = runtime.latest_task_records().await?;
    let completed_tasks: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed))
        .filter(|task| task.is_worktree_child_agent_task())
        .collect();

    assert!(
        completed_tasks.len() >= 3,
        "expected at least 3 completed worktree tasks, got {}",
        completed_tasks.len()
    );

    // Verify each task has its own unique worktree path
    for task in &completed_tasks {
        let messages = runtime.storage().read_recent_messages(100)?;
        let task_result = messages.iter().find(|message| {
            matches!(message.kind, MessageKind::TaskResult)
                && message
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("task_id"))
                    .and_then(|id| id.as_str())
                    == Some(&task.id[..])
        });

        assert!(
            task_result.is_some(),
            "task {} should have a result message",
            task.id
        );

        let worktree_metadata = task_result
            .and_then(|msg| msg.metadata.as_ref())
            .and_then(|m| m.get("worktree"));

        assert!(
            worktree_metadata.is_some(),
            "task {} should have worktree metadata",
            task.id
        );

        let worktree_path = worktree_metadata
            .and_then(|w| w.get("worktree_path"))
            .and_then(|p| p.as_str());

        assert!(
            worktree_path.is_some(),
            "task {} should have a worktree path",
            task.id
        );

        // Verify the worktree path contains the task_id (ensures uniqueness)
        assert!(
            worktree_path.unwrap().contains(&task.id),
            "worktree path for task {} should contain its task_id",
            task.id
        );
    }

    // Verify all worktrees were cleaned up (no changes made)
    let repo_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo");
    let managed_root = workspace
        .parent()
        .unwrap_or(workspace.as_path())
        .join(format!(".holon-worktrees-{repo_name}"));

    let worktree_dirs = std::fs::read_dir(managed_root)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();

    assert!(
        worktree_dirs.is_empty(),
        "all worktrees should be auto-cleaned up when no changes were made, found {} worktrees",
        worktree_dirs.len()
    );

    // Verify the parent session workspace is unchanged
    assert_eq!(runtime.workspace_root(), workspace);

    Ok(())
}
