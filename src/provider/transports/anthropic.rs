use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use twox_hash::XxHash64;

use crate::{
    config::{
        AnthropicCacheStrategy, AnthropicContextManagementConfig, AppConfig, ProviderId,
        ProviderRuntimeConfig,
    },
    prompt::PromptStability,
    provider::{
        AgentProvider, AnthropicPromptCacheDiagnostics, CacheBreakpointInfo, ConversationMessage,
        ModelBlock, PromptContentBlock, ProviderCacheUsage, ProviderContextManagementPolicy,
        ProviderPromptCapability, ProviderTurnRequest, ProviderTurnResponse,
    },
};

use super::build_http_client;
use crate::provider::retry::{
    classify_reqwest_transport_error, classify_status_error, invalid_response_error,
};

#[derive(Clone)]
pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    auth_token: String,
    model: String,
    max_output_tokens: u32,
    context_management: AnthropicContextManagementConfig,
}

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: Value,
    messages: Vec<ApiMessage>,
    tools: Vec<ApiTool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    betas: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<ContextManagementRequest>,
}

#[derive(Debug, Serialize)]
struct ContextManagementRequest {
    edits: Vec<ContextManagementEdit>,
}

#[derive(Debug, Serialize)]
struct ContextManagementEdit {
    #[serde(rename = "type")]
    kind: &'static str,
    trigger: ContextManagementThreshold,
    keep: ContextManagementThreshold,
    exclude_tools: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clear_at_least: Option<ContextManagementThreshold>,
}

#[derive(Debug, Serialize)]
struct ContextManagementThreshold {
    #[serde(rename = "type")]
    kind: &'static str,
    value: u32,
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Value,
}

#[derive(Debug, Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ApiResponseBlock>,
    stop_reason: Option<String>,
    usage: Option<ApiUsage>,
    context_management: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ApiResponseBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl AnthropicProvider {
    pub fn from_config(config: &AppConfig) -> Result<Self> {
        Self::from_config_with_model(config, "claude-sonnet-4-6")
    }

    pub fn from_config_with_model(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::anthropic())
            .ok_or_else(|| anyhow!("missing anthropic provider config"))?;
        Self::from_runtime_config(provider_config, model, config.runtime_max_output_tokens)
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let auth_token = provider_config
            .credential
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                let credential_name = provider_config
                    .auth
                    .env
                    .as_deref()
                    .or(provider_config.auth.profile.as_deref())
                    .or(provider_config.auth.external.as_deref())
                    .unwrap_or("configured credential");
                anyhow!("missing {credential_name}")
            })?;
        Ok(Self {
            client,
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            auth_token,
            model: model.to_string(),
            max_output_tokens,
            context_management: provider_config.context_management.clone(),
        })
    }
}

#[async_trait]
impl AgentProvider for AnthropicProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let cache_strategy = self.context_management.cache_strategy;
        let wire_conversation = build_anthropic_wire_conversation(&request, cache_strategy);
        let rolling_cache_marker =
            rolling_conversation_cache_marker(&wire_conversation, cache_strategy);
        let messages = build_anthropic_messages(&wire_conversation, rolling_cache_marker);
        let request_body = MessagesRequest {
            model: &self.model,
            max_tokens: self.max_output_tokens,
            system: build_anthropic_system(&request, cache_strategy),
            messages,
            tools: request
                .tools
                .iter()
                .map(|tool| ApiTool {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
            betas: self.context_management.betas.clone(),
            metadata: build_anthropic_metadata(&request, cache_strategy),
            temperature: (cache_strategy == AnthropicCacheStrategy::ClaudeCliLike).then_some(1.0),
            context_management: build_context_management_request(&self.context_management),
        };
        let request_payload = serde_json::to_value(&request_body)?;

        let url = format!("{}/v1/messages", self.base_url);
        let mut request_builder = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.auth_token))
            .header("anthropic-version", "2023-06-01");
        if self.context_management.enabled {
            request_builder =
                request_builder.header("anthropic-beta", "context-management-2025-06-27");
        }
        let response = request_builder
            .json(&request_body)
            .send()
            .await
            .map_err(|error| {
                classify_reqwest_transport_error(
                    "Anthropic request failed",
                    "request_send",
                    "anthropic",
                    Some(&format!("anthropic/{}", self.model)),
                    Some(&format!("{}/v1/messages", self.base_url)),
                    error,
                )
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_status_error(
                "Anthropic request failed",
                status,
                body,
            ));
        }

        let response_body = response.text().await.map_err(|error| {
            classify_reqwest_transport_error(
                "Anthropic response body failed",
                "response_body",
                "anthropic",
                Some(&format!("anthropic/{}", self.model)),
                Some(&format!("{}/v1/messages", self.base_url)),
                error,
            )
        })?;
        let parsed: MessagesResponse = serde_json::from_str(&response_body)
            .map_err(|error| invalid_response_error("invalid Anthropic JSON", error))?;
        let input_tokens = parsed
            .usage
            .as_ref()
            .and_then(|usage| usage.input_tokens)
            .unwrap_or(0);
        let output_tokens = parsed
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens)
            .unwrap_or(0);
        let cache_usage = parsed.usage.as_ref().map(|usage| ProviderCacheUsage {
            read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        });
        let blocks = parsed
            .content
            .into_iter()
            .filter_map(api_response_block_to_model)
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            return Err(anyhow!(
                "Anthropic response contained no supported content blocks"
            ));
        }

        let cache_diagnostics = collect_anthropic_cache_diagnostics(
            &request,
            &wire_conversation,
            rolling_cache_marker,
            &request_payload,
            &self.model,
            cache_strategy,
            &self.context_management.betas,
        );

        // Determine the actual request lowering mode based on system block structure
        let request_lowering_mode = anthropic_request_lowering_mode(&request, cache_strategy);

        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: parsed.stop_reason,
            input_tokens,
            output_tokens,
            cache_usage,
            request_diagnostics: Some(crate::provider::ProviderRequestDiagnostics {
                request_lowering_mode: request_lowering_mode.to_string(),
                anthropic_cache: Some(cache_diagnostics),
                anthropic_context_management: parsed.context_management,
                incremental_continuation: None,
            }),
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("anthropic/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        let mut capabilities = vec![
            ProviderPromptCapability::FullRequestOnly,
            ProviderPromptCapability::PromptCacheBlocks,
        ];
        if self.context_management.enabled {
            capabilities.push(ProviderPromptCapability::ContextManagement);
        }
        capabilities
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        self.context_management
            .enabled
            .then(|| ProviderContextManagementPolicy {
                provider: "anthropic".to_string(),
                strategy: "clear_tool_uses_20250919".to_string(),
                keep_recent_tool_uses: self.context_management.keep_recent_tool_uses as usize,
                trigger_input_tokens: self.context_management.trigger_input_tokens,
                clear_at_least_input_tokens: self.context_management.clear_at_least_input_tokens,
            })
    }
}

fn build_anthropic_wire_conversation(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
) -> Vec<ConversationMessage> {
    match cache_strategy {
        AnthropicCacheStrategy::Current => request.conversation.clone(),
        AnthropicCacheStrategy::ClaudeCliLike => strip_initial_context_message(request),
    }
}

fn strip_initial_context_message(request: &ProviderTurnRequest) -> Vec<ConversationMessage> {
    let mut conversation = request.conversation.clone();
    if matches!(
        conversation.first(),
        Some(ConversationMessage::UserBlocks(blocks))
            if *blocks == request.prompt_frame.context_blocks
    ) {
        conversation.remove(0);
        if conversation.is_empty()
            || !matches!(
                conversation.first(),
                Some(ConversationMessage::UserText(_) | ConversationMessage::UserBlocks(_))
            )
        {
            conversation.insert(
                0,
                ConversationMessage::UserText("Continue using the context above.".to_string()),
            );
        }
    }
    conversation
}

fn build_anthropic_system(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
) -> Value {
    match cache_strategy {
        AnthropicCacheStrategy::Current => current_anthropic_system(request),
        AnthropicCacheStrategy::ClaudeCliLike => claude_cli_like_anthropic_system(request),
    }
}

fn current_anthropic_system(request: &ProviderTurnRequest) -> Value {
    if request.prompt_frame.has_structured_system_blocks() {
        Value::Array(
            request
                .prompt_frame
                .system_blocks
                .iter()
                .map(prompt_block_to_anthropic_content)
                .collect(),
        )
    } else {
        Value::String(request.prompt_frame.system_prompt.clone())
    }
}

fn claude_cli_like_anthropic_system(request: &ProviderTurnRequest) -> Value {
    let mut system = vec![json!({
        "type": "text",
        "text": "x-anthropic-billing-header: holon",
    })];

    if !request.prompt_frame.system_prompt.trim().is_empty() {
        system.push(cacheable_text_block(&request.prompt_frame.system_prompt));
    }

    let context_text = request
        .prompt_frame
        .context_blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !context_text.trim().is_empty() {
        system.push(cacheable_text_block(&context_text));
    }

    Value::Array(system)
}

fn cacheable_text_block(text: &str) -> Value {
    json!({
        "type": "text",
        "text": text,
        "cache_control": { "type": "ephemeral" },
    })
}

fn build_anthropic_metadata(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
) -> Option<Value> {
    if cache_strategy != AnthropicCacheStrategy::ClaudeCliLike {
        return None;
    }
    let session_id = request
        .prompt_frame
        .cache
        .as_ref()
        .map(|cache| cache.prompt_cache_key.as_str())
        .unwrap_or("holon-default");
    let user_id = json!({
        "device_id": "holon",
        "account_uuid": "",
        "session_id": session_id,
    })
    .to_string();
    Some(json!({
        "user_id": user_id
    }))
}

fn anthropic_request_lowering_mode(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
) -> &'static str {
    match cache_strategy {
        AnthropicCacheStrategy::ClaudeCliLike => "claude_cli_like_prompt_cache",
        AnthropicCacheStrategy::Current if request.prompt_frame.has_structured_system_blocks() => {
            "prompt_cache_blocks"
        }
        AnthropicCacheStrategy::Current => "plain_system",
    }
}

fn build_context_management_request(
    config: &AnthropicContextManagementConfig,
) -> Option<ContextManagementRequest> {
    config.enabled.then(|| ContextManagementRequest {
        edits: vec![ContextManagementEdit {
            kind: "clear_tool_uses_20250919",
            trigger: ContextManagementThreshold {
                kind: "input_tokens",
                value: config.trigger_input_tokens,
            },
            keep: ContextManagementThreshold {
                kind: "tool_uses",
                value: config.keep_recent_tool_uses,
            },
            exclude_tools: vec!["ApplyPatch", "NotifyOperator"],
            clear_at_least: config.clear_at_least_input_tokens.map(|value| {
                ContextManagementThreshold {
                    kind: "input_tokens",
                    value,
                }
            }),
        }],
    })
}

fn build_anthropic_messages(
    conversation: &[ConversationMessage],
    rolling_cache_marker: Option<(usize, usize)>,
) -> Vec<ApiMessage> {
    conversation
        .iter()
        .enumerate()
        .map(|(message_index, message)| {
            conversation_message_to_api(
                message,
                rolling_cache_marker
                    .filter(|(marker_message_index, _)| *marker_message_index == message_index)
                    .map(|(_, marker_block_index)| marker_block_index),
            )
        })
        .collect()
}

fn conversation_message_to_api(
    message: &ConversationMessage,
    rolling_cache_block_index: Option<usize>,
) -> ApiMessage {
    match message {
        ConversationMessage::UserText(text) => ApiMessage {
            role: "user",
            content: Value::Array(vec![maybe_mark_cache_control(
                json!({ "type": "text", "text": text }),
                rolling_cache_block_index == Some(0),
            )]),
        },
        ConversationMessage::UserBlocks(blocks) => ApiMessage {
            role: "user",
            content: Value::Array(
                blocks
                    .iter()
                    .enumerate()
                    .map(|(block_index, block)| {
                        maybe_mark_cache_control(
                            prompt_block_to_anthropic_content(block),
                            rolling_cache_block_index == Some(block_index),
                        )
                    })
                    .collect(),
            ),
        },
        ConversationMessage::AssistantBlocks(blocks) => ApiMessage {
            role: "assistant",
            content: Value::Array(
                blocks
                    .iter()
                    .enumerate()
                    .map(|(block_index, block)| {
                        maybe_mark_cache_control(
                            match block {
                                ModelBlock::Text { text } => json!({
                                    "type": "text",
                                    "text": text,
                                }),
                                ModelBlock::ToolUse { id, name, input } => json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input,
                                }),
                            },
                            rolling_cache_block_index == Some(block_index),
                        )
                    })
                    .collect(),
            ),
        },
        ConversationMessage::UserToolResults(results) => ApiMessage {
            role: "user",
            content: Value::Array(
                results
                    .iter()
                    .enumerate()
                    .map(|(block_index, result)| {
                        maybe_mark_cache_control(
                            json!({
                                "type": "tool_result",
                                "tool_use_id": result.tool_use_id,
                                "content": result.content,
                                "is_error": result.is_error,
                            }),
                            rolling_cache_block_index == Some(block_index),
                        )
                    })
                    .collect(),
            ),
        },
    }
}

fn maybe_mark_cache_control(mut content: Value, should_mark: bool) -> Value {
    if should_mark && content.get("cache_control").is_none() {
        content["cache_control"] = json!({ "type": "ephemeral" });
    }
    content
}

fn rolling_conversation_cache_marker(
    conversation: &[ConversationMessage],
    cache_strategy: AnthropicCacheStrategy,
) -> Option<(usize, usize)> {
    conversation
        .iter()
        .enumerate()
        .rev()
        .find_map(|(message_index, message)| {
            last_cacheable_content_index(message, cache_strategy)
                .map(|block_index| (message_index, block_index))
        })
}

fn last_cacheable_content_index(
    message: &ConversationMessage,
    cache_strategy: AnthropicCacheStrategy,
) -> Option<usize> {
    match message {
        ConversationMessage::UserText(_) => Some(0),
        ConversationMessage::UserBlocks(blocks) => (!blocks.is_empty()).then_some(blocks.len() - 1),
        ConversationMessage::AssistantBlocks(blocks) => match cache_strategy {
            AnthropicCacheStrategy::Current => (!blocks.is_empty()).then_some(blocks.len() - 1),
            AnthropicCacheStrategy::ClaudeCliLike => {
                blocks.iter().enumerate().rev().find_map(|(index, block)| {
                    matches!(block, ModelBlock::Text { .. }).then_some(index)
                })
            }
        },
        ConversationMessage::UserToolResults(results) => match cache_strategy {
            AnthropicCacheStrategy::Current => (!results.is_empty()).then_some(results.len() - 1),
            AnthropicCacheStrategy::ClaudeCliLike => None,
        },
    }
}

fn prompt_block_to_anthropic_content(block: &PromptContentBlock) -> Value {
    let mut content = json!({
        "type": "text",
        "text": block.text,
    });
    if block.cache_breakpoint {
        content["cache_control"] = json!({ "type": "ephemeral" });
    }
    content
}

fn api_response_block_to_model(block: ApiResponseBlock) -> Option<ModelBlock> {
    match block.kind.as_str() {
        "text" => Some(ModelBlock::Text {
            text: block.text.unwrap_or_default(),
        }),
        "tool_use" => Some(ModelBlock::ToolUse {
            id: block.id?,
            name: block.name?,
            input: block.input.unwrap_or_else(|| json!({})),
        }),
        _ => None,
    }
}

fn collect_anthropic_cache_diagnostics(
    request: &ProviderTurnRequest,
    conversation: &[ConversationMessage],
    rolling_cache_marker: Option<(usize, usize)>,
    request_payload: &Value,
    model: &str,
    cache_strategy: AnthropicCacheStrategy,
    betas: &[String],
) -> AnthropicPromptCacheDiagnostics {
    let tools_count = request.tools.len();
    let tools_hash = hash_tools(&request.tools);

    let (system_hash, system_block_count, estimated_system_tokens) =
        payload_system_diagnostics(request_payload);

    let context_hash_by_stability = hash_context_by_stability(&request.prompt_frame.context_blocks);

    let (conversation_message_count, conversation_content_block_count) =
        count_conversation_blocks(conversation);

    let cache_breakpoints = collect_cache_breakpoints(
        request,
        conversation,
        rolling_cache_marker,
        request_payload,
        cache_strategy,
    );

    let (tokens_before_last_breakpoint, tokens_after_last_breakpoint) =
        estimate_token_distribution_from_payload(request_payload, &cache_breakpoints);
    let (system_cache_control_count, message_cache_control_count) =
        count_payload_cache_controls(request_payload);

    AnthropicPromptCacheDiagnostics {
        cache_strategy: cache_strategy.as_str().to_string(),
        model: model.to_string(),
        betas: betas.to_vec(),
        tools_count,
        tools_hash,
        system_hash,
        system_block_count,
        estimated_system_tokens,
        context_hash_by_stability,
        conversation_message_count,
        conversation_content_block_count,
        system_cache_control_count,
        message_cache_control_count,
        cache_breakpoints,
        tokens_before_last_breakpoint,
        tokens_after_last_breakpoint,
        automatic_cache_control_requested: false,
    }
}

fn hash_tools(tools: &[crate::tool::ToolSpec]) -> String {
    let mut hasher = XxHash64::default();
    for tool in tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        // Hash input_schema to detect schema changes
        tool.input_schema.to_string().hash(&mut hasher);
        // Hash freeform_grammar to detect grammar changes
        match &tool.freeform_grammar {
            Some(grammar) => {
                grammar.syntax.hash(&mut hasher);
                grammar.definition.hash(&mut hasher);
            }
            None => 0u64.hash(&mut hasher),
        };
    }
    format!("{:x}", hasher.finish())
}

fn hash_system_blocks(blocks: &[PromptContentBlock], system_prompt: &str) -> String {
    let mut hasher = XxHash64::default();
    system_prompt.hash(&mut hasher);
    for block in blocks {
        block.text.hash(&mut hasher);
        block.stability.hash(&mut hasher);
        // Include cache_breakpoint in hash to detect breakpoint changes
        block.cache_breakpoint.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn payload_system_diagnostics(request_payload: &Value) -> (String, usize, u64) {
    let Some(system) = request_payload.get("system") else {
        return (hash_system_blocks(&[], ""), 0, 0);
    };
    let block_count = system.as_array().map(Vec::len).unwrap_or(0);
    let estimated_tokens = match system {
        Value::String(text) => estimate_tokens_from_chars(text.len()),
        Value::Array(blocks) => estimate_tokens_from_chars(
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::len)
                .sum(),
        ),
        _ => 0,
    };
    let mut hasher = XxHash64::default();
    canonical_json(system).hash(&mut hasher);
    (
        format!("{:x}", hasher.finish()),
        block_count,
        estimated_tokens,
    )
}

fn stability_label(stability: crate::prompt::PromptStability) -> &'static str {
    match stability {
        crate::prompt::PromptStability::Stable => "stable",
        crate::prompt::PromptStability::AgentScoped => "agent_scoped",
        crate::prompt::PromptStability::TurnScoped => "turn_scoped",
    }
}

fn hash_context_by_stability(
    blocks: &[PromptContentBlock],
) -> std::collections::BTreeMap<String, String> {
    let mut by_stability: std::collections::HashMap<PromptStability, Vec<&PromptContentBlock>> =
        HashMap::new();
    for block in blocks {
        by_stability.entry(block.stability).or_default().push(block);
    }

    let mut result = std::collections::BTreeMap::new();
    for (stability, blocks) in by_stability {
        let mut hasher = XxHash64::default();
        for block in blocks {
            block.text.hash(&mut hasher);
            // Include cache_breakpoint in context hash to detect breakpoint changes
            block.cache_breakpoint.hash(&mut hasher);
        }
        result.insert(
            stability_label(stability).to_string(),
            format!("{:x}", hasher.finish()),
        );
    }
    result
}

fn estimate_tokens_from_chars(byte_count: usize) -> u64 {
    (byte_count / 4) as u64
}

fn count_conversation_blocks(conversation: &[ConversationMessage]) -> (usize, usize) {
    let message_count = conversation.len();
    let block_count = conversation.iter().fold(0, |acc, msg| match msg {
        ConversationMessage::UserBlocks(blocks) => acc + blocks.len(),
        ConversationMessage::AssistantBlocks(blocks) => acc + blocks.len(),
        ConversationMessage::UserToolResults(results) => acc + results.len(),
        _ => acc,
    });
    (message_count, block_count)
}

fn collect_cache_breakpoints(
    request: &ProviderTurnRequest,
    conversation: &[ConversationMessage],
    rolling_cache_marker: Option<(usize, usize)>,
    request_payload: &Value,
    cache_strategy: AnthropicCacheStrategy,
) -> Vec<CacheBreakpointInfo> {
    const MAX_BREAKPOINTS: usize = 10;
    let mut breakpoints = Vec::new();
    let mut token_offset = 0u64;

    match request_payload.get("system") {
        Some(Value::String(text)) => {
            token_offset += estimate_tokens_from_chars(text.len());
        }
        Some(Value::Array(system_blocks)) => {
            for (idx, system_block) in system_blocks.iter().enumerate() {
                let text = system_block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let block_tokens = estimate_tokens_from_chars(text.len());
                if system_block.get("cache_control").is_some() {
                    let (location, stability) =
                        system_cache_breakpoint_metadata(request, cache_strategy, idx);
                    if breakpoints.len() < MAX_BREAKPOINTS {
                        breakpoints.push(CacheBreakpointInfo {
                            location,
                            provider_payload_path: format!("system[{}]", idx),
                            block_kind: "system_text".to_string(),
                            stability,
                            estimated_prefix_tokens: token_offset + block_tokens,
                            content_hash: hash_secret_safe_string(text),
                            canonical_prefix_fingerprint: canonical_prefix_fingerprint(
                                request_payload,
                                CacheBreakpointPath::System(idx),
                            ),
                        });
                    }
                }
                token_offset += block_tokens;
            }
        }
        _ => {}
    }

    // Process conversation messages.
    for (msg_idx, message) in conversation.iter().enumerate() {
        if breakpoints.len() >= MAX_BREAKPOINTS {
            break;
        }
        match message {
            ConversationMessage::UserBlocks(blocks) => {
                for (block_idx, block) in blocks.iter().enumerate() {
                    if breakpoints.len() >= MAX_BREAKPOINTS {
                        break;
                    }
                    let block_tokens = estimate_tokens_from_chars(block.text.len());
                    if block.cache_breakpoint || rolling_cache_marker == Some((msg_idx, block_idx))
                    {
                        breakpoints.push(CacheBreakpointInfo {
                            location: format!("messages[{}].content[{}]", msg_idx, block_idx),
                            provider_payload_path: format!(
                                "messages[{}].content[{}]",
                                msg_idx, block_idx
                            ),
                            block_kind: "user_text".to_string(),
                            stability: if block.cache_breakpoint {
                                stability_label(block.stability).to_string()
                            } else {
                                "conversation_tail".to_string()
                            },
                            estimated_prefix_tokens: token_offset + block_tokens,
                            content_hash: hash_secret_safe_string(&block.text),
                            canonical_prefix_fingerprint: canonical_prefix_fingerprint(
                                request_payload,
                                CacheBreakpointPath::MessageContent(msg_idx, block_idx),
                            ),
                        });
                    }
                    token_offset += block_tokens;
                }
            }
            ConversationMessage::UserText(text) => {
                let block_tokens = estimate_tokens_from_chars(text.len());
                if rolling_cache_marker == Some((msg_idx, 0)) {
                    breakpoints.push(CacheBreakpointInfo {
                        location: format!("messages[{}].content[0]", msg_idx),
                        provider_payload_path: format!("messages[{}].content[0]", msg_idx),
                        block_kind: "user_text".to_string(),
                        stability: "conversation_tail".to_string(),
                        estimated_prefix_tokens: token_offset + block_tokens,
                        content_hash: hash_secret_safe_string(text),
                        canonical_prefix_fingerprint: canonical_prefix_fingerprint(
                            request_payload,
                            CacheBreakpointPath::MessageContent(msg_idx, 0),
                        ),
                    });
                }
                token_offset += block_tokens;
            }
            ConversationMessage::AssistantBlocks(blocks) => {
                for (block_idx, block) in blocks.iter().enumerate() {
                    if breakpoints.len() >= MAX_BREAKPOINTS {
                        break;
                    }
                    let block_tokens = estimate_model_block_tokens(block);
                    if rolling_cache_marker == Some((msg_idx, block_idx)) {
                        breakpoints.push(CacheBreakpointInfo {
                            location: format!("messages[{}].content[{}]", msg_idx, block_idx),
                            provider_payload_path: format!(
                                "messages[{}].content[{}]",
                                msg_idx, block_idx
                            ),
                            block_kind: model_block_kind(block).to_string(),
                            stability: "conversation_tail".to_string(),
                            estimated_prefix_tokens: token_offset + block_tokens,
                            content_hash: hash_model_block(block),
                            canonical_prefix_fingerprint: canonical_prefix_fingerprint(
                                request_payload,
                                CacheBreakpointPath::MessageContent(msg_idx, block_idx),
                            ),
                        });
                    }
                    token_offset += block_tokens;
                }
            }
            ConversationMessage::UserToolResults(results) => {
                for (block_idx, result) in results.iter().enumerate() {
                    if breakpoints.len() >= MAX_BREAKPOINTS {
                        break;
                    }
                    let block_tokens = estimate_tokens_from_chars(result.content.len());
                    if rolling_cache_marker == Some((msg_idx, block_idx)) {
                        breakpoints.push(CacheBreakpointInfo {
                            location: format!("messages[{}].content[{}]", msg_idx, block_idx),
                            provider_payload_path: format!(
                                "messages[{}].content[{}]",
                                msg_idx, block_idx
                            ),
                            block_kind: "tool_result".to_string(),
                            stability: "conversation_tail".to_string(),
                            estimated_prefix_tokens: token_offset + block_tokens,
                            content_hash: hash_tool_result_block(result),
                            canonical_prefix_fingerprint: canonical_prefix_fingerprint(
                                request_payload,
                                CacheBreakpointPath::MessageContent(msg_idx, block_idx),
                            ),
                        });
                    }
                    token_offset += block_tokens;
                }
            }
        }
    }

    breakpoints
}

fn system_cache_breakpoint_metadata(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
    idx: usize,
) -> (String, String) {
    if cache_strategy == AnthropicCacheStrategy::Current {
        if let Some(block) = request.prompt_frame.system_blocks.get(idx) {
            return (
                format!("system_blocks[{}]", idx),
                stability_label(block.stability).to_string(),
            );
        }
    }
    (format!("system[{}]", idx), "provider_system".to_string())
}

enum CacheBreakpointPath {
    System(usize),
    MessageContent(usize, usize),
}

fn canonical_prefix_fingerprint(
    request_payload: &Value,
    breakpoint: CacheBreakpointPath,
) -> String {
    let mut prefix = canonical_prefix_base(request_payload);
    match breakpoint {
        CacheBreakpointPath::System(index) => {
            if let Some(system) = request_payload.get("system").and_then(Value::as_array) {
                let end = index.saturating_add(1).min(system.len());
                prefix.insert("system".to_string(), Value::Array(system[..end].to_vec()));
            }
            if request_payload.get("messages").is_some() {
                prefix.insert("messages".to_string(), Value::Array(Vec::new()));
            }
        }
        CacheBreakpointPath::MessageContent(message_index, content_index) => {
            if let Some(system) = request_payload.get("system") {
                prefix.insert("system".to_string(), system.clone());
            }
            if let Some(messages) = request_payload.get("messages").and_then(Value::as_array) {
                let end = message_index.saturating_add(1).min(messages.len());
                let mut truncated_messages = messages[..end].to_vec();
                if let Some(message) = truncated_messages.get_mut(message_index) {
                    if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut)
                    {
                        content.truncate(content_index.saturating_add(1));
                    }
                }
                prefix.insert("messages".to_string(), Value::Array(truncated_messages));
            }
        }
    }
    sha256_hex(canonical_json(&Value::Object(prefix)).as_bytes())
}

fn canonical_prefix_base(request_payload: &Value) -> serde_json::Map<String, Value> {
    let mut prefix = serde_json::Map::new();
    if let Some(object) = request_payload.as_object() {
        for (key, value) in object {
            if key != "system" && key != "messages" {
                prefix.insert(key.clone(), value.clone());
            }
        }
    }
    prefix
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(values) => {
            let items = values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{}]", items)
        }
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let items = keys
                .into_iter()
                .map(|key| {
                    let key_json =
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
                    let value_json = canonical_json(&map[key]);
                    format!("{}:{}", key_json, value_json)
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", items)
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn hash_secret_safe_string(s: &str) -> String {
    sha256_hex(s.as_bytes())
}

fn model_block_kind(block: &ModelBlock) -> &'static str {
    match block {
        ModelBlock::Text { .. } => "assistant_text",
        ModelBlock::ToolUse { .. } => "tool_use",
    }
}

fn hash_model_block(block: &ModelBlock) -> String {
    match block {
        ModelBlock::Text { text } => {
            let value = json!({ "type": "text", "text": text });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::ToolUse { id, name, input } => {
            let value = json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            });
            sha256_hex(canonical_json(&value).as_bytes())
        }
    }
}

fn hash_tool_result_block(result: &crate::provider::ToolResultBlock) -> String {
    let value = json!({
        "type": "tool_result",
        "tool_use_id": result.tool_use_id,
        "content": result.content,
        "is_error": result.is_error,
    });
    sha256_hex(canonical_json(&value).as_bytes())
}

fn estimate_model_block_tokens(block: &ModelBlock) -> u64 {
    match block {
        ModelBlock::Text { text } => estimate_tokens_from_chars(text.len()),
        ModelBlock::ToolUse { .. } => 50,
    }
}

fn estimate_token_distribution_from_payload(
    request_payload: &Value,
    breakpoints: &[CacheBreakpointInfo],
) -> (u64, u64) {
    if let Some(last_bp) = breakpoints.last() {
        let before = last_bp.estimated_prefix_tokens;
        let total_estimated = estimate_total_payload_tokens(request_payload);
        let after = total_estimated.saturating_sub(before);
        (before, after)
    } else {
        (0, estimate_total_payload_tokens(request_payload))
    }
}

fn estimate_total_payload_tokens(request_payload: &Value) -> u64 {
    let mut total = 0u64;
    match request_payload.get("system") {
        Some(Value::String(text)) => {
            total += estimate_tokens_from_chars(text.len());
        }
        Some(Value::Array(blocks)) => {
            total += blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(|text| estimate_tokens_from_chars(text.len()))
                .sum::<u64>();
        }
        _ => {}
    }

    if let Some(messages) = request_payload.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(content) = message.get("content").and_then(Value::as_array) {
                for block in content {
                    total += match block.get("type").and_then(Value::as_str) {
                        Some("text") | Some("tool_result") => block
                            .get("text")
                            .or_else(|| block.get("content"))
                            .and_then(Value::as_str)
                            .map(|text| estimate_tokens_from_chars(text.len()))
                            .unwrap_or(0),
                        Some("tool_use") => 50,
                        _ => 0,
                    };
                }
            }
        }
    }
    total
}

fn count_payload_cache_controls(request_payload: &Value) -> (usize, usize) {
    let system_count = request_payload
        .get("system")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("cache_control").is_some())
                .count()
        })
        .unwrap_or(0);
    let message_count = request_payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message.get("content").and_then(Value::as_array))
                .flatten()
                .filter(|block| block.get("cache_control").is_some())
                .count()
        })
        .unwrap_or(0);
    (system_count, message_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::PromptStability;
    use crate::provider::{PromptContentBlock, ProviderPromptFrame, ToolResultBlock};
    use serde_json::json;

    #[test]
    fn build_anthropic_messages_marks_latest_tool_result_as_rolling_cache_tail() {
        let conversation = vec![
            ConversationMessage::UserText("inspect".to_string()),
            ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "ExecCommand".to_string(),
                input: json!({ "cmd": "printf ok" }),
            }]),
            ConversationMessage::UserToolResults(vec![ToolResultBlock {
                tool_use_id: "toolu_1".to_string(),
                content: "ok".to_string(),
                is_error: false,
                error: None,
            }]),
        ];

        let messages = build_anthropic_messages(
            &conversation,
            rolling_conversation_cache_marker(&conversation, AnthropicCacheStrategy::Current),
        );

        assert_eq!(
            messages[2].content[0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert!(messages[1].content[0].get("cache_control").is_none());
    }

    #[test]
    fn build_anthropic_messages_marks_latest_assistant_block_as_rolling_cache_tail() {
        let conversation = vec![
            ConversationMessage::UserText("hello".to_string()),
            ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
                text: "done".to_string(),
            }]),
        ];

        let messages = build_anthropic_messages(
            &conversation,
            rolling_conversation_cache_marker(&conversation, AnthropicCacheStrategy::Current),
        );

        assert_eq!(
            messages[1].content[0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert!(messages[0].content[0].get("cache_control").is_none());
    }

    #[test]
    fn rolling_cache_marker_does_not_mutate_provider_conversation() {
        let conversation = vec![ConversationMessage::UserBlocks(vec![PromptContentBlock {
            text: "turn scoped context".to_string(),
            stability: PromptStability::TurnScoped,
            cache_breakpoint: false,
        }])];

        let messages = build_anthropic_messages(
            &conversation,
            rolling_conversation_cache_marker(&conversation, AnthropicCacheStrategy::Current),
        );

        assert_eq!(
            messages[0].content[0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        let ConversationMessage::UserBlocks(blocks) = &conversation[0] else {
            panic!("conversation should remain user blocks");
        };
        assert!(!blocks[0].cache_breakpoint);
    }

    #[test]
    fn anthropic_request_payload_for_fingerprints_is_wire_object() {
        let request = ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame {
                system_prompt: "unused plain system".to_string(),
                system_blocks: vec![PromptContentBlock {
                    text: "System instruction".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                }],
                context_blocks: vec![],
                cache: None,
            },
            conversation: vec![ConversationMessage::UserText("Hello".to_string())],
            tools: vec![],
        };

        let request_payload = anthropic_request_payload_for_test(&request);

        assert!(request_payload.is_object());
        assert!(request_payload.get("system").is_some());
        assert!(request_payload.get("messages").is_some());
        let fingerprint =
            canonical_prefix_fingerprint(&request_payload, CacheBreakpointPath::System(0));
        assert!(!fingerprint.is_empty());
        assert_ne!(
            fingerprint,
            canonical_prefix_fingerprint(
                &Value::String("response body".to_string()),
                CacheBreakpointPath::System(0)
            )
        );
    }

    #[test]
    fn test_collect_anthropic_cache_diagnostics_initial_request() {
        let request = ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame {
                system_prompt: "You are a helpful assistant.".to_string(),
                system_blocks: vec![
                    PromptContentBlock {
                        text: "System instruction 1".to_string(),
                        stability: PromptStability::Stable,
                        cache_breakpoint: true,
                    },
                    PromptContentBlock {
                        text: "System instruction 2".to_string(),
                        stability: PromptStability::AgentScoped,
                        cache_breakpoint: false,
                    },
                ],
                context_blocks: vec![PromptContentBlock {
                    text: "Context information".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: false,
                }],
                cache: None,
            },
            conversation: vec![ConversationMessage::UserText(
                "Hello, how are you?".to_string(),
            )],
            tools: vec![],
        };

        let request_payload = anthropic_request_payload_for_test(&request);
        let diagnostics = collect_anthropic_cache_diagnostics(
            &request,
            &request.conversation,
            rolling_conversation_cache_marker(
                &request.conversation,
                AnthropicCacheStrategy::Current,
            ),
            &request_payload,
            "claude-sonnet-4-6",
            AnthropicCacheStrategy::Current,
            &[],
        );

        assert_eq!(diagnostics.tools_count, 0);
        assert_eq!(diagnostics.system_block_count, 2);
        assert!(diagnostics.estimated_system_tokens > 0);
        assert_eq!(diagnostics.conversation_message_count, 1);
        assert!(!diagnostics.cache_breakpoints.is_empty());
        assert_eq!(diagnostics.cache_breakpoints.len(), 2);
        assert_eq!(
            diagnostics.cache_breakpoints[0].location,
            "system_blocks[0]"
        );
        assert_eq!(diagnostics.cache_breakpoints[0].stability, "stable");
        assert_eq!(
            diagnostics.cache_breakpoints[0].provider_payload_path,
            "system[0]"
        );
        assert_eq!(diagnostics.cache_breakpoints[0].block_kind, "system_text");
        assert!(!diagnostics.cache_breakpoints[0]
            .canonical_prefix_fingerprint
            .is_empty());
        assert_eq!(
            diagnostics.cache_breakpoints[1].location,
            "messages[0].content[0]"
        );
        assert_eq!(
            diagnostics.cache_breakpoints[1].stability,
            "conversation_tail"
        );
        assert!(diagnostics.tokens_before_last_breakpoint > 0);
        assert_eq!(diagnostics.tokens_after_last_breakpoint, 0);
    }

    #[test]
    fn test_collect_anthropic_cache_diagnostics_continuation_with_breakpoints() {
        let request = ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame {
                system_prompt: "You are a helpful assistant.".to_string(),
                system_blocks: vec![
                    PromptContentBlock {
                        text: "System instruction 1".to_string(),
                        stability: PromptStability::Stable,
                        cache_breakpoint: true,
                    },
                    PromptContentBlock {
                        text: "System instruction 2".to_string(),
                        stability: PromptStability::AgentScoped,
                        cache_breakpoint: true,
                    },
                ],
                context_blocks: vec![PromptContentBlock {
                    text: "Context information".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: false,
                }],
                cache: None,
            },
            conversation: vec![
                ConversationMessage::UserBlocks(vec![PromptContentBlock {
                    text: "User message content".to_string(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: true,
                }]),
                ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
                    text: "I'm doing well, thank you!".to_string(),
                }]),
            ],
            tools: vec![],
        };

        let request_payload = anthropic_request_payload_for_test(&request);
        let diagnostics = collect_anthropic_cache_diagnostics(
            &request,
            &request.conversation,
            rolling_conversation_cache_marker(
                &request.conversation,
                AnthropicCacheStrategy::Current,
            ),
            &request_payload,
            "claude-sonnet-4-6",
            AnthropicCacheStrategy::Current,
            &[],
        );

        assert_eq!(diagnostics.system_block_count, 2);
        assert!(diagnostics.cache_breakpoints.len() <= 10);
        assert!(diagnostics.cache_breakpoints.len() > 0);

        // Check that breakpoints include system blocks
        let system_breakpoints: Vec<_> = diagnostics
            .cache_breakpoints
            .iter()
            .filter(|bp| bp.location.starts_with("system_blocks"))
            .collect();
        assert_eq!(system_breakpoints.len(), 2);

        // Check token distribution
        assert!(diagnostics.tokens_before_last_breakpoint > 0);
        assert!(diagnostics.tokens_after_last_breakpoint < u64::MAX);
    }

    fn anthropic_request_payload_for_test(request: &ProviderTurnRequest) -> Value {
        let rolling_cache_marker = rolling_conversation_cache_marker(
            &request.conversation,
            AnthropicCacheStrategy::Current,
        );
        let body = MessagesRequest {
            model: "claude-sonnet-4-6",
            max_tokens: 4096,
            system: if request.prompt_frame.has_structured_system_blocks() {
                Value::Array(
                    request
                        .prompt_frame
                        .system_blocks
                        .iter()
                        .map(prompt_block_to_anthropic_content)
                        .collect(),
                )
            } else {
                Value::String(request.prompt_frame.system_prompt.clone())
            },
            messages: build_anthropic_messages(&request.conversation, rolling_cache_marker),
            tools: Vec::new(),
            betas: Vec::new(),
            metadata: None,
            temperature: None,
            context_management: None,
        };
        serde_json::to_value(body).expect("test request payload should serialize")
    }

    #[test]
    fn test_hash_tools_includes_schema() {
        use crate::tool::ToolSpec;

        let tool1 = ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            freeform_grammar: None,
        };

        let tool2 = ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object", "properties": {"new_field": {"type": "string"}}}),
            freeform_grammar: None,
        };

        let hash1 = hash_tools(&[tool1]);
        let hash2 = hash_tools(&[tool2]);

        // Different input_schema should produce different hashes
        assert_ne!(
            hash1, hash2,
            "tools_hash should differ when input_schema changes"
        );
    }

    #[test]
    fn test_hash_tools_includes_freeform_grammar() {
        use crate::tool::{spec::ToolFreeformGrammar, ToolSpec};

        let tool1 = ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            freeform_grammar: Some(ToolFreeformGrammar {
                syntax: "grammar_v1".to_string(),
                definition: "definition_v1".to_string(),
            }),
        };

        let tool2 = ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            freeform_grammar: Some(ToolFreeformGrammar {
                syntax: "grammar_v2".to_string(),
                definition: "definition_v2".to_string(),
            }),
        };

        let tool3 = ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            freeform_grammar: None,
        };

        let hash1 = hash_tools(&[tool1.clone()]);
        let hash2 = hash_tools(&[tool2]);
        let hash3 = hash_tools(&[tool3]);

        // Different freeform_grammar syntax should produce different hashes
        assert_ne!(
            hash1, hash2,
            "tools_hash should differ when freeform_grammar syntax changes"
        );

        // Tool with freeform_grammar should differ from tool without
        assert_ne!(
            hash1, hash3,
            "tools_hash should differ when freeform_grammar presence changes"
        );
    }

    #[test]
    fn test_hash_context_by_stability() {
        let blocks = vec![
            PromptContentBlock {
                text: "Stable content".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: false,
            },
            PromptContentBlock {
                text: "Agent scoped content".to_string(),
                stability: PromptStability::AgentScoped,
                cache_breakpoint: false,
            },
            PromptContentBlock {
                text: "More stable content".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: false,
            },
        ];

        let hashes = hash_context_by_stability(&blocks);

        assert!(hashes.contains_key("stable"));
        assert!(hashes.contains_key("agent_scoped"));
        assert!(!hashes.contains_key("turn_scoped"));
    }

    #[test]
    fn test_estimate_token_distribution() {
        let request = ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame {
                system_prompt: "System prompt".to_string(),
                system_blocks: vec![
                    PromptContentBlock {
                        text: "Block 1".to_string(),
                        stability: PromptStability::Stable,
                        cache_breakpoint: true,
                    },
                    PromptContentBlock {
                        text: "Block 2".to_string(),
                        stability: PromptStability::AgentScoped,
                        cache_breakpoint: false,
                    },
                ],
                context_blocks: vec![],
                cache: None,
            },
            conversation: vec![ConversationMessage::UserText("User message".to_string())],
            tools: vec![],
        };

        let request_payload = anthropic_request_payload_for_test(&request);
        let breakpoints = collect_cache_breakpoints(
            &request,
            &request.conversation,
            rolling_conversation_cache_marker(
                &request.conversation,
                AnthropicCacheStrategy::Current,
            ),
            &request_payload,
            AnthropicCacheStrategy::Current,
        );
        let (before, after) =
            estimate_token_distribution_from_payload(&request_payload, &breakpoints);

        let total = estimate_total_payload_tokens(&request_payload);
        assert_eq!(
            before.saturating_add(after),
            total,
            "before + after should equal total"
        );
    }

    #[test]
    fn test_cache_breakpoints_bounded() {
        let mut system_blocks = vec![];
        for i in 0..20 {
            system_blocks.push(PromptContentBlock {
                text: format!("System block {}", i),
                stability: PromptStability::Stable,
                cache_breakpoint: true,
            });
        }

        let request = ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame {
                system_prompt: "System prompt".to_string(),
                system_blocks,
                context_blocks: vec![],
                cache: None,
            },
            conversation: vec![],
            tools: vec![],
        };

        let request_payload = anthropic_request_payload_for_test(&request);
        let breakpoints = collect_cache_breakpoints(
            &request,
            &request.conversation,
            None,
            &request_payload,
            AnthropicCacheStrategy::Current,
        );

        // Should be bounded by MAX_BREAKPOINTS (10)
        assert!(
            breakpoints.len() <= 10,
            "cache_breakpoints should be bounded to 10 items"
        );
    }
}
