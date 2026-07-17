use anyhow::Result;
use holon::{
    config::{AppConfig, ProviderId, ProviderTransportKind},
    host::RuntimeHost,
    prompt::PromptStability,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, PromptContentBlock,
        ProviderNativeWebSearchRequest, ProviderPromptCache, ProviderPromptFrame,
        ProviderTurnRequest, ToolResultBlock,
    },
    tool::ToolRegistry,
    types::{
        AgentStatus, AuthorityClass, BriefKind, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin, Priority,
    },
};
use std::path::PathBuf;

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn provider_model_env(provider: &str, default_model: &str) -> String {
    let env_name = format!(
        "HOLON_LIVE_{}_MODEL",
        provider.replace('-', "_").to_ascii_uppercase()
    );
    std::env::var(env_name).unwrap_or_else(|_| default_model.into())
}

async fn wait_until_asleep(runtime: &holon::runtime::RuntimeHandle) -> Result<AgentStatus> {
    for _ in 0..300 {
        let status = runtime.agent_state().await?.status;
        if status == AgentStatus::Asleep {
            return Ok(status);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(runtime.agent_state().await?.status)
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_provider_returns_real_response() -> Result<()> {
    let config = live_config()?;
    let provider = AnthropicProvider::from_config(&config)?;
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "Reply to the user briefly.",
            vec![ConversationMessage::UserText(
                "Reply with exactly OK.".into(),
            )],
            vec![],
        ))
        .await?;
    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_AUTH_TOKEN and network access"]
async fn live_anthropic_builtin_web_search_reports_backend() -> Result<()> {
    let mut config = live_config()?;
    let provider_id = ProviderId::anthropic();
    let model = provider_model_env("anthropic", "claude-sonnet-4-6");
    let runtime_max_output_tokens = config.runtime_max_output_tokens;
    let provider_config = config
        .providers
        .get_mut(&provider_id)
        .expect("built-in anthropic provider should exist");
    assert_eq!(
        provider_config.transport,
        ProviderTransportKind::AnthropicMessages
    );
    let trace_home_dir = tempfile::tempdir()?;
    let provider = AnthropicProvider::from_runtime_config(
        provider_config,
        &model,
        runtime_max_output_tokens,
        trace_home_dir.path(),
        true,
    )?;
    let capability = provider
        .builtin_web_search()
        .expect("anthropic provider should declare builtin search");
    assert_eq!(capability.backend_kind, "anthropic_web_search");

    let output = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: None,
            prompt_frame: ProviderPromptFrame::plain(
                "Use web search if needed. Reply in one short sentence.",
            ),
            conversation: vec![ConversationMessage::UserText(
                "Search the web and name today's date.".into(),
            )],
            tools: vec![],
            native_web_search: Some(ProviderNativeWebSearchRequest {
                kind: capability.kind,
                provider_id: "anthropic-native".into(),
                provider_model_ref: capability.provider_model_ref,
                advertised_tool_type: capability.advertised_tool_type,
                backend_kind: capability.backend_kind,
                max_results: Some(3),
            }),
            response_format: None,
        })
        .await?;
    let diagnostics = output
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.native_web_search.as_ref())
        .expect("native web search diagnostics should be recorded");
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.backend_kind, "anthropic_web_search");
    assert_eq!(diagnostics.advertised_tool_type, "web_search_20250305");
    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_provider_accepts_tool_result_continuation_with_runtime_tools() -> Result<()> {
    let config = live_config()?;
    let provider = AnthropicProvider::from_config(&config)?;
    let tools = ToolRegistry::new(PathBuf::from(".")).tool_specs()?;
    let context_blocks = vec![PromptContentBlock {
        text: "Agent context that should be moved into the Anthropic system prefix.".into(),
        stability: PromptStability::AgentScoped,
        cache_breakpoint: true,
    }];
    let output = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: None,
            prompt_frame: ProviderPromptFrame::structured(
                "Reply to the user briefly.",
                Vec::new(),
                context_blocks.clone(),
                Some(ProviderPromptCache {
                    agent_id: "live-anthropic-continuation".into(),
                    prompt_cache_key: "live-anthropic-continuation".into(),
                    context_fingerprint: "live-anthropic-continuation".into(),
                    compression_epoch: 0,
                }),
            ),
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks),
                ConversationMessage::AssistantBlocks(vec![
                    ModelBlock::Text {
                        text: "I'll start by inspecting the GitHub issue.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "call_live_issue_probe".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "gh issue view 565 --json title,body,labels,state,url"
                        }),
                        kind: holon::provider::ModelToolCallKind::Function,
                    },
                ]),
                ConversationMessage::UserToolResults(vec![ToolResultBlock {
                    tool_use_id: "call_live_issue_probe".into(),
                    content: "Process exited with code 0\n\nstdout:\n{\"title\":\"Split provider contract tests into focused modules\",\"state\":\"OPEN\",\"labels\":[\"priority:p2\"],\"url\":\"https://github.com/holon-run/holon/issues/565\"}".into(),
                    is_error: false,
                    error: None,
                }]),
            ],
            tools,
            native_web_search: None,
            response_format: None,
        })
        .await?;
    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_runtime_wakes_sleeps_and_preserves_context() -> Result<()> {
    let mut config = live_config()?;
    config.data_dir = tempfile::tempdir()?.keep();
    let host = RuntimeHost::new(config.clone())?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            &config.default_agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Reply with exactly FIRST_OK.".into(),
            },
        ))
        .await?;
    let first_status = wait_until_asleep(&runtime).await?;
    assert_eq!(first_status, AgentStatus::Asleep);

    runtime
        .enqueue(MessageEnvelope::new(
            &config.default_agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Tell me what the previous result was in one short sentence.".into(),
            },
        ))
        .await?;
    let final_status = wait_until_asleep(&runtime).await?;

    let briefs = runtime.recent_briefs(10).await?;
    let result_briefs = briefs
        .iter()
        .filter(|brief| brief.kind == BriefKind::Result)
        .collect::<Vec<_>>();
    assert!(
        result_briefs.len() >= 2,
        "expected at least two result briefs, got {:?}",
        briefs
            .iter()
            .map(|brief| format!("{:?}: {}", brief.kind, brief.text))
            .collect::<Vec<_>>()
    );
    let second_result = result_briefs.last().unwrap().text.to_lowercase();
    assert!(
        second_result.contains("first")
            || second_result.contains("ok")
            || second_result.contains("previous result")
            || second_result.contains("exact requested phrase")
            || second_result.contains("replied with"),
        "unexpected final brief: {}\nall briefs: {:?}",
        result_briefs.last().unwrap().text,
        briefs
            .iter()
            .map(|brief| brief.text.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(final_status, AgentStatus::Asleep);
    Ok(())
}
