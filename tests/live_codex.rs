use anyhow::Result;
use holon::{
    config::AppConfig,
    prompt::PromptStability,
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, OpenAiCodexProvider, PromptContentBlock,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest,
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

fn live_append_match_prompt_frame() -> ProviderPromptFrame {
    ProviderPromptFrame::structured(
        "Reply briefly and follow the user's exact formatting requirements.",
        vec![PromptContentBlock {
            text: "stable live append-match probe system block".into(),
            stability: PromptStability::Stable,
            cache_breakpoint: true,
        }],
        vec![PromptContentBlock {
            text: "agent-scoped live append-match probe context".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: true,
        }],
        Some(ProviderPromptCache {
            agent_id: "live-openai-codex-append-match".into(),
            prompt_cache_key: "live-openai-codex-append-match".into(),
            working_memory_revision: 1,
            compression_epoch: 0,
        }),
    )
}

fn text_response(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
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
async fn live_openai_codex_replays_provider_window_after_append_match() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_model())?;
    let frame = live_append_match_prompt_frame();
    let first = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: frame.clone(),
            conversation: vec![ConversationMessage::UserText(
                "Reply with exactly READY.".into(),
            )],
            tools: vec![],
        })
        .await?;
    let first_text = text_response(&first.blocks);
    assert!(
        !first_text.trim().is_empty(),
        "expected text output from first live Codex response"
    );

    let second = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: frame,
            conversation: vec![
                ConversationMessage::UserText("Reply with exactly READY.".into()),
                ConversationMessage::AssistantBlocks(vec![ModelBlock::Text { text: first_text }]),
                ConversationMessage::UserText("Reply with exactly DONE.".into()),
            ],
            tools: vec![],
        })
        .await?;

    let diagnostics = second
        .request_diagnostics
        .as_ref()
        .expect("request diagnostics");
    assert_eq!(
        diagnostics.request_lowering_mode.as_str(),
        "provider_window_replay"
    );
    let incremental = diagnostics
        .incremental_continuation
        .as_ref()
        .expect("incremental continuation diagnostics");
    assert_eq!(incremental.status, "hit");
    assert_eq!(incremental.fallback_reason, None);
    assert_eq!(incremental.incremental_input_items, Some(1));
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
