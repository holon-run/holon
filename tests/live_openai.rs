use anyhow::Result;
use holon::{
    config::AppConfig,
    provider::{
        AgentProvider, ContinuationScopeId, ConversationMessage, ModelBlock, ModelToolCallKind,
        OpenAiCodexProvider, OpenAiProvider, ProviderTurnRequest, ToolResultBlock,
    },
    tool::ToolSpec,
};
use serde_json::json;

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_openai_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.4".into())
}

fn live_openai_codex_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.3-codex-spark".into())
}

fn live_openai_responses_provider(config: &AppConfig) -> Result<Box<dyn AgentProvider>> {
    match OpenAiProvider::from_config(config, &live_openai_model()) {
        Ok(provider) => Ok(Box::new(provider)),
        Err(openai_error) => OpenAiCodexProvider::from_config(config, &live_openai_codex_model())
            .map(|provider| Box::new(provider) as Box<dyn AgentProvider>)
            .map_err(|codex_error| {
                anyhow::anyhow!(
                    "standard OpenAI Responses unavailable ({openai_error}); \
                     OpenAI Codex Responses unavailable ({codex_error})"
                )
            }),
    }
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

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_openai_apply_patch_function_call_kind_survives_continuation() -> Result<()> {
    let config = live_config()?;
    let provider = live_openai_responses_provider(&config)?;
    let tools = vec![ToolSpec {
        name: "ApplyPatch".into(),
        description: "Record a patch string for this transport probe. Do not apply it.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string" }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        freeform_grammar: None,
    }];
    let user = ConversationMessage::UserText(
        "Call the ApplyPatch tool exactly once with {\"patch\":\"live function probe\"}. Do not answer with plain text."
            .into(),
    );
    let scope = ContinuationScopeId::new("live-openai-apply-patch").unwrap();
    let mut first_request = ProviderTurnRequest::plain(
        "Follow tool requirements exactly.",
        vec![user.clone()],
        tools.clone(),
    );
    first_request.continuation_scope_id = Some(scope.clone());
    let first = provider.complete_turn(first_request).await?;
    let (call_id, input) = first
        .blocks
        .iter()
        .find_map(|block| match block {
            ModelBlock::ToolUse {
                id,
                name,
                input,
                kind,
            } if name == "ApplyPatch" => Some((id.clone(), input.clone(), *kind)),
            _ => None,
        })
        .map(|(id, input, kind)| {
            assert_eq!(kind, ModelToolCallKind::Function);
            (id, input)
        })
        .expect("expected ApplyPatch function call from real OpenAI response");
    assert_eq!(input["patch"], json!("live function probe"));

    let mut second_request = ProviderTurnRequest::plain(
        "Follow tool requirements exactly.",
        vec![
            user,
            ConversationMessage::AssistantBlocks(first.blocks),
            ConversationMessage::UserToolResults(vec![ToolResultBlock {
                tool_use_id: call_id,
                content: "Recorded live function probe.".into(),
                is_error: false,
                error: None,
            }]),
        ],
        tools,
    );
    second_request.continuation_scope_id = Some(scope);
    let second = provider.complete_turn(second_request).await?;
    assert!(!second.blocks.is_empty());
    Ok(())
}
