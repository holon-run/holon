//! Test for WT-202: Summarize candidate worktree results for review

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
async fn wt202_summarize_candidate_worktree_results_for_review() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(ParallelWorktreeProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    // Create multiple worktree tasks concurrently
    let _task1 = runtime
        .schedule_child_agent_task(
            "Implement approach A in worktree".into(),
            "Implement the first approach".into(),
            holon::types::TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let _task2 = runtime
        .schedule_child_agent_task(
            "Implement approach B in worktree".into(),
            "Implement the second approach".into(),
            holon::types::TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let _task3 = runtime
        .schedule_child_agent_task(
            "Implement approach C in worktree".into(),
            "Implement the third approach".into(),
            holon::types::TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    // Wait for all tasks to complete
    wait_until_async(|| async {
        let state = runtime.agent_state().await?;
        Ok(state.active_task_ids.is_empty())
    })
    .await?;

    // Generate summary of candidate worktree results
    let summary = runtime.summarize_worktree_tasks().await?;

    // Verify summary contains all expected sections
    assert!(summary.contains("Worktree Task Summary"));
    assert!(summary.contains("Total tasks"));

    // Parse the summary to verify it contains information about each task
    let tasks = runtime.latest_task_records().await?;
    let worktree_tasks: Vec<_> = tasks
        .iter()
        .filter(|task| task.is_worktree_child_agent_task())
        .collect();

    for task in &worktree_tasks {
        // Each task should be mentioned in the summary
        assert!(
            summary.contains(&task.id),
            "summary should mention task {}",
            task.id
        );

        // Task summary should be included
        if let Some(task_summary) = &task.summary {
            assert!(
                summary.contains(task_summary),
                "summary should include task summary: {}",
                task_summary
            );
        }
    }

    // Verify the summary includes status information
    assert!(summary.contains("Status") || summary.contains("status"));

    // Verify the summary includes worktree path information
    assert!(summary.contains("Worktree path") || summary.contains("worktree_path"));

    // The summary should be clear and structured for operator review
    // It should help the operator quickly decide which worktree attempt to inspect
    println!("\n=== Generated Worktree Task Summary ===\n");
    println!("{}", summary);
    println!("\n=== End Summary ===\n");

    Ok(())
}
