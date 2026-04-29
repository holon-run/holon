use anyhow::Result;
use holon::{
    config::AppConfig,
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, OpenAiCodexProvider, ProviderTurnRequest,
    },
    tool::ToolSpec,
};
use serde_json::json;

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_openai_codex_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.3-codex-spark".into())
}

fn probe_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "ProbeAction".into(),
        description: "Record that the transport reached a real tool-using round.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string" }
            },
            "required": ["reason"]
        }),
        freeform_grammar: None,
    }
}

#[tokio::test]
#[ignore = "requires real Codex auth state and network access"]
async fn live_openai_codex_provider_returns_real_response() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_model())?;
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
#[ignore = "requires real Codex auth state and network access"]
async fn live_openai_codex_provider_returns_tool_call_for_real_schema() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_model())?;
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "Follow tool requirements exactly. Prefer tool calls over prose when a tool is explicitly required.",
            vec![ConversationMessage::UserText(
                "Call the ProbeAction tool exactly once with {\"reason\":\"live codex tool probe\"}. Do not answer with plain text.".into(),
            )],
            vec![probe_tool_spec()],
        ))
        .await?;

    let tool_use = output.blocks.iter().find_map(|block| match block {
        ModelBlock::ToolUse { name, input, .. } if name == "ProbeAction" => Some(input),
        _ => None,
    });
    let tool_use = tool_use.expect("expected ProbeAction tool call from real codex response");
    assert_eq!(tool_use["reason"], json!("live codex tool probe"));
    Ok(())
}
