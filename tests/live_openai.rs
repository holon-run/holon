use anyhow::Result;
use holon::{
    config::AppConfig,
    provider::{AgentProvider, ConversationMessage, OpenAiProvider, ProviderTurnRequest},
};

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_openai_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.4".into())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_openai_provider_returns_real_response() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiProvider::from_config(&config, &live_openai_model())?;
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
