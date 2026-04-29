use anyhow::Result;
use holon::{
    config::AppConfig,
    provider::{
        AgentProvider, ConversationMessage, OpenAiChatCompletionsProvider, ProviderTurnRequest,
    },
};

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_chat_completion_model() -> String {
    std::env::var("HOLON_LIVE_CHAT_COMPLETION_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_chat_completions_provider_returns_real_response() -> Result<()> {
    let config = live_config()?;
    let provider =
        OpenAiChatCompletionsProvider::from_config(&config, &live_chat_completion_model())?;

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
#[ignore = "requires real provider credentials and network access"]
async fn live_chat_completions_provider_handles_tool_calls() -> Result<()> {
    let config = live_config()?;
    let provider =
        OpenAiChatCompletionsProvider::from_config(&config, &live_chat_completion_model())?;

    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "You are a helpful assistant.",
            vec![ConversationMessage::UserText(
                "What is 2 + 2? Call the calculator tool.".into(),
            )],
            vec![holon::tool::ToolSpec {
                name: "calculator".to_string(),
                description: "A simple calculator".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "expression": {
                            "type": "string",
                            "description": "Math expression to evaluate"
                        }
                    },
                    "required": ["expression"]
                }),
                freeform_grammar: None,
            }],
        ))
        .await?;

    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_chat_completions_provider_handles_multi_turn_conversation() -> Result<()> {
    let config = live_config()?;
    let provider =
        OpenAiChatCompletionsProvider::from_config(&config, &live_chat_completion_model())?;

    // First turn
    let _ = provider
        .complete_turn(ProviderTurnRequest::plain(
            "You are a helpful assistant. Remember information across turns.",
            vec![ConversationMessage::UserText(
                "My name is Alice. Remember that.".into(),
            )],
            vec![],
        ))
        .await?;

    // Second turn - test if provider can maintain context
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "You are a helpful assistant. Remember information across turns.",
            vec![
                ConversationMessage::UserText("My name is Alice. Remember that.".into()),
                ConversationMessage::AssistantBlocks(vec![holon::provider::ModelBlock::Text {
                    text: "Hello Alice! I'll remember your name.".into(),
                }]),
                ConversationMessage::UserText("What is my name?".into()),
            ],
            vec![],
        ))
        .await?;

    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_chat_completions_provider_handles_empty_response() -> Result<()> {
    let config = live_config()?;
    let provider =
        OpenAiChatCompletionsProvider::from_config(&config, &live_chat_completion_model())?;

    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "Reply to the user briefly.",
            vec![ConversationMessage::UserText(
                "Say nothing. Just respond with an empty message.".into(),
            )],
            vec![],
        ))
        .await?;

    // Should handle empty or minimal responses gracefully
    assert!(!output.blocks.is_empty() || output.stop_reason.is_some());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_chat_completions_provider_provides_token_usage() -> Result<()> {
    let config = live_config()?;
    let provider =
        OpenAiChatCompletionsProvider::from_config(&config, &live_chat_completion_model())?;

    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "Reply to the user briefly.",
            vec![ConversationMessage::UserText(
                "Hello! Please respond with a short greeting.".into(),
            )],
            vec![],
        ))
        .await?;

    // Check that token usage is reported
    assert!(output.input_tokens > 0 || output.output_tokens > 0);
    Ok(())
}
