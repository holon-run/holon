use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ProviderId},
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
        "HOLON_LIVE_{}_ANTHROPIC_MODEL",
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
    let prompt_frame = ProviderPromptFrame::structured(
        "Reply to the user briefly after reading the tool result.",
        vec![PromptContentBlock {
            text: "Live Anthropic-compatible context management smoke test.".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: false,
        }],
        vec![],
        Some(ProviderPromptCache {
            agent_id: format!("live-{provider_id_text}-context-management"),
            prompt_cache_key: format!("live-{provider_id_text}-context-management"),
            working_memory_revision: 0,
            compression_epoch: 0,
        }),
    );

    let output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame,
            conversation: vec![
                ConversationMessage::UserText("Use the probe result and answer exactly OK.".into()),
                ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
                    id: "call_context_management_probe".into(),
                    name: "ProbeTool".into(),
                    input: serde_json::json!({ "reason": "context-management-smoke" }),
                }]),
                ConversationMessage::UserToolResults(vec![ToolResultBlock {
                    tool_use_id: "call_context_management_probe".into(),
                    content: "probe_result=OK".into(),
                    is_error: false,
                    error: None,
                }]),
            ],
            tools: vec![tool],
        })
        .await?;

    assert!(
        !output.blocks.is_empty(),
        "{provider_id_text}/{model} returned no supported content blocks"
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
