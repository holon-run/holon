use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{TaskRecord, TaskStatus, TrustLevel},
};
use tempfile::tempdir;
use tokio::time::{sleep, Duration, Instant};

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
    for _ in 0..60 {
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

struct DelayedProvider;

#[async_trait]
impl AgentProvider for DelayedProvider {
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
                text: format!("Completed candidate worktree attempt for: {}", prompt),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

fn expected_worktree_path(workspace: &Path, task_id: &str) -> PathBuf {
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
    task: &TaskRecord,
) -> Result<PathBuf> {
    let task_id = task.id.clone();
    let summary = task.summary.clone().unwrap_or_default();
    let storage = runtime.storage().clone();
    let deadline = Instant::now() + Duration::from_secs(15);

    loop {
        let events = storage.read_recent_events(200)?;
        if let Some(event) = events.iter().find(|event| {
            event.kind == "worktree_created_for_task"
                && event.data["task_id"].as_str() == Some(task_id.as_str())
        }) {
            let path = event.data["worktree_path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing worktree_path for task {}", summary))?;
            return Ok(PathBuf::from(path));
        }

        if Instant::now() >= deadline {
            let recent_kinds = events
                .iter()
                .rev()
                .take(5)
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow::anyhow!(
                "timed out waiting for worktree creation for task {} (recent events: {})",
                task_id,
                recent_kinds
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn wt204_parallel_worktree_workflow_demo_is_reviewable_end_to_end() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let attempts = vec![
        (
            "strategy pattern approach".to_string(),
            "Implement the strategy pattern variant and verify it.".to_string(),
            "strategy-approach.txt".to_string(),
        ),
        (
            "middleware composition approach".to_string(),
            "Implement the middleware composition variant and verify it.".to_string(),
            "middleware-approach.txt".to_string(),
        ),
        (
            "adapter interface approach".to_string(),
            "Implement the adapter interface variant and verify it.".to_string(),
            "adapter-approach.txt".to_string(),
        ),
    ];

    let mut tasks = Vec::new();
    for (summary, prompt, _) in &attempts {
        tasks.push(
            runtime
                .schedule_child_agent_task(
                    summary.clone(),
                    prompt.clone(),
                    TrustLevel::TrustedOperator,
                    holon::types::ChildAgentWorkspaceMode::Worktree,
                )
                .await?,
        );
    }

    let mut expected_files = HashMap::new();
    let mut worktree_paths = HashMap::new();
    for (task, (_, _, filename)) in tasks.iter().zip(attempts.iter()) {
        let worktree_path = wait_for_worktree(&runtime, task).await?;
        std::fs::write(
            worktree_path.join(filename),
            format!("candidate generated for {}\n", task.id),
        )?;
        expected_files.insert(task.id.clone(), filename.clone());
        worktree_paths.insert(task.id.clone(), worktree_path);
    }

    wait_until_async(|| async {
        let records = runtime.latest_task_records().await?;
        let all_completed = tasks.iter().all(|task| {
            records
                .iter()
                .any(|record| record.id == task.id && record.status == TaskStatus::Completed)
        });
        Ok(all_completed)
    })
    .await?;

    let summary = runtime.summarize_worktree_tasks().await?;
    assert!(summary.contains("Worktree Task Summary"));
    assert!(summary.contains("Total tasks: 3"));

    for task in &tasks {
        let file = expected_files.get(&task.id).unwrap();
        let worktree_path = worktree_paths.get(&task.id).unwrap();
        let expected_worktree = expected_worktree_path(&workspace, &task.id);
        assert_eq!(&expected_worktree, worktree_path);
        assert!(
            summary.contains(task.summary.as_deref().unwrap_or_default()),
            "summary should include task summary for {}",
            task.id
        );
        assert!(
            summary.contains(file),
            "summary should include changed file {} for task {}",
            file,
            task.id
        );
        assert!(
            summary.contains(worktree_path.to_string_lossy().as_ref()),
            "summary should include worktree path for {}",
            task.id
        );
    }

    let keep_task = &tasks[0];
    let discard_tasks = [&tasks[1], &tasks[2]];

    let state = runtime.agent_state().await?;
    let active_entry = state.active_workspace_entry.as_ref().expect(
        "default runtime should remain in canonical root while discarding retained worktrees",
    );
    assert_eq!(
        active_entry.projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert_eq!(active_entry.access_mode, WorkspaceAccessMode::SharedRead);

    for task in discard_tasks {
        let worktree_path = worktree_paths.get(&task.id).unwrap();
        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .current_dir(&workspace)
            .output()?;
        assert!(
            output.status.success(),
            "manual worktree removal should succeed for {}: {}",
            task.id,
            String::from_utf8_lossy(&output.stderr)
        );
        git(&workspace, &["branch", "-D", &format!("task-{}", task.id)])?;
    }

    let kept_worktree = worktree_paths.get(&keep_task.id).unwrap();
    assert!(
        kept_worktree.exists(),
        "selected worktree should remain for operator review"
    );

    for task in discard_tasks {
        let discarded = worktree_paths.get(&task.id).unwrap();
        assert!(
            !discarded.exists(),
            "discarded worktree should be removed for task {}",
            task.id
        );
        let branch_output = git(
            &workspace,
            &["branch", "--list", &format!("task-{}", task.id)],
        )?;
        assert!(
            branch_output.is_empty(),
            "discarded task branch should be removed for {}",
            task.id
        );
    }
    Ok(())
}
