use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ProviderId, ProviderTransportKind},
    prompt::PromptStability,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, PromptContentBlock,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest, ToolResultBlock,
    },
    tool::ToolSpec,
};

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

async fn provider_accepts_context_management(provider_id: &str, model: &str) -> Result<()> {
    let mut config = live_config()?;
    let provider_id = ProviderId::parse(provider_id)?;
    let provider_id_text = provider_id.as_str().to_string();
    let runtime_max_output_tokens = config.runtime_max_output_tokens;
    let provider_config = config
        .providers
        .get_mut(&provider_id)
        .with_context(|| format!("missing {provider_id_text} provider config"))?;
    assert_eq!(
        provider_config.transport,
        ProviderTransportKind::AnthropicMessages,
        "{provider_id_text} is not configured with Anthropic Messages transport"
    );
    provider_config.context_management.enabled = true;
    provider_config.context_management.trigger_input_tokens = 1;
    provider_config.context_management.keep_recent_tool_uses = 1;

    let provider =
        AnthropicProvider::from_runtime_config(provider_config, model, runtime_max_output_tokens)?;

    let tool = ToolSpec {
        name: "ProbeTool".into(),
        description: "Returns a short probe value.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why the probe is being called."
                }
            },
            "required": ["reason"]
        }),
        freeform_grammar: None,
    };
    let context_blocks = vec![PromptContentBlock {
        text: "Runtime context block: the probe result must be preserved in the continuation."
            .into(),
        stability: PromptStability::AgentScoped,
        cache_breakpoint: true,
    }];
    let prompt_frame = ProviderPromptFrame::structured(
        "Reply to the user briefly after reading the tool result.",
        vec![PromptContentBlock {
            text: "Live Anthropic-compatible context management smoke test.".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: false,
        }],
        context_blocks.clone(),
        Some(ProviderPromptCache {
            agent_id: format!("live-{provider_id_text}-context-management"),
            prompt_cache_key: format!("live-{provider_id_text}-context-management"),
            context_fingerprint: format!("live-{provider_id_text}-context-management"),
            working_memory_revision: 0,
            compression_epoch: 0,
        }),
    );

    let initial_user_text = "Call ProbeTool once with reason context-management-smoke. Do not answer directly before the tool result.".to_string();
    let first_output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: prompt_frame.clone(),
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks.clone()),
                ConversationMessage::UserText(initial_user_text.clone()),
            ],
            tools: vec![tool.clone()],
        })
        .await?;
    let tool_use_id = first_output
        .blocks
        .iter()
        .find_map(|block| match block {
            ModelBlock::ToolUse { id, name, .. } if name == "ProbeTool" => Some(id.clone()),
            _ => None,
        })
        .with_context(|| {
            format!(
                "{provider_id_text}/{model} did not request ProbeTool; blocks={:?}",
                first_output.blocks
            )
        })?;

    let output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame,
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks),
                ConversationMessage::UserText(initial_user_text),
                ConversationMessage::AssistantBlocks(first_output.blocks),
                ConversationMessage::UserToolResults(vec![ToolResultBlock {
                    tool_use_id,
                    content: "probe_result=OK".into(),
                    is_error: false,
                    error: None,
                }]),
            ],
            tools: vec![tool],
        })
        .await?;

    let response_text = output
        .blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            ModelBlock::ToolUse { .. } => None,
            ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        output
            .blocks
            .iter()
            .all(|block| {
                matches!(
                    block,
                    ModelBlock::Text { .. }
                        | ModelBlock::Thinking { .. }
                        | ModelBlock::RedactedThinking { .. }
                )
            }),
        "{provider_id_text}/{model} emitted another tool call instead of completing the continuation"
    );
    assert!(
        response_text.contains("OK"),
        "{provider_id_text}/{model} did not answer from the tool result; got {response_text:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and network access"]
async fn live_deepseek_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "deepseek-anthropic",
        &provider_model_env("deepseek-anthropic", "deepseek-chat"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires XIAOMI_TOKEN_PLAN_API_KEY and network access"]
async fn live_xiaomi_token_plan_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "xiaomi-token-plan",
        &provider_model_env("xiaomi-token-plan", "mimo-v2-pro"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires XIAOMI_API_KEY and network access"]
async fn live_xiaomi_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "xiaomi-anthropic",
        &provider_model_env("xiaomi-anthropic", "mimo-v2-pro"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires XIAOMI_TOKEN_PLAN_API_KEY and network access"]
async fn live_xiaomi_token_plan_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "xiaomi-token-plan-anthropic",
        &provider_model_env("xiaomi-token-plan-anthropic", "mimo-v2-pro"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires ZAI_API_KEY and network access"]
async fn live_zai_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "zai-anthropic",
        &provider_model_env("zai-anthropic", "glm-4.7"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires BIGMODEL_API_KEY and network access"]
async fn live_bigmodel_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "bigmodel-anthropic",
        &provider_model_env("bigmodel-anthropic", "glm-4.7"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and network access"]
async fn live_minimax_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management("minimax", &provider_model_env("minimax", "MiniMax-M2.7"))
        .await
}
