use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ProviderId, ProviderTransportKind},
    prompt::PromptStability,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, OpenAiCodexProvider,
        OpenAiProvider, PromptContentBlock, ProviderNativeWebSearchKind,
        ProviderNativeWebSearchRequest, ProviderPromptCache, ProviderPromptFrame,
        ProviderTurnRequest, ToolResultBlock,
    },
    tool::ToolSpec,
};

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn provider_model_env(provider: &str, default_model: &str) -> String {
    let env_name = format!(
        "HOLON_LIVE_{}_MODEL",
        provider.replace('-', "_").to_ascii_uppercase()
    );
    std::env::var(env_name).unwrap_or_else(|_| default_model.into())
}

fn configured_provider_model(
    provider_candidates: &[&str],
    default_provider: &str,
    default_model: &str,
) -> Result<(String, String)> {
    let config = live_config()?;
    for model_ref in config.provider_chain() {
        let provider = model_ref.provider.as_str();
        if provider_candidates.contains(&provider) {
            return Ok((provider.to_string(), model_ref.model));
        }
    }
    Ok((default_provider.into(), default_model.into()))
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
    assert_eq!(
        provider_config.transport,
        ProviderTransportKind::AnthropicMessages,
        "{provider_id_text} is not configured with Anthropic Messages transport"
    );
    provider_config.context_management.enabled = true;
    provider_config.context_management.trigger_input_tokens = 1;
    provider_config.context_management.keep_recent_tool_uses = 1;

    let trace_home_dir = tempfile::tempdir()?;
    let provider = AnthropicProvider::from_runtime_config(
        provider_config,
        model,
        runtime_max_output_tokens,
        trace_home_dir.path(),
        true,
    )?;

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
    let context_blocks = vec![PromptContentBlock {
        text: "Runtime context block: the probe result must be preserved in the continuation."
            .into(),
        stability: PromptStability::AgentScoped,
        cache_breakpoint: true,
    }];
    let prompt_frame = ProviderPromptFrame::structured(
        "Reply to the user briefly after reading the tool result.",
        vec![PromptContentBlock {
            text: "Live Anthropic-compatible context management smoke test.".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: false,
        }],
        context_blocks.clone(),
        Some(ProviderPromptCache {
            agent_id: format!("live-{provider_id_text}-context-management"),
            prompt_cache_key: format!("live-{provider_id_text}-context-management"),
            context_fingerprint: format!("live-{provider_id_text}-context-management"),
            compression_epoch: 0,
        }),
    );

    let initial_user_text = "Call ProbeTool once with reason context-management-smoke. Do not answer directly before the tool result.".to_string();
    let first_output = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: None,
            prompt_frame: prompt_frame.clone(),
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks.clone()),
                ConversationMessage::UserText(initial_user_text.clone()),
            ],
            tools: vec![tool.clone()],
            native_web_search: None,
            response_format: None,
        })
        .await?;
    let tool_use_id = first_output
        .blocks
        .iter()
        .find_map(|block| match block {
            ModelBlock::ToolUse { id, name, .. } if name == "ProbeTool" => Some(id.clone()),
            _ => None,
        })
        .with_context(|| {
            format!(
                "{provider_id_text}/{model} did not request ProbeTool; blocks={:?}",
                first_output.blocks
            )
        })?;

    let output = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: None,
            prompt_frame,
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks),
                ConversationMessage::UserText(initial_user_text),
                ConversationMessage::AssistantBlocks(first_output.blocks),
                ConversationMessage::UserToolResults(vec![ToolResultBlock {
                    tool_use_id,
                    content: "probe_result=OK".into(),
                    is_error: false,
                    error: None,
                }]),
            ],
            tools: vec![tool],
            native_web_search: None,
            response_format: None,
        })
        .await?;

    let response_text = output
        .blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            ModelBlock::ToolUse { .. } => None,
            ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        output
            .blocks
            .iter()
            .all(|block| {
                matches!(
                    block,
                    ModelBlock::Text { .. }
                        | ModelBlock::Thinking { .. }
                        | ModelBlock::RedactedThinking { .. }
                )
            }),
        "{provider_id_text}/{model} emitted another tool call instead of completing the continuation"
    );
    assert!(
        response_text.contains("OK"),
        "{provider_id_text}/{model} did not answer from the tool result; got {response_text:?}"
    );
    Ok(())
}

async fn provider_builtin_web_search_reports_backend(
    provider_id: &str,
    model: &str,
    expected_backend: &str,
) -> Result<()> {
    let mut config = live_config()?;
    let provider_id = ProviderId::parse(provider_id)?;
    let provider_id_text = provider_id.as_str().to_string();
    let runtime_max_output_tokens = config.runtime_max_output_tokens;
    let provider_config = config
        .providers
        .get_mut(&provider_id)
        .with_context(|| format!("missing {provider_id_text} provider config"))?;
    assert_eq!(
        provider_config.transport,
        ProviderTransportKind::AnthropicMessages,
        "{provider_id_text} is not configured with Anthropic Messages transport"
    );

    let trace_home_dir = tempfile::tempdir()?;
    let provider = AnthropicProvider::from_runtime_config(
        provider_config,
        model,
        runtime_max_output_tokens,
        trace_home_dir.path(),
        true,
    )?;
    let capability = provider.builtin_web_search().with_context(|| {
        format!("{provider_id_text}/{model} did not declare builtin web search")
    })?;
    assert_eq!(capability.backend_kind, expected_backend);

    let output = provider
        .complete_turn(ProviderTurnRequest {
            continuation_scope_id: None,
            prompt_frame: ProviderPromptFrame::plain(
                "Use web search if needed. Reply in one short sentence.",
            ),
            conversation: vec![ConversationMessage::UserText(
                "Search the web and name today's date.".into(),
            )],
            tools: vec![],
            native_web_search: Some(ProviderNativeWebSearchRequest {
                kind: capability.kind,
                provider_id: format!("{provider_id_text}-native"),
                provider_model_ref: capability.provider_model_ref,
                advertised_tool_type: capability.advertised_tool_type,
                backend_kind: capability.backend_kind,
                max_results: Some(3),
            }),
            response_format: None,
        })
        .await?;
    let diagnostics = output
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.native_web_search.as_ref())
        .expect("native web search diagnostics should be recorded");
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.backend_kind, expected_backend);
    assert_eq!(diagnostics.advertised_tool_type, "web_search_20250305");
    assert!(!output.blocks.is_empty());
    Ok(())
}

fn forced_native_web_search_request(
    provider_id: &str,
    model_ref: &str,
    transport: ProviderTransportKind,
) -> Option<ProviderNativeWebSearchRequest> {
    match transport {
        ProviderTransportKind::AnthropicMessages => Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::Anthropic,
            provider_id: provider_id.into(),
            provider_model_ref: model_ref.into(),
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: format!("{provider_id}_forced_web_search_probe"),
            max_results: Some(3),
        }),
        ProviderTransportKind::OpenAiResponses => Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: provider_id.into(),
            provider_model_ref: model_ref.into(),
            advertised_tool_type: "web_search_preview".into(),
            backend_kind: format!("{provider_id}_forced_web_search_probe"),
            max_results: Some(3),
        }),
        ProviderTransportKind::OpenAiCodexResponses => Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: provider_id.into(),
            provider_model_ref: model_ref.into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: format!("{provider_id}_forced_web_search_probe"),
            max_results: Some(3),
        }),
        ProviderTransportKind::OpenAiChatCompletions
        | ProviderTransportKind::GeminiGenerateContent => None,
    }
}

#[tokio::test]
#[ignore = "requires configured provider credentials and network access"]
async fn live_configured_model_chain_builtin_web_search_support() -> Result<()> {
    let config = live_config()?;
    let provider_chain = config.provider_chain();
    let mut tested = Vec::new();
    let mut declared_failures = Vec::new();

    for model_ref in provider_chain {
        let provider_id = model_ref.provider.as_str().to_string();
        let Some(provider_config) = config.providers.get(&model_ref.provider) else {
            println!("SKIP {}: provider is not configured", model_ref.as_string());
            continue;
        };

        let trace_home_dir = tempfile::tempdir()?;
        let provider: Box<dyn AgentProvider> = match provider_config.transport {
            ProviderTransportKind::AnthropicMessages => {
                Box::new(AnthropicProvider::from_runtime_config(
                    provider_config,
                    &model_ref.model,
                    config.runtime_max_output_tokens,
                    trace_home_dir.path(),
                    true,
                )?)
            }
            ProviderTransportKind::OpenAiResponses => {
                Box::new(OpenAiProvider::from_runtime_config(
                    provider_config,
                    &model_ref.model,
                    config.runtime_max_output_tokens,
                    trace_home_dir.path(),
                )?)
            }
            ProviderTransportKind::OpenAiCodexResponses => {
                Box::new(OpenAiCodexProvider::from_runtime_config(
                    provider_config,
                    &model_ref.model,
                    config.runtime_max_output_tokens,
                    trace_home_dir.path(),
                    true,
                )?)
            }
            ProviderTransportKind::OpenAiChatCompletions
            | ProviderTransportKind::GeminiGenerateContent => {
                println!(
                    "SKIP {}: transport {:?} has no builtin web search lowering",
                    model_ref.as_string(),
                    provider_config.transport
                );
                continue;
            }
        };

        let declared = provider.builtin_web_search();
        let request = declared
            .as_ref()
            .map(|capability| ProviderNativeWebSearchRequest {
                kind: capability.kind,
                provider_id: provider_id.clone(),
                provider_model_ref: capability.provider_model_ref.clone(),
                advertised_tool_type: capability.advertised_tool_type.clone(),
                backend_kind: capability.backend_kind.clone(),
                max_results: Some(3),
            })
            .or_else(|| {
                forced_native_web_search_request(
                    &provider_id,
                    &model_ref.as_string(),
                    provider_config.transport,
                )
            });
        let Some(request) = request else {
            println!(
                "SKIP {}: transport {:?} has no builtin web search probe shape",
                model_ref.as_string(),
                provider_config.transport
            );
            continue;
        };
        let source = if declared.is_some() {
            "declared"
        } else {
            "forced"
        };
        let backend_kind = request.backend_kind.clone();

        match provider.probe_builtin_web_search(request).await {
            Ok(()) => {
                println!(
                    "PASS {} builtin_web_search source={} backend={}",
                    model_ref.as_string(),
                    source,
                    backend_kind
                );
                tested.push(model_ref.as_string());
            }
            Err(error) => {
                println!(
                    "FAIL {} builtin_web_search source={}: {error}",
                    model_ref.as_string(),
                    source
                );
                if declared.is_some() {
                    declared_failures.push(format!("{}: {error}", model_ref.as_string()));
                }
            }
        }
    }

    assert!(
        !tested.is_empty(),
        "configured provider chain did not include any builtin web search providers"
    );
    assert!(
        declared_failures.is_empty(),
        "declared builtin web search failures:\n{}",
        declared_failures.join("\n")
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires a configured DeepSeek credential and network access"]
async fn live_deepseek_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "deepseek",
        &provider_model_env("deepseek", "deepseek-v4-flash"),
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

#[tokio::test]
#[ignore = "requires XIAOMI_API_KEY and network access"]
async fn live_xiaomi_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "xiaomi-anthropic",
        &provider_model_env("xiaomi-anthropic", "mimo-v2-pro"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires XIAOMI_TOKEN_PLAN_API_KEY and network access"]
async fn live_xiaomi_token_plan_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "xiaomi-token-plan-anthropic",
        &provider_model_env("xiaomi-token-plan-anthropic", "mimo-v2-pro"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires ZAI_API_KEY and network access"]
async fn live_zai_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "zai-anthropic",
        &provider_model_env("zai-anthropic", "glm-4.7"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires ZAI_API_KEY and network access"]
async fn live_zai_anthropic_builtin_web_search_reports_prime_backend() -> Result<()> {
    provider_builtin_web_search_reports_backend(
        "zai-anthropic",
        &provider_model_env("zai-anthropic", "glm-4.7"),
        "zai_web_search_prime",
    )
    .await
}

#[tokio::test]
#[ignore = "requires BIGMODEL_API_KEY and network access"]
async fn live_bigmodel_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management(
        "bigmodel-anthropic",
        &provider_model_env("bigmodel-anthropic", "glm-4.7"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires BIGMODEL_API_KEY and network access"]
async fn live_bigmodel_anthropic_builtin_web_search_reports_backend() -> Result<()> {
    let (provider, model) = configured_provider_model(
        &["bigmodel", "bigmodel-anthropic"],
        "bigmodel-anthropic",
        "glm-4.7",
    )?;
    provider_builtin_web_search_reports_backend(&provider, &model, "bigmodel_web_search").await
}

#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and network access"]
async fn live_minimax_anthropic_accepts_context_management() -> Result<()> {
    provider_accepts_context_management("minimax", &provider_model_env("minimax", "MiniMax-M2.7"))
        .await
}
