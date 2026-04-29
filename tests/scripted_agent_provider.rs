use std::sync::Arc;

use anyhow::Result;
use holon::{
    config::{AppConfig, ControlAuthMode, ModelRef},
    host::RuntimeHost,
    provider::{
        test_support::{ScriptedAgentProvider, ScriptedProviderStep},
        ConversationMessage,
    },
    types::{MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TrustLevel},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::time::{Duration, Instant};

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
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        prompt_budget_estimated_tokens: 4096,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: ControlAuthMode::Auto,
        config_file_path: home_dir.join("config.json"),
        stored_config: Default::default(),
        default_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
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

async fn wait_until(predicate: impl Fn() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if predicate().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

#[tokio::test]
async fn scripted_agent_provider_drives_tool_loop_and_captures_requests() -> Result<()> {
    let provider = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use("agent-get-1", "AgentGet", json!({}))
            .with_token_usage(10, 5),
        ScriptedProviderStep::text("finished after scripted tool result").with_token_usage(7, 3),
    ]);
    let captured_provider = provider.clone();
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(provider))?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "inspect agent state".into(),
            },
        ))
        .await?;

    wait_until(|| Ok(captured_provider.request_count() >= 2)).await?;
    wait_until_async(|| async {
        Ok(runtime
            .recent_briefs(10)
            .await?
            .iter()
            .any(|brief| brief.text.contains("finished after scripted tool result")))
    })
    .await?;

    let requests = captured_provider.requests();
    assert_eq!(requests.len(), 2);

    let first = &requests[0];
    let first_tool_names = first
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(first_tool_names.contains(&"AgentGet"));
    assert!(
        first.prompt_frame.system_prompt.contains("Use AgentGet"),
        "prompt should include AgentGet guidance when AgentGet is exposed"
    );

    let second = &requests[1];
    let tool_results = second
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserToolResults(results) => Some(results),
            _ => None,
        });
    let tool_results = tool_results.expect("second request should include tool results");
    let agent_get_result = tool_results
        .iter()
        .find(|result| result.tool_use_id == "agent-get-1")
        .expect("AgentGet result should be returned to the provider");
    assert!(!agent_get_result.is_error);
    assert!(
        agent_get_result.content.contains("\"agent\""),
        "AgentGet tool result should preserve the structured result envelope"
    );

    let state = runtime.agent_state().await?;
    assert_eq!(state.total_model_rounds, 2);
    assert_eq!(state.total_input_tokens, 17);
    assert_eq!(state.total_output_tokens, 8);

    Ok(())
}
