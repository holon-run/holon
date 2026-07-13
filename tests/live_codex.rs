use anyhow::Result;
use holon::{
    config::AppConfig,
    prompt::PromptStability,
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, OpenAiCodexProvider, PromptContentBlock,
        ProviderGenerateImageRequest, ProviderNativeWebSearchRequest, ProviderPromptCache,
        ProviderPromptFrame, ProviderTurnRequest,
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

fn live_openai_codex_image_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_CODEX_IMAGE_MODEL").unwrap_or_else(|_| "gpt-5.5".into())
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
            context_fingerprint: "live-openai-codex-append-match".into(),
            compression_epoch: 0,
        }),
    )
}

fn assistant_text_blocks(blocks: &[ModelBlock]) -> Vec<ModelBlock> {
    blocks
        .iter()
        .filter(|block| matches!(block, ModelBlock::Text { .. }))
        .cloned()
        .collect()
}

fn assistant_response(blocks: &[ModelBlock]) -> ConversationMessage {
    ConversationMessage::AssistantBlocks(assistant_text_blocks(blocks))
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
#[ignore = "requires real Codex auth state, network access, and Codex builtin web search support"]
async fn live_openai_codex_builtin_web_search_reports_backend() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_model())?;
    let capability = provider
        .builtin_web_search()
        .expect("openai codex provider should declare builtin search");
    assert_eq!(capability.advertised_tool_type, "web_search");
    assert_eq!(capability.backend_kind, "openai_codex_web_search");
    let native_web_search = ProviderNativeWebSearchRequest {
        kind: capability.kind,
        provider_id: "openai-codex-native".into(),
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
                "Use web search if needed. Reply in one short sentence.",
            ),
            conversation: vec![ConversationMessage::UserText(
                "Search the web and name today's date.".into(),
            )],
            tools: vec![],
            native_web_search: Some(native_web_search),
            response_format: None,
        })
        .await?;
    let diagnostics = output
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.native_web_search.as_ref())
        .expect("native web search diagnostics should be recorded");
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.advertised_tool_type, "web_search");
    assert_eq!(diagnostics.backend_kind, "openai_codex_web_search");
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
            native_web_search: None,
            response_format: None,
        })
        .await?;
    let first_text = response_text(&first.blocks);
    assert!(
        !first_text.trim().is_empty(),
        "expected text output from first live Codex response"
    );

    let second = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: frame,
            conversation: vec![
                ConversationMessage::UserText("Reply with exactly READY.".into()),
                assistant_response(&first.blocks),
                ConversationMessage::UserText("Reply with exactly DONE.".into()),
            ],
            tools: vec![],
            native_web_search: None,
            response_format: None,
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

#[tokio::test]
#[ignore = "requires real Codex auth state, network access, and Codex image generation support"]
async fn live_openai_codex_generates_image_with_responses_tool() -> Result<()> {
    let config = live_config()?;
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_image_model())?;
    let output = provider
        .generate_image(ProviderGenerateImageRequest {
            prompt: "Create a simple flat icon of a blue circle on a white background.".into(),
            size: Some("1024x1024".into()),
            background: Some("opaque".into()),
            output_format: Some("png".into()),
        })
        .await?;

    assert_eq!(output.provider.as_str(), "openai-codex");
    assert_eq!(output.model, live_openai_codex_image_model());
    assert_eq!(output.images.len(), 1);
    assert_eq!(output.images[0].mime.as_deref(), Some("image/png"));
    assert!(
        !output.images[0].bytes.is_empty(),
        "expected non-empty generated image bytes"
    );
    Ok(())
}
