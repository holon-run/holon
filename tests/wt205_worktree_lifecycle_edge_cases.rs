use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse},
    runtime::RuntimeHandle,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{MessageKind, TaskRecord, TaskStatus, TrustLevel, WorkspaceEntry},
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
        return Err(anyhow!(
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

async fn attach_default_workspace(
    host: &RuntimeHost,
) -> Result<(RuntimeHandle, WorkspaceEntry, PathBuf)> {
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
    Ok((runtime, workspace, host.config().workspace_dir.clone()))
}

async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    for _ in 0..120 {
        if predicate().await? {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("timed out waiting for condition"))
}

async fn discover_managed_root(runtime: &RuntimeHandle) -> Result<PathBuf> {
    let probe = runtime
        .schedule_child_agent_task(
            "probe managed root".into(),
            "Create a probe worktree".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let (worktree_path, _) = wait_for_worktree(runtime, &probe).await?;
    wait_for_task_status(runtime, &probe.id, TaskStatus::Completed).await?;
    worktree_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("probe worktree has no parent directory"))
}

async fn wait_for_worktree(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
) -> Result<(PathBuf, String)> {
    let task_id = task.id.clone();
    let storage = runtime.storage().clone();
    for _ in 0..120 {
        let events = storage.read_recent_events(200)?;
        if let Some(event) = events.iter().find(|event| {
            event.kind == "worktree_created_for_task"
                && event.data["task_id"].as_str() == Some(task_id.as_str())
        }) {
            let path = event.data["worktree_path"]
                .as_str()
                .ok_or_else(|| anyhow!("missing worktree_path"))?;
            let branch = event.data["worktree_branch"]
                .as_str()
                .ok_or_else(|| anyhow!("missing worktree_branch"))?;
            return Ok((PathBuf::from(path), branch.to_string()));
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!(
        "timed out waiting for worktree creation for task {}",
        task_id
    ))
}

async fn wait_for_task_status(
    runtime: &RuntimeHandle,
    task_id: &str,
    status: TaskStatus,
) -> Result<()> {
    wait_until_async(|| async {
        let records = runtime.latest_task_records().await?;
        Ok(records
            .iter()
            .any(|record| record.id == task_id && record.status == status))
    })
    .await
}

fn task_result_worktree_metadata(
    runtime: &RuntimeHandle,
    task_id: &str,
) -> Result<serde_json::Value> {
    let messages = runtime.storage().read_recent_messages(200)?;
    messages
        .iter()
        .find(|msg| {
            matches!(msg.kind, MessageKind::TaskResult)
                && msg
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("task_id"))
                    .and_then(|id| id.as_str())
                    == Some(task_id)
        })
        .and_then(|msg| msg.metadata.as_ref())
        .and_then(|m| m.get("worktree").cloned())
        .ok_or_else(|| anyhow!("missing worktree metadata for task {task_id}"))
}

struct DelayedSuccessProvider;

#[async_trait]
impl AgentProvider for DelayedSuccessProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        sleep(Duration::from_millis(250)).await;
        let prompt = request
            .conversation
            .iter()
            .rev()
            .find_map(|message| match message {
                holon::provider::ConversationMessage::UserText(text) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("worktree attempt");
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: format!("Completed worktree attempt for: {}", prompt),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

struct DelayedFailureProvider;

#[async_trait]
impl AgentProvider for DelayedFailureProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        sleep(Duration::from_millis(250)).await;
        Err(anyhow!("simulated provider failure"))
    }
}

#[tokio::test]
async fn worktree_enter_path_collision_preserves_canonical_entry() -> Result<()> {
    let config = test_config();
    let workspace_dir = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace_dir)?;
    init_git_repo(&workspace_dir)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedSuccessProvider))?;
    let (runtime, workspace, _workspace_dir) = attach_default_workspace(&host).await?;
    let managed_root = discover_managed_root(&runtime).await?;

    let collision_branch = "candidate path collision";
    let collision_path = managed_root.join("candidate-path-collision");
    std::fs::create_dir_all(&collision_path)?;

    let err = runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::GitWorktreeRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            None,
            Some(collision_branch.to_string()),
        )
        .await
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("managed worktree path already exists"));
    assert!(
        collision_path.exists(),
        "collision path should remain untouched"
    );

    let state = runtime.agent_state().await?;
    let entry = state
        .active_workspace_entry
        .as_ref()
        .expect("canonical entry should remain active");
    assert_eq!(
        entry.projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert!(state.worktree_session.is_none());
    Ok(())
}

#[tokio::test]
async fn worktree_enter_branch_collision_does_not_leave_worktree() -> Result<()> {
    let config = test_config();
    let workspace_dir = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace_dir)?;
    init_git_repo(&workspace_dir)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedSuccessProvider))?;
    let (runtime, workspace, workspace_dir) = attach_default_workspace(&host).await?;
    let managed_root = discover_managed_root(&runtime).await?;

    git(&workspace_dir, &["branch", "existing-branch"])?;
    let expected_path = managed_root.join("existing-branch");

    let err = runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::GitWorktreeRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            None,
            Some("existing-branch".to_string()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("git worktree add failed"));
    assert!(
        !expected_path.exists(),
        "failed branch collision should not leave a worktree path behind"
    );

    let state = runtime.agent_state().await?;
    let entry = state
        .active_workspace_entry
        .as_ref()
        .expect("canonical entry should remain active");
    assert_eq!(
        entry.projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert!(state.worktree_session.is_none());
    Ok(())
}

#[tokio::test]
async fn worktree_task_cleanup_and_review_retention_stay_distinct() -> Result<()> {
    let config = test_config();
    let workspace_dir = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace_dir)?;
    init_git_repo(&workspace_dir)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedSuccessProvider))?;
    let (runtime, _workspace, _workspace_dir) = attach_default_workspace(&host).await?;

    let retained_task = runtime
        .schedule_child_agent_task(
            "retain candidate".into(),
            "Try the retained variant".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    let cleaned_task = runtime
        .schedule_child_agent_task(
            "clean candidate".into(),
            "Try the cleaned variant".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let (retained_path, _) = wait_for_worktree(&runtime, &retained_task).await?;
    let (cleaned_path, _) = wait_for_worktree(&runtime, &cleaned_task).await?;
    std::fs::write(retained_path.join("retained-change.txt"), "candidate\n")?;

    wait_for_task_status(&runtime, &retained_task.id, TaskStatus::Completed).await?;
    wait_for_task_status(&runtime, &cleaned_task.id, TaskStatus::Completed).await?;

    let retained_metadata = task_result_worktree_metadata(&runtime, &retained_task.id)?;
    let cleaned_metadata = task_result_worktree_metadata(&runtime, &cleaned_task.id)?;

    assert_eq!(
        retained_metadata["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["retained-change.txt"]
    );
    assert_eq!(
        retained_metadata["retained_for_review"].as_bool(),
        Some(true)
    );
    assert_eq!(retained_metadata.get("auto_cleaned_up"), None);
    assert!(
        retained_path.exists(),
        "retained worktree should remain on disk"
    );

    assert_eq!(
        cleaned_metadata["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    assert_eq!(cleaned_metadata["auto_cleaned_up"].as_bool(), Some(true));
    assert_eq!(cleaned_metadata.get("retained_for_review"), None);
    assert!(
        !cleaned_path.exists(),
        "clean worktree should be auto-removed when no changes were made"
    );

    let summary = runtime.summarize_worktree_tasks().await?;
    assert!(summary.contains("Worktree retained for review"));
    assert!(summary.contains("Worktree auto-cleaned"));
    assert!(summary.contains("retained-change.txt"));
    assert!(summary.contains(retained_path.to_string_lossy().as_ref()));
    Ok(())
}

#[tokio::test]
async fn failed_worktree_task_keeps_metadata_and_summary_consistent() -> Result<()> {
    let config = test_config();
    let workspace_dir = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace_dir)?;
    init_git_repo(&workspace_dir)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedFailureProvider))?;
    let (runtime, _workspace, _workspace_dir) = attach_default_workspace(&host).await?;

    let task = runtime
        .schedule_child_agent_task(
            "failing candidate".into(),
            "This attempt should fail after creating a worktree".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let (worktree_path, worktree_branch) = wait_for_worktree(&runtime, &task).await?;
    std::fs::write(worktree_path.join("failed-change.txt"), "candidate\n")?;

    wait_for_task_status(&runtime, &task.id, TaskStatus::Failed).await?;

    let worktree_metadata = task_result_worktree_metadata(&runtime, &task.id)?;
    assert_eq!(
        worktree_metadata["worktree_path"].as_str(),
        Some(worktree_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        worktree_metadata["worktree_branch"].as_str(),
        Some(worktree_branch.as_str())
    );
    assert_eq!(
        worktree_metadata["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["failed-change.txt"]
    );
    assert_eq!(
        worktree_metadata["retained_for_review"].as_bool(),
        Some(true)
    );
    assert_eq!(worktree_metadata.get("auto_cleaned_up"), None);
    assert!(
        worktree_path.exists(),
        "failed worktree with changes should remain"
    );

    let summary = runtime.summarize_worktree_tasks().await?;
    assert!(summary.contains("Failed Tasks (1)"));
    assert!(summary.contains(&task.id));
    assert!(summary.contains("failing candidate"));
    assert!(summary.contains(worktree_path.to_string_lossy().as_ref()));
    assert!(summary.contains("failed-change.txt"));

    Ok(())
}
