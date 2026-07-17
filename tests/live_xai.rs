use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ProviderId, XSearchRuntimeConfig},
    provider::{
        AgentProvider, ContinuationScopeId, ConversationMessage, ModelBlock, OpenAiProvider,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest,
    },
    x_search::{search, XSearchRequest},
};

fn live_xai_model() -> String {
    std::env::var("HOLON_LIVE_XAI_MODEL").unwrap_or_else(|_| "grok-4.3".into())
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
    let scope = ContinuationScopeId::new("live-xai-responses").unwrap();
    let first_user = ConversationMessage::UserText("Reply with exactly ALPHA-2158.".into());
    let first = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: Some(scope.clone()),
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
            continuation_scope_id: Some(scope),
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
async fn live_xai_x_search_returns_durable_text_and_citations() -> Result<()> {
    let config = AppConfig::load()?;
    let x_search_config = XSearchRuntimeConfig::from_app_config(&config)?
        .context("xAI must be configured and XSearch enabled")?;
    let output = search(
        XSearchRequest {
            query: "Recent public posts from xAI about Grok".into(),
            allowed_x_handles: vec!["xai".into()],
            excluded_x_handles: Vec::new(),
            from_date: None,
            to_date: None,
        },
        &x_search_config,
    )
    .await?;

    assert!(!output.text.trim().is_empty());
    assert_eq!(output.provider, "xai");
    assert_eq!(output.backend, "x_search");
    assert!(!output.citations.is_empty());
    Ok(())
}
