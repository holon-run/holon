mod support;

use std::{fs, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use holon::{
    config::{AppConfig, ModelRouteRef},
    host::RuntimeHost,
    types::{
        AgentStatus, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, ToolExecutionStatus,
    },
};
use tempfile::tempdir;

const MODEL_ENV: &str = "HOLON_LIVE_WORKSPACE_TOOLS_MODEL";
const BRANCH: &str = "live-workspace-tools-acceptance";

fn git(path: &Path, args: &[&str]) -> Result<String> {
    support::git(path, args)
}

fn live_config() -> Result<(AppConfig, tempfile::TempDir, tempfile::TempDir)> {
    let mut config = AppConfig::load().context(
        "failed to load live Holon config; configure a provider credential before running this ignored test",
    )?;
    let data_dir = tempdir()?;
    let workspace_dir = tempdir()?;
    support::init_git_repo(workspace_dir.path())?;

    if let Ok(model) = std::env::var(MODEL_ENV) {
        config.default_model = ModelRouteRef::parse_compatible(&model)
            .with_context(|| format!("invalid {MODEL_ENV} value {model:?}"))?;
    }
    let provider = config
        .providers
        .get(&config.default_model.provider)
        .with_context(|| {
            format!(
                "live workspace-tools model provider {} is absent from config; set {MODEL_ENV} to a configured provider/model",
                config.default_model.provider.as_str()
            )
        })?;
    if !provider.has_configured_credential() {
        bail!(
            "live workspace-tools model {} has no configured credential; configure it or set {MODEL_ENV}",
            config.default_model.as_string()
        );
    }

    config.default_agent_id = "default".into();
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.home_dir = data_dir.path().to_path_buf();
    config.data_dir = data_dir.path().to_path_buf();
    config.config_file_path = data_dir.path().join("config.json");
    config.socket_path = data_dir.path().join("run").join("holon.sock");
    config.workspace_dir = workspace_dir.path().to_path_buf();
    config.runtime_max_output_tokens = config.runtime_max_output_tokens.max(4096);

    Ok((config, data_dir, workspace_dir))
}

async fn wait_for_turn_completion(
    runtime: &holon::runtime::RuntimeHandle,
    baseline_turn_index: u64,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let state = runtime.agent_state().await?;
        if state.turn_index > baseline_turn_index
            && matches!(
                state.status,
                AgentStatus::AwakeIdle | AgentStatus::Asleep | AgentStatus::AwaitingTask
            )
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for live workspace-tools acceptance turn");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
#[ignore = "requires configured live provider credentials and network access; set HOLON_LIVE_WORKSPACE_TOOLS_MODEL to choose the model"]
async fn live_llm_completes_canonical_workspace_tool_lifecycle() -> Result<()> {
    let (config, _data_dir, workspace_dir) = live_config()?;
    let canonical = workspace_dir.path().canonicalize()?;
    let base_ref = git(&canonical, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let host = RuntimeHost::new(config)?;
    support::attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let canonical_workspace_id = runtime
        .agent_state()
        .await?
        .active_workspace_entry
        .context("canonical workspace should be active after attach")?
        .workspace_id;
    let baseline_turn_index = runtime.agent_state().await?.turn_index;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: format!(
                    "This is a live acceptance test. Complete the following lifecycle yourself using \
                     the canonical workspace tools, not shell git/worktree commands:\n\
                     1. Call GetWorkspaceState.\n\
                     2. Call CreateWorktree for workspace_id {canonical_workspace_id:?}, branch \
                     {BRANCH:?}, base_ref {base_ref:?}, with activate=true.\n\
                     3. Confirm the managed worktree is active, then call SwitchWorkspace with \
                     workspace_id {canonical_workspace_id:?} to return to the canonical root.\n\
                     4. Call RemoveWorktree for the created execution_root_id, keeping the branch. \
                     The worktree is clean, so removal must succeed.\n\
                     5. Call GetWorkspaceState again and only then report completion.\n\
                     Do not modify any files, do not create a WorkItem, and do not stop before the \
                     clean removal is complete."
                ),
            },
        ))
        .await?;
    wait_for_turn_completion(&runtime, baseline_turn_index).await?;

    let executions = runtime.all_tool_executions()?;
    let expected_tools = [
        "GetWorkspaceState",
        "CreateWorktree",
        "SwitchWorkspace",
        "RemoveWorktree",
    ];
    for tool in expected_tools {
        assert!(
            executions.iter().any(|record| {
                record.tool_name == tool && record.status == ToolExecutionStatus::Success
            }),
            "live model did not successfully execute canonical tool {tool}; executions={executions:#?}"
        );
    }
    assert!(
        executions
            .iter()
            .filter(|record| record.tool_name == "GetWorkspaceState")
            .count()
            >= 2,
        "expected workspace state inspection before and after lifecycle; executions={executions:#?}"
    );

    let create = executions
        .iter()
        .find(|record| record.tool_name == "CreateWorktree")
        .context("missing CreateWorktree execution")?;
    let create_result = &create.output["envelope"]["result"];
    let execution_root_id = create_result["execution_root_id"]
        .as_str()
        .context("CreateWorktree output should contain execution_root_id")?;
    let worktree_path = create_result["worktree_path"]
        .as_str()
        .context("CreateWorktree output should contain worktree_path")?;
    let remove = executions
        .iter()
        .find(|record| record.tool_name == "RemoveWorktree")
        .context("missing RemoveWorktree execution")?;
    assert_eq!(
        remove.input["execution_root_id"].as_str(),
        Some(execution_root_id),
        "RemoveWorktree must target the worktree created in this turn"
    );

    let transcript = runtime.recent_transcript(usize::MAX).await?;
    let transcript_json = serde_json::to_string(&transcript)?;
    for record in executions
        .iter()
        .filter(|record| expected_tools.contains(&record.tool_name.as_str()))
    {
        assert!(
            transcript_json.contains(&record.id),
            "transcript should reference canonical tool execution {} ({})",
            record.id,
            record.tool_name
        );
    }

    let final_state = runtime.agent_state().await?;
    let active = final_state
        .active_workspace_entry
        .context("runtime should finish with an active canonical workspace")?;
    assert_eq!(active.workspace_id, canonical_workspace_id);
    assert_eq!(active.execution_root.canonicalize()?, canonical);
    assert!(
        final_state.worktree_session.is_none(),
        "managed worktree session should be cleared after removal"
    );
    assert!(
        !Path::new(worktree_path).exists(),
        "removed worktree path still exists: {worktree_path}"
    );
    assert_eq!(
        git(&canonical, &["status", "--porcelain"])?,
        "",
        "canonical repository should remain clean"
    );
    let listed_worktrees = git(&canonical, &["worktree", "list", "--porcelain"])?;
    assert_eq!(
        listed_worktrees
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "only the canonical worktree should remain:\n{listed_worktrees}"
    );
    assert_eq!(fs::read_to_string(canonical.join("README.md"))?, "holon\n");

    Ok(())
}
