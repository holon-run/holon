use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ProviderId},
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, OpenAiProvider,
        ProviderNativeWebSearchRequest, ProviderPromptCache, ProviderPromptFrame,
        ProviderTurnRequest,
    },
};

fn live_xai_model() -> String {
    std::env::var("HOLON_LIVE_XAI_MODEL").unwrap_or_else(|_| "grok-4-1-fast".into())
}

fn live_xai_provider(config: &AppConfig) -> Result<OpenAiProvider> {
    let provider_id = ProviderId::parse("xai")?;
    let provider_config = config
        .providers
        .get(&provider_id)
        .context("xai provider is missing from config")?;
    OpenAiProvider::from_runtime_config(
        provider_config,
        &live_xai_model(),
        config.runtime_max_output_tokens,
        &config.home_dir,
    )
}

fn continuation_prompt_frame() -> ProviderPromptFrame {
    ProviderPromptFrame {
        system_prompt: "Follow the user's exact reply format.".into(),
        system_blocks: Vec::new(),
        context_blocks: Vec::new(),
        cache: Some(ProviderPromptCache {
            agent_id: "live-xai-continuation".into(),
            prompt_cache_key: "live-xai-continuation".into(),
            context_fingerprint: "live-xai-continuation".into(),
            compression_epoch: 0,
        }),
    }
}

fn response_text(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn response_token(blocks: &[ModelBlock]) -> String {
    response_text(blocks)
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation())
        .to_owned()
}

#[tokio::test]
#[ignore = "requires configured xAI credentials and network access"]
async fn live_xai_responses_uses_incremental_continuation_without_instructions() -> Result<()> {
    let config = AppConfig::load()?;
    let provider = live_xai_provider(&config)?;
    let prompt_frame = continuation_prompt_frame();
    let first_user = ConversationMessage::UserText("Reply with exactly ALPHA-2158.".into());
    let first = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: prompt_frame.clone(),
            conversation: vec![first_user.clone()],
            tools: Vec::new(),
            native_web_search: None,
            response_format: None,
        })
        .await?;
    assert_eq!(response_token(&first.blocks), "ALPHA-2158");

    let second = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame,
            conversation: vec![
                first_user,
                ConversationMessage::AssistantBlocks(first.blocks),
                ConversationMessage::UserText(
                    "Reply with exactly the token from your preceding answer.".into(),
                ),
            ],
            tools: Vec::new(),
            native_web_search: None,
            response_format: None,
        })
        .await?;

    assert_eq!(response_token(&second.blocks), "ALPHA-2158");
    assert_eq!(
        second
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation_omit_instructions")
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires configured xAI credentials, network access, and x_search support"]
async fn live_xai_builtin_x_search_reports_native_lowering() -> Result<()> {
    let config = AppConfig::load()?;
    let provider = live_xai_provider(&config)?;
    let capability = provider
        .builtin_web_search()
        .context("xai provider should declare builtin web search")?;
    assert_eq!(capability.advertised_tool_type, "web_search");
    assert_eq!(capability.backend_kind, "xai_web_search_x_search");
    let native_web_search = ProviderNativeWebSearchRequest {
        kind: capability.kind,
        provider_id: "xai".into(),
        provider_model_ref: capability.provider_model_ref,
        advertised_tool_type: capability.advertised_tool_type,
        backend_kind: capability.backend_kind,
        max_results: Some(3),
    };

    provider
        .probe_builtin_web_search(native_web_search.clone())
        .await?;
    let output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame::plain(
                "Use native web search and reply with one concise sentence.",
            ),
            conversation: vec![ConversationMessage::UserText(
                "Search the web for the official xAI homepage and state its site name.".into(),
            )],
            tools: Vec::new(),
            native_web_search: Some(native_web_search),
            response_format: None,
        })
        .await?;

    let diagnostics = output
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.native_web_search.as_ref())
        .context("native web search diagnostics should be recorded")?;
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.advertised_tool_type, "web_search");
    assert_eq!(diagnostics.backend_kind, "xai_web_search_x_search");
    assert!(!response_text(&output.blocks).trim().is_empty());
    Ok(())
}
