use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ModelRef, ProviderId, ProviderTransportKind},
    prompt::PromptStability,
    provider::{
        build_provider_from_model_chain, AgentProvider, AnthropicProvider, ConversationMessage,
        ModelBlock, PromptContentBlock, ProviderPromptCache, ProviderPromptFrame,
        ProviderTurnRequest,
    },
    tool::ToolSpec,
};

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_baseline_model_limit() -> usize {
    std::env::var("HOLON_LIVE_BASELINE_MAX_MODELS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

fn live_baseline_models(config: &AppConfig) -> Vec<ModelRef> {
    config
        .provider_chain()
        .into_iter()
        .take(live_baseline_model_limit())
        .collect()
}

fn live_anthropic_model() -> String {
    std::env::var("HOLON_LIVE_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".into())
}

fn response_text(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            ModelBlock::ToolUse { .. }
            | ModelBlock::Thinking { .. }
            | ModelBlock::RedactedThinking { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn baseline_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "RecordBaselineProbe".into(),
        description: "Record a short live baseline probe value.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "result": {
                    "type": "string",
                    "description": "The probe result token."
                }
            },
            "required": ["result"]
        }),
        freeform_grammar: None,
    }
}

fn large_stable_context() -> String {
    let paragraph = "This is stable live LLM baseline context. It is intentionally repetitive so provider prompt cache thresholds are crossed while the semantic task remains harmless. The model should ignore the body except for preserving a stable request prefix.";
    let mut text = String::from("<live-llm-baseline-cache-document>\n");
    for index in 0..96 {
        text.push_str(&format!("Section {index}: {paragraph}\n"));
    }
    text.push_str("</live-llm-baseline-cache-document>");
    text
}

fn anthropic_cache_frame() -> ProviderPromptFrame {
    ProviderPromptFrame::structured(
        "Reply briefly and follow the user's exact output token requirement.",
        vec![PromptContentBlock {
            text: "Live baseline Anthropic prompt-cache system block.".into(),
            stability: PromptStability::Stable,
            cache_breakpoint: true,
        }],
        vec![PromptContentBlock {
            text: large_stable_context(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: true,
        }],
        Some(ProviderPromptCache {
            agent_id: "live-llm-baseline-cache".into(),
            prompt_cache_key: "live-llm-baseline-cache".into(),
            context_fingerprint: "live-llm-baseline-cache".into(),
            compression_epoch: 0,
        }),
    )
}

#[tokio::test]
#[ignore = "requires configured live provider credentials and network access"]
async fn live_llm_baseline_configured_chain_smoke() -> Result<()> {
    let config = live_config()?;
    let model_chain = live_baseline_models(&config);
    assert!(
        !model_chain.is_empty(),
        "live baseline needs at least one configured provider model"
    );
    let model_refs = model_chain
        .iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();
    for model_ref in &model_chain {
        let single_model_ref = model_ref.as_string();
        let provider = build_provider_from_model_chain(&config, std::slice::from_ref(model_ref))
            .with_context(|| {
                format!("failed to build live baseline provider for {single_model_ref}")
            })?;

        let output = provider
            .complete_turn(ProviderTurnRequest::plain(
                "Reply with exactly LIVE_BASELINE_OK.",
                vec![ConversationMessage::UserText(
                    "Return the requested token now.".into(),
                )],
                vec![],
            ))
            .await
            .with_context(|| {
                format!("live baseline smoke request failed for {single_model_ref}")
            })?;
        let text = response_text(&output.blocks);
        assert!(
            text.contains("LIVE_BASELINE_OK"),
            "live baseline smoke did not return requested sentinel token; ref={single_model_ref} selected_refs={model_refs:?} text={text:?} blocks={:?}",
            output.blocks
        );
        println!(
            "live_llm_baseline_smoke ref={single_model_ref} selected_refs={model_refs:?} input_tokens={} output_tokens={} text={text:?}",
            output.input_tokens, output.output_tokens
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires configured live provider credentials and network access"]
async fn live_llm_baseline_tool_roundtrip() -> Result<()> {
    let config = live_config()?;
    let model_chain = live_baseline_models(&config);
    assert!(
        !model_chain.is_empty(),
        "live baseline needs at least one configured provider model"
    );
    let model_refs = model_chain
        .iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();
    for model_ref in &model_chain {
        let single_model_ref = model_ref.as_string();
        let provider = build_provider_from_model_chain(&config, std::slice::from_ref(model_ref))
            .with_context(|| {
                format!("failed to build live baseline provider for {single_model_ref}")
            })?;

        let output = provider
            .complete_turn(ProviderTurnRequest::plain(
                "When the user asks for a baseline probe, call the RecordBaselineProbe tool exactly once.",
                vec![ConversationMessage::UserText(
                    "Run the baseline probe with result LIVE_TOOL_OK. Do not answer in prose."
                        .into(),
                )],
                vec![baseline_tool_spec()],
            ))
            .await
            .with_context(|| format!("live baseline tool request failed for {single_model_ref}"))?;
        let tool_use = output.blocks.iter().find_map(|block| match block {
            ModelBlock::ToolUse { name, input, .. } if name == "RecordBaselineProbe" => Some(input),
            _ => None,
        });
        let tool_use = tool_use.with_context(|| {
            format!(
                "live baseline expected RecordBaselineProbe tool call for {single_model_ref}; selected_refs={model_refs:?} blocks={:?}",
                output.blocks
            )
        })?;
        assert_eq!(tool_use["result"], serde_json::json!("LIVE_TOOL_OK"));
        println!(
            "live_llm_baseline_tool ref={single_model_ref} selected_refs={model_refs:?} input_tokens={} output_tokens={} tool_input={tool_use}",
            output.input_tokens, output.output_tokens
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_AUTH_TOKEN, Anthropic prompt-cache support, and network access"]
async fn live_llm_baseline_anthropic_prompt_cache_hit() -> Result<()> {
    let mut config = live_config()?;
    let runtime_max_output_tokens = config.runtime_max_output_tokens;
    let provider_config = config
        .providers
        .get_mut(&ProviderId::anthropic())
        .context("missing anthropic provider config")?;
    assert_eq!(
        provider_config.transport,
        ProviderTransportKind::AnthropicMessages,
        "anthropic provider must use Anthropic Messages transport"
    );
    let trace_home_dir = tempfile::tempdir()?;
    let provider = AnthropicProvider::from_runtime_config(
        provider_config,
        &live_anthropic_model(),
        runtime_max_output_tokens,
        trace_home_dir.path(),
    )?;
    let frame = anthropic_cache_frame();

    let first = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: frame.clone(),
            conversation: vec![ConversationMessage::UserText(
                "Reply with exactly CACHE_BASELINE_ONE.".into(),
            )],
            tools: vec![],
            native_web_search: None,
        })
        .await
        .context("first Anthropic live cache baseline request failed")?;
    let second = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: frame,
            conversation: vec![ConversationMessage::UserText(
                "Reply with exactly CACHE_BASELINE_TWO.".into(),
            )],
            tools: vec![],
            native_web_search: None,
        })
        .await
        .context("second Anthropic live cache baseline request failed")?;

    let first_cache = first.cache_usage.clone();
    let second_cache = second
        .cache_usage
        .clone()
        .context("second Anthropic live cache baseline response did not include cache usage")?;
    println!(
        "live_llm_baseline_anthropic_cache first={first_cache:?} second={second_cache:?} first_tokens=({},{}) second_tokens=({},{})",
        first.input_tokens, first.output_tokens, second.input_tokens, second.output_tokens
    );
    assert!(
        second_cache.read_input_tokens > 0,
        "expected second Anthropic baseline request to report cache read tokens; first={first_cache:?} second={second_cache:?}"
    );
    Ok(())
}
