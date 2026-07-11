mod support;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ModelRef},
    host::RuntimeHost,
    types::{
        AgentStatus, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, TodoItemState, WorkItemPlanStatus, WorkItemReadiness,
    },
};
use tempfile::tempdir;

fn live_prompt_continuity_model() -> Result<ModelRef> {
    let model = std::env::var("HOLON_LIVE_PROMPT_CONTINUITY_MODEL")
        .unwrap_or_else(|_| "deepseek-anthropic/deepseek-v4-pro".into());
    ModelRef::parse(&model).with_context(|| format!("invalid live continuity model ref {model:?}"))
}

fn live_runtime_config() -> Result<(AppConfig, tempfile::TempDir, tempfile::TempDir)> {
    let mut config = AppConfig::load()?;
    let data_dir = tempdir()?;
    let workspace_dir = tempdir()?;
    fs::write(
        workspace_dir.path().join("README.md"),
        "live prompt continuity fixture\n",
    )?;

    let model = live_prompt_continuity_model()?;
    config.default_agent_id = "live-prompt-continuity".into();
    config.default_model = holon::config::ModelRouteRef::from_legacy_model_ref(&model);
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.home_dir = data_dir.path().to_path_buf();
    config.data_dir = data_dir.path().to_path_buf();
    config.config_file_path = data_dir.path().join("config.json");
    config.socket_path = data_dir.path().join("run").join("holon.sock");
    config.workspace_dir = workspace_dir.path().to_path_buf();
    config.context_window_messages = 4;
    config.context_window_briefs = 4;
    config.compaction_trigger_messages = 4;
    config.compaction_keep_recent_messages = 1;
    config.prompt_budget_estimated_tokens = 4096;
    config.compaction_trigger_estimated_tokens = 1;
    config.compaction_keep_recent_estimated_tokens = 1;
    config.runtime_max_output_tokens = config.runtime_max_output_tokens.max(4096);

    assert!(
        config.providers.contains_key(&model.provider),
        "configured live model provider {} is missing from provider config",
        model.provider.as_str()
    );

    Ok((config, data_dir, workspace_dir))
}

async fn wait_for_idle(runtime: &holon::runtime::RuntimeHandle) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        let state = runtime.agent_state().await?;
        if matches!(
            state.status,
            AgentStatus::AwakeIdle | AgentStatus::Asleep | AgentStatus::AwaitingTask
        ) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for live prompt continuity turn to finish");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn enqueue_operator_prompt(
    runtime: &holon::runtime::RuntimeHandle,
    agent_id: &str,
    text: impl Into<String>,
) -> Result<()> {
    runtime
        .enqueue(MessageEnvelope::new(
            agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        ))
        .await?;
    wait_for_idle(runtime).await
}

fn assert_contains_all(haystack: &str, expected: &[&str], label: &str) {
    for needle in expected {
        assert!(
            haystack.contains(needle),
            "{label} should contain {needle:?}; got:\n{haystack}"
        );
    }
}

fn assert_contains_any(haystack: &str, expected: &[&str], label: &str) {
    assert!(
        expected.iter().any(|needle| haystack.contains(needle)),
        "{label} should contain one of {expected:?}; got:\n{haystack}"
    );
}

#[tokio::test]
#[ignore = "requires configured live provider credentials and network access; set HOLON_LIVE_PROMPT_CONTINUITY_MODEL to choose the model"]
async fn live_llm_tracks_discussion_work_item_without_project_file_edits() -> Result<()> {
    let (config, _data_dir, workspace_dir) = live_runtime_config()?;
    let agent_id = config.default_agent_id.clone();
    let workspace_path = workspace_dir.path().to_path_buf();
    let host = RuntimeHost::new(config)?;
    support::attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "只读模拟测试：请围绕 prompt context 多条目讨论连续性，固定使用以下 5 个条目形成稳定清单：\
         1. 条目锚定与稳定引用；2. 跨轮状态跟踪；3. 上下文预算与条目压缩；\
         4. 持久化 vs 工作记忆边界；5. 重入与恢复协议。\
         请主动记录成 WorkItem/todo/plan 以便后续逐项讨论。不要修改 workspace/project 文件。",
    )
    .await?;
    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "讨论第 1 条：它为什么会导致用户逐项筛选时跑偏？请只讨论并记录状态，不要实现。",
    )
    .await?;
    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "讨论第 2 条：它和 recent_turns 截断有什么关系？请继续记录状态，不要实现。",
    )
    .await?;
    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "讨论第 3 条：请把这轮讨论结论写回 WorkItem 的 durable state；仍然不要修改工程文件。",
    )
    .await?;
    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "讨论第 4 条：先回忆原始 5 条清单，再讨论第 4 条，并把结论写回 WorkItem/todo/plan。",
    )
    .await?;
    enqueue_operator_prompt(
        &runtime,
        &agent_id,
        "确认一下：我还没有授权实现，只是在讨论。请说明当前应该保持什么状态，并确保 durable state 不要声称未写入的内容已经写入。",
    )
    .await?;

    let state = runtime.agent_state().await?;
    let work_items = runtime.latest_work_items_for_agent(&agent_id, 20).await?;
    let tracked = work_items
        .iter()
        .find(|item| item.objective.contains("prompt") || item.objective.contains("context"))
        .or_else(|| work_items.first())
        .context("live model should create a discussion tracking WorkItem")?;

    assert_eq!(
        tracked.plan_status,
        WorkItemPlanStatus::NeedsInput,
        "discussion-only tracking should wait for operator input instead of becoming implementation work"
    );
    assert_eq!(
        tracked.readiness(),
        WorkItemReadiness::WaitingForOperator,
        "tracking WorkItem should be waiting for operator input"
    );
    assert_eq!(
        state.current_work_item_id.as_deref(),
        Some(tracked.id.as_str()),
        "created tracking WorkItem should remain current"
    );
    assert!(
        tracked.todo_list.len() >= 5,
        "tracking WorkItem should preserve the original five-item list; got {:?}",
        tracked.todo_list
    );
    assert!(
        tracked
            .todo_list
            .iter()
            .take(4)
            .all(|item| item.state == TodoItemState::Completed),
        "first four discussed items should be durably marked completed; got {:?}",
        tracked.todo_list
    );

    let plan_artifact = tracked
        .plan_artifact
        .as_ref()
        .context("tracking WorkItem should have a plan artifact")?;
    let plan_text = fs::read_to_string(&plan_artifact.path)
        .with_context(|| format!("failed to read {}", plan_artifact.path.display()))?;
    assert_contains_all(
        &plan_text,
        &["条目锚定", "跨轮", "上下文", "持久化", "重入"],
        "WorkItem plan artifact",
    );
    assert_contains_any(
        &plan_text,
        &["第 4", "第四", "条目4", "条目 4"],
        "WorkItem plan artifact",
    );

    let workspace_entries = fs::read_dir(&workspace_path)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(
        workspace_entries,
        vec!["README.md".to_string()],
        "live discussion testcase must not modify workspace/project files"
    );

    Ok(())
}
