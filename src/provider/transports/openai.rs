use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    auth::load_codex_cli_credential,
    config::{AppConfig, CredentialKind, CredentialSource, ProviderId, ProviderRuntimeConfig},
    provider::{
        emitted_tool_json_schema, AgentProvider, ConversationMessage, ModelBlock,
        ProviderCacheUsage, ProviderIncrementalContinuationDiagnostics, ProviderPromptFrame,
        ProviderRequestDiagnostics, ProviderTurnRequest, ProviderTurnResponse, ToolSchemaContract,
    },
};

use super::build_http_client;
use crate::provider::retry::{
    classify_reqwest_transport_error, classify_status_error, invalid_response_error,
    provider_transport_error, ProviderFailureClassification, ProviderFailureKind, RetryDisposition,
};

#[derive(Clone)]
pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub struct OpenAiCodexProvider {
    client: Client,
    base_url: String,
    codex_home: std::path::PathBuf,
    originator: String,
    model: String,
    max_output_tokens: u32,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub struct OpenAiChatCompletionsProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenAiResponsesTransportContract {
    StandardJson,
    CodexStreaming,
    ChatCompletions,
}

#[derive(Debug, Default)]
struct OpenAiContinuationState {
    snapshots: HashMap<OpenAiContinuationScope, OpenAiContinuationSnapshot>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct OpenAiContinuationScope {
    agent_id: String,
    prompt_cache_key: String,
}

#[derive(Debug, Clone)]
struct OpenAiContinuationSnapshot {
    response_id: String,
    request_shape: OpenAiRequestShape,
    full_input: Vec<Value>,
    response_output: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenAiRequestShape {
    wire_shape: Value,
    prompt_frame: ProviderPromptFrame,
}

#[derive(Debug)]
struct OpenAiRequestPlan {
    body: Value,
    scope: Option<OpenAiContinuationScope>,
    full_input: Vec<Value>,
    request_shape: OpenAiRequestShape,
    diagnostics: ProviderRequestDiagnostics,
}

#[derive(Debug)]
pub(crate) struct ParsedOpenAiResponse {
    pub(crate) response: ProviderTurnResponse,
    pub(crate) response_id: Option<String>,
    pub(crate) output_items: Vec<Value>,
}

impl OpenAiProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        Self::from_runtime_config(provider_config, model, config.runtime_max_output_tokens)
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
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
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_output_tokens,
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

impl OpenAiCodexProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai_codex())
            .ok_or_else(|| anyhow::anyhow!("missing openai-codex provider config"))?;
        Self::from_runtime_config(provider_config, model, config.runtime_max_output_tokens)
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let codex_home = provider_config
            .codex_home
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing codex_home for OpenAI Codex provider"))?;
        load_codex_cli_credential(&codex_home)?;
        Ok(Self {
            client,
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            codex_home,
            originator: provider_config
                .originator
                .clone()
                .unwrap_or_else(|| "codex_cli_rs".into()),
            model: model.to_string(),
            max_output_tokens,
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

impl OpenAiChatCompletionsProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        Self::from_runtime_config(provider_config, model, config.runtime_max_output_tokens)
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
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
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_output_tokens,
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

#[async_trait]
impl AgentProvider for OpenAiProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Relaxed,
        )?;
        let plan = plan_openai_responses_request(body, &request, &self.continuation)?;
        let sent_diagnostics = plan.diagnostics.clone();
        let parsed = match send_openai_responses_request(
            &self.client,
            format!("{}/responses", self.base_url),
            plan.body,
            self.api_key
                .as_ref()
                .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
                .unwrap_or_default(),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan.scope,
            plan.request_shape,
            plan.full_input,
            &parsed,
        );
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }
}

#[async_trait]
impl AgentProvider for OpenAiCodexProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let credential = load_codex_cli_credential(&self.codex_home)?;
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
        )?;
        let plan = plan_openai_responses_request(body, &request, &self.continuation)?;
        let sent_diagnostics = plan.diagnostics.clone();
        let parsed = match send_openai_responses_streaming_request(
            &self.client,
            format!("{}/codex/responses", self.base_url),
            plan.body,
            vec![
                (
                    "authorization",
                    format!("Bearer {}", credential.access_token),
                ),
                ("chatgpt-account-id", credential.account_id),
                ("OpenAI-Beta", "responses=experimental".to_string()),
                ("originator", self.originator.clone()),
            ],
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan.scope,
            plan.request_shape,
            plan.full_input,
            &parsed,
        );
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai-codex/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }
}

#[async_trait]
impl AgentProvider for OpenAiChatCompletionsProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        // Build Chat Completions request
        let (body, plan) = plan_chat_completion_request(
            &self.model,
            self.max_output_tokens,
            &request,
            ToolSchemaContract::Relaxed,
            false, // Streaming infrastructure exists but is not currently enabled; requests are non-streaming
            &self.continuation,
        )?;
        let sent_diagnostics = plan.diagnostics.clone();

        // Send to /v1/chat/completions endpoint
        let parsed = match send_chat_completion_request(
            &self.client,
            chat_completions_url(&self.base_url),
            body,
            self.api_key
                .as_ref()
                .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
                .unwrap_or_default(),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };

        update_openai_continuation(
            &self.continuation,
            plan.scope,
            plan.request_shape,
            plan.full_input,
            &parsed,
        );

        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
        ]
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

fn has_trailing_version_segment(url: &str) -> bool {
    let Some(last_segment) = url.rsplit('/').next() else {
        return false;
    };
    let Some(version_digits) = last_segment.strip_prefix('v') else {
        return false;
    };
    !version_digits.is_empty() && version_digits.chars().all(|ch| ch.is_ascii_digit())
}

impl ProviderTurnResponse {
    fn with_request_diagnostics(mut self, diagnostics: ProviderRequestDiagnostics) -> Self {
        self.request_diagnostics = Some(diagnostics);
        self
    }
}

pub(crate) fn build_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
) -> Result<Value> {
    // Build messages array for Chat Completions API
    let messages =
        build_chat_completion_messages(&request.prompt_frame.system_prompt, &request.conversation)?;

    // Build tools array in OpenAI function calling format
    let tools = if !request.tools.is_empty() {
        Some(
            request
                .tools
                .iter()
                .map(|tool| {
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                            "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };

    // Build Chat Completions request body
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "stream": stream,
    });

    // Add tools if present
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String("auto".to_string());
    }

    // Add prompt cache key if available
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }

    Ok(body)
}

fn plan_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> Result<(Value, OpenAiRequestPlan)> {
    // Build full messages array
    let messages =
        build_chat_completion_messages(&request.prompt_frame.system_prompt, &request.conversation)?;

    // Build tools array
    let tools = if !request.tools.is_empty() {
        Some(
            request
                .tools
                .iter()
                .map(|tool| {
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                            "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };

    // Create full request body for shape comparison
    let mut full_body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "stream": stream,
    });

    if let Some(tools) = &tools {
        full_body["tools"] = Value::Array(tools.clone());
        full_body["tool_choice"] = Value::String("auto".to_string());
    }

    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        full_body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }

    // Calculate continuation scope
    let scope = continuation_scope(request);
    let full_messages = full_body["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let full_message_count = full_messages.len();
    let request_shape = request_shape_for_chat_completion(&full_body, request);

    // Check for continuation opportunity
    let Some(scope_ref) = scope.as_ref() else {
        // No continuation scope - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                full_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "missing_continuation_scope",
                    None,
                    full_message_count,
                ),
            },
        ));
    };

    let previous = lock_openai_continuation(continuation)
        .snapshots
        .get(scope_ref)
        .cloned();

    let Some(previous) = previous else {
        // No previous state - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                full_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "not_applicable_initial_request",
                    None,
                    full_message_count,
                ),
            },
        ));
    };

    // Check if request shape changed
    if previous.request_shape != request_shape {
        // Request changed - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                full_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "request_shape_changed",
                    None,
                    full_message_count,
                ),
            },
        ));
    }

    // Chat Completions continuation currently cannot safely reconstruct an
    // assistant message from `previous.response_output` for prefix matching.
    // `full_messages` contains message objects, but `response_output` is not
    // guaranteed to store a comparable message value, so incremental
    // continuation would be unreliable here. Send the full request instead.
    return Ok((
        full_body.clone(),
        OpenAiRequestPlan {
            body: full_body,
            scope,
            full_input: full_messages,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "chat_completions_incremental_continuation_unsupported",
                None,
                full_message_count,
            ),
        },
    ));
}

fn request_shape_for_chat_completion(
    body: &Value,
    request: &ProviderTurnRequest,
) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("messages");
        object.remove("prompt_cache_key");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

pub(crate) fn build_chat_completion_messages(
    system_prompt: &str,
    conversation: &[ConversationMessage],
) -> Result<Vec<Value>> {
    let mut messages = Vec::new();

    // Add system prompt as first message
    if !system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
    }

    // Process conversation messages
    for msg in conversation {
        match msg {
            ConversationMessage::UserText(text) => {
                messages.push(json!({
                    "role": "user",
                    "content": text,
                }));
            }
            ConversationMessage::UserBlocks(blocks) => {
                // Concatenate all block texts
                let content = blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            ConversationMessage::AssistantBlocks(blocks) => {
                // Extract text content and tool calls
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ModelBlock::ToolUse { id, name, input } => {
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input)
                                        .context("failed to serialize tool call arguments")?,
                                }
                            }));
                        }
                    }
                }

                // Build assistant message
                let content = text_parts.join("\n\n");
                let mut message = json!({
                    "role": "assistant",
                });

                // Set content field: null for tool-only messages, otherwise string content
                if !text_parts.is_empty() {
                    message["content"] = Value::String(content);
                } else {
                    message["content"] = Value::Null;
                }

                // Add tool_calls if present
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(tool_calls);
                }

                messages.push(message);
            }
            ConversationMessage::UserToolResults(results) => {
                // Each tool result becomes a separate "tool" message
                for result in results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_use_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    Ok(messages)
}

pub(crate) fn build_openai_responses_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    contract: OpenAiResponsesTransportContract,
    tool_schema_contract: ToolSchemaContract,
) -> Result<Value> {
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            if let Some(grammar) = tool.freeform_grammar.as_ref() {
                Ok(json!({
                    "type": "custom",
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "syntax": grammar.syntax,
                        "definition": grammar.definition,
                    }
                }))
            } else {
                Ok(json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                    "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                }))
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let mut body = json!({
        "model": model,
        "instructions": request.prompt_frame.system_prompt,
        "input": build_openai_input(&request.conversation)?,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
    });
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }
    match contract {
        OpenAiResponsesTransportContract::StandardJson => {
            body["max_output_tokens"] = Value::from(max_output_tokens);
        }
        OpenAiResponsesTransportContract::CodexStreaming => {
            body["stream"] = Value::Bool(true);
            body["reasoning"] = Value::Null;
            body["include"] = Value::Array(Vec::new());
        }
        OpenAiResponsesTransportContract::ChatCompletions => {
            // ChatCompletions uses a separate request building function
            // This branch should not be reached
            body["max_output_tokens"] = Value::from(max_output_tokens);
        }
    }
    Ok(body)
}

fn plan_openai_responses_request(
    mut body: Value,
    request: &ProviderTurnRequest,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> Result<OpenAiRequestPlan> {
    let full_input = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI request did not contain input array",
                "missing input",
            )
        })?;
    let full_input_items = full_input.len();
    let request_shape = request_shape_without_input(&body, request);
    let scope = continuation_scope(request);
    let Some(scope_ref) = scope.as_ref() else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "missing_continuation_scope",
                None,
                full_input_items,
            ),
        });
    };
    let previous = lock_openai_continuation(continuation)
        .snapshots
        .get(scope_ref)
        .cloned();
    let Some(previous) = previous else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "not_applicable_initial_request",
                None,
                full_input_items,
            ),
        });
    };

    if previous.request_shape != request_shape {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "request_shape_changed",
                None,
                full_input_items,
            ),
        });
    }

    let mut expected_prefix = previous.full_input.clone();
    expected_prefix.extend(previous.response_output.clone());
    if expected_prefix.is_empty()
        || full_input.len() <= expected_prefix.len()
        || !full_input.starts_with(&expected_prefix)
    {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "conversation_not_strict_append_only",
                None,
                full_input_items,
            ),
        });
    }

    let incremental_input = full_input[expected_prefix.len()..].to_vec();
    body["input"] = Value::Array(incremental_input.clone());
    body["previous_response_id"] = Value::String(previous.response_id);
    Ok(OpenAiRequestPlan {
        body,
        scope,
        full_input,
        request_shape,
        diagnostics: ProviderRequestDiagnostics {
            request_lowering_mode: "incremental_continuation".into(),
            anthropic_cache: None,
            anthropic_context_management: None,
            incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
                status: "hit".into(),
                fallback_reason: None,
                incremental_input_items: Some(incremental_input.len()),
                full_input_items: Some(full_input_items),
            }),
        },
    })
}

fn continuation_scope(request: &ProviderTurnRequest) -> Option<OpenAiContinuationScope> {
    request
        .prompt_frame
        .cache
        .as_ref()
        .map(|cache| OpenAiContinuationScope {
            agent_id: cache.agent_id.clone(),
            prompt_cache_key: cache.prompt_cache_key.clone(),
        })
}

fn request_shape_without_input(body: &Value, request: &ProviderTurnRequest) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("input");
        object.remove("previous_response_id");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

fn incremental_diagnostics(
    request_lowering_mode: &str,
    fallback_reason: &str,
    incremental_input_items: Option<usize>,
    full_input_items: usize,
) -> ProviderRequestDiagnostics {
    ProviderRequestDiagnostics {
        request_lowering_mode: request_lowering_mode.into(),
        anthropic_cache: None,
        anthropic_context_management: None,
        incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
            status: "fallback_full_request".into(),
            fallback_reason: Some(fallback_reason.into()),
            incremental_input_items,
            full_input_items: Some(full_input_items),
        }),
    }
}

fn update_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<OpenAiContinuationScope>,
    request_shape: OpenAiRequestShape,
    full_input: Vec<Value>,
    parsed: &ParsedOpenAiResponse,
) {
    let Some(scope) = scope else {
        return;
    };
    let next = match (parsed.response_id.as_ref(), parsed.output_items.is_empty()) {
        (Some(response_id), false) => Some(OpenAiContinuationSnapshot {
            response_id: response_id.clone(),
            request_shape,
            full_input,
            response_output: parsed.output_items.clone(),
        }),
        _ => None,
    };
    let mut state = lock_openai_continuation(continuation);
    if let Some(next) = next {
        state.snapshots.insert(scope, next);
    } else {
        state.snapshots.remove(&scope);
    }
}

fn invalidate_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&OpenAiContinuationScope>,
) {
    let mut state = lock_openai_continuation(continuation);
    if let Some(scope) = scope {
        state.snapshots.remove(scope);
    } else {
        state.snapshots.clear();
    }
}

fn lock_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> MutexGuard<'_, OpenAiContinuationState> {
    match continuation.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn build_openai_input(conversation: &[ConversationMessage]) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut custom_tool_calls = HashMap::<String, bool>::new();
    for message in conversation {
        match message {
            ConversationMessage::UserText(text) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            ConversationMessage::UserBlocks(blocks) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": blocks.iter().map(|block| json!({
                    "type": "input_text",
                    "text": block.text,
                })).collect::<Vec<_>>(),
            })),
            ConversationMessage::AssistantBlocks(blocks) => {
                let mut pending_text = Vec::new();
                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => pending_text.push(text.clone()),
                        ModelBlock::ToolUse { id, name, input } => {
                            flush_assistant_text(&mut items, &mut pending_text);
                            if let Some(raw_input) = openai_custom_tool_input(name, input) {
                                custom_tool_calls.insert(id.clone(), true);
                                items.push(json!({
                                    "type": "custom_tool_call",
                                    "call_id": id,
                                    "name": name,
                                    "input": raw_input,
                                }));
                            } else {
                                custom_tool_calls.insert(id.clone(), false);
                                items.push(json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": serde_json::to_string(input)
                                        .context("failed to serialize tool call arguments")?,
                                }));
                            }
                        }
                    }
                }
                flush_assistant_text(&mut items, &mut pending_text);
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    let item_type = if custom_tool_calls
                        .get(&result.tool_use_id)
                        .copied()
                        .unwrap_or(false)
                    {
                        "custom_tool_call_output"
                    } else {
                        "function_call_output"
                    };
                    items.push(json!({
                        "type": item_type,
                        "call_id": result.tool_use_id,
                        "output": result.content,
                    }));
                }
            }
        }
    }
    Ok(items)
}

fn openai_custom_tool_input(name: &str, input: &Value) -> Option<String> {
    if name != crate::tool::tools::apply_patch_tool::NAME {
        return None;
    }
    match input {
        Value::String(value) => Some(value.clone()),
        Value::Object(map) => map
            .get("patch")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        _ => None,
    }
}

fn flush_assistant_text(items: &mut Vec<Value>, pending_text: &mut Vec<String>) {
    if pending_text.is_empty() {
        return;
    }
    let content = pending_text
        .drain(..)
        .map(|text| json!({ "type": "output_text", "text": text }))
        .collect::<Vec<_>>();
    items.push(json!({
        "type": "message",
        "role": "assistant",
        "content": content,
    }));
}

async fn send_chat_completion_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI Chat Completions request failed",
            "request_send",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions request failed",
            status,
            body,
        ));
    }

    let body = response.text().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI Chat Completions response body failed",
            "response_body",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;

    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI Chat Completions JSON", error))?;

    parse_chat_completion_response(parsed)
}

fn classify_chat_completion_status_error(
    context: &str,
    status: reqwest::StatusCode,
    body: String,
) -> anyhow::Error {
    // Try to parse as OpenAI error response
    if let Ok(error_json) = serde_json::from_str::<Value>(&body) {
        if let Some(error_obj) = error_json.get("error") {
            return classify_openai_chat_completion_error(context, error_obj);
        }
    }

    // Fallback to generic status error classification
    classify_status_error(context, status, body)
}

pub(crate) fn classify_openai_chat_completion_error(context: &str, error: &Value) -> anyhow::Error {
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let error_message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let error_code = error.get("code").and_then(Value::as_str);

    let classification = match error_code {
        Some("rate_limit_exceeded") | Some("rate_limit_exceeded_error") => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            }
        }
        Some("insufficient_quota") | Some("quota_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_api_key") | Some("invalid_authentication") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded") | Some("max_tokens") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_request_error") | Some("invalid_request") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("server_error") | Some("service_unavailable") => ProviderFailureClassification {
            kind: ProviderFailureKind::ServerError,
            disposition: RetryDisposition::Retryable,
        },
        _ => match error_type {
            "rate_limit_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            },
            "invalid_request_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
            "authentication_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::AuthError,
                disposition: RetryDisposition::FailFast,
            },
            "server_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            },
            _ => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
        },
    };

    let detail = if let Some(code) = error_code {
        format!("{}: {}", code, error_message)
    } else {
        format!("{}: {}", error_type, error_message)
    };

    provider_transport_error(
        classification,
        None,
        None,
        format!("{}: {}", context, detail),
    )
}

pub(crate) fn parse_chat_completion_response(response: Value) -> Result<ParsedOpenAiResponse> {
    // Extract response ID
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    // Extract choices array
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI Chat Completions response did not contain choices array",
                "missing choices",
            )
        })?;

    let first_choice = choices.first().ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choices array was empty",
            "empty choices",
        )
    })?;

    // Extract message from first choice
    let message = first_choice.get("message").ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choice did not contain message",
            "missing message",
        )
    })?;

    // Parse message content
    let mut blocks = Vec::new();

    // Extract text content
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            blocks.push(ModelBlock::Text {
                text: content.to_string(),
            });
        }
    }

    // Extract tool calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let id = tool_call.get("id").and_then(Value::as_str).ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain id",
                    "missing tool_call_id",
                )
            })?;

            let function = tool_call.get("function").ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain function",
                    "missing function",
                )
            })?;

            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_response_error(
                        "OpenAI Chat Completions function did not contain name",
                        "missing function_name",
                    )
                })?;

            let arguments_str = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");

            let arguments = if arguments_str.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments_str).map_err(|error| {
                    invalid_response_error("invalid tool call arguments JSON", error)
                })?
            };

            blocks.push(ModelBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: arguments,
            });
        }
    }

    // Allow valid minimal assistant messages that contain neither text nor tool calls.
    // OpenAI Chat Completions can return empty/null content together with a finish_reason.
    // In such cases, we return an empty blocks vector rather than an error.
    if blocks.is_empty() {
        // Check if we have a valid finish_reason before accepting empty blocks
        let finish_reason = first_choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);

        if finish_reason.is_some() {
            // Accept empty response when we have a finish_reason
        } else {
            return Err(invalid_response_error(
                "OpenAI Chat Completions response contained no supported content",
                "empty content",
            ));
        }
    }

    // Extract usage
    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        creation_input_tokens: 0,
    });

    // Extract finish reason
    let stop_reason = first_choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Build output items for continuation tracking
    // Store the complete message object for proper continuation support
    let output_items = vec![message.clone()];

    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason,
            input_tokens: usage
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

pub(crate) async fn send_chat_completion_stream_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI Chat Completions streaming request failed",
            "request_send",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions streaming request failed",
            status,
            body,
        ));
    }

    let response = read_chat_completion_stream(response).await?;
    parse_chat_completion_response(response)
}

async fn read_chat_completion_stream(response: Response) -> Result<Value> {
    const MAX_STREAMED_EVENTS: usize = 128;
    let mut streamed_events = Vec::new();

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI Chat Completions streaming response failed",
            "streaming_response_body",
            "openai",
            None,
            None,
            error,
        )
    })? {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..newline_idx + 1);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
                    match event {
                        ChatCompletionSseEvent::ContentDelta(delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            streamed_events.push(json!({"delta": {"content": delta}}));
                        }
                        ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            // Extract the tool_calls array and store in delta format
                            if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                                streamed_events
                                    .push(json!({"delta": {"tool_calls": tool_calls_array}}));
                            }
                        }
                        ChatCompletionSseEvent::Done => {
                            // Stream ended
                            break;
                        }
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    // Process remaining data
    if !pending.is_empty() {
        let line = pending.trim();
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }

    // Process final event
    if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
        match event {
            ChatCompletionSseEvent::ContentDelta(delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                streamed_events.push(json!({"delta": {"content": delta}}));
            }
            ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                // Extract the tool_calls array and store in delta format
                if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                    streamed_events.push(json!({"delta": {"tool_calls": tool_calls_array}}));
                }
            }
            ChatCompletionSseEvent::Done => {}
        }
    }

    // Accumulate streamed events into final response
    let accumulated = accumulate_chat_completion_stream_events(streamed_events)?;
    Ok(accumulated)
}

fn process_chat_completion_sse_event(
    data_lines: &mut Vec<String>,
) -> Result<Option<ChatCompletionSseEvent>> {
    if data_lines.is_empty() {
        return Ok(None);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed == "[DONE]" {
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    let event: Value = serde_json::from_str(trimmed).map_err(|error| {
        invalid_response_error("invalid Chat Completions streaming JSON", error)
    })?;

    // Check for errors
    if event.get("error").is_some() {
        return Err(invalid_response_error(
            "Chat Completions streaming contained error event",
            "error_in_stream",
        ));
    }

    // Process delta content from choices[0].delta (OpenAI Chat Completions streaming format)
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if let Some(delta) = first_choice.get("delta") {
                // Process content delta
                if let Some(content) = delta.get("content") {
                    if let Some(text) = content.as_str() {
                        return Ok(Some(ChatCompletionSseEvent::ContentDelta(text.to_string())));
                    }
                }

                // Process tool_calls delta - return the entire array for accumulation
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    return Ok(Some(ChatCompletionSseEvent::ToolCallDelta(json!({
                        "tool_calls": tool_calls
                    }))));
                }
            }
        }
    }

    // Check for finish_reason at both top-level and choices[0] level
    if event.get("finish_reason").is_some() {
        // Stream ending event
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    // Also check for finish_reason in choices[0]
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if first_choice.get("finish_reason").is_some() {
                // Stream ending event
                return Ok(Some(ChatCompletionSseEvent::Done));
            }
        }
    }

    Ok(None)
}

pub(crate) fn accumulate_chat_completion_stream_events(events: Vec<Value>) -> Result<Value> {
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = None;

    for event in events {
        // Handle both formats: direct delta and nested choices[0].delta
        let delta = if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            choices.first().and_then(|c| c.get("delta"))
        } else {
            event.get("delta")
        };

        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str) {
            content.push_str(text);
        }

        if let Some(tool_calls_delta) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tool_call_delta in tool_calls_delta {
                let index = tool_call_delta["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(json!({}));
                }

                let tool_call = &mut tool_calls[index];

                if let Some(id) = tool_call_delta.get("id").and_then(Value::as_str) {
                    tool_call["id"] = Value::String(id.to_string());
                }
                if let Some(name) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    tool_call["function"]["name"] = Value::String(name.to_string());
                }
                if let Some(arguments) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                {
                    let current_args = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");

                    let new_args = if let Some(additional) = arguments.as_str() {
                        // If we have existing content, concatenate; otherwise use new content
                        if current_args.is_empty() {
                            additional.to_string()
                        } else {
                            format!("{}{}", current_args, additional)
                        }
                    } else {
                        current_args.to_string()
                    };

                    // Only set if we have arguments
                    if !new_args.is_empty() {
                        tool_call["function"]["arguments"] = Value::String(new_args);
                    }
                }
            }
        }

        // Handle finish_reason in both formats: direct and nested in choices[0]
        if let Some(reason) = event.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(reason.to_string());
        } else if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            if let Some(first_choice) = choices.first() {
                if let Some(reason) = first_choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = Some(reason.to_string());
                }
            }
        }
    }

    // Build accumulated response
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(json!({
        "id": "chatcmpl-stream",
        "choices": [{
            "message": message,
            "finish_reason": finish_reason.unwrap_or("stop".to_string())
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    }))
}

enum ChatCompletionSseEvent {
    ContentDelta(String),
    ToolCallDelta(Value),
    Done,
}

async fn send_openai_responses_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI-style request failed",
            "request_send",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_status_error(
            "OpenAI-style request failed",
            status,
            body,
        ));
    }

    let body = response.text().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI-style response body failed",
            "response_body",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI-style JSON", error))?;
    parse_openai_response_with_transport_state(parsed)
}

async fn send_openai_responses_streaming_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI-style streaming request failed",
            "streaming_request_send",
            "openai-codex",
            Some(&provider_model_ref("openai-codex", &body)),
            Some(url.as_str()),
            error,
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_status_error(
            "OpenAI-style streaming request failed",
            status,
            body,
        ));
    }

    let terminal_response = read_openai_streaming_response(response).await?;
    parse_openai_response_with_transport_state(terminal_response)
}

async fn read_openai_streaming_response(response: Response) -> Result<Value> {
    const MAX_STREAMED_OUTPUT_ITEMS: usize = 128;

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();
    let mut streamed_output_items = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI-style streaming response body failed",
            "streaming_response_body",
            "openai-codex",
            None,
            None,
            error,
        )
    })? {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..=newline_idx);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                match consume_openai_sse_event(&mut data_lines)? {
                    StreamingSseEvent::Continue => {}
                    StreamingSseEvent::OutputItem(item) => push_streamed_output_item(
                        &mut streamed_output_items,
                        item,
                        MAX_STREAMED_OUTPUT_ITEMS,
                    )?,
                    StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
                    StreamingSseEvent::Terminal(response) => {
                        return Ok(finalize_openai_terminal_response(
                            response,
                            &streamed_output_items,
                        ))
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    if !pending.is_empty() {
        let line = pending.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    match consume_openai_sse_event(&mut data_lines)? {
        StreamingSseEvent::Continue => {}
        StreamingSseEvent::OutputItem(item) => {
            push_streamed_output_item(&mut streamed_output_items, item, MAX_STREAMED_OUTPUT_ITEMS)?
        }
        StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
        StreamingSseEvent::Terminal(response) => {
            return Ok(finalize_openai_terminal_response(
                response,
                &streamed_output_items,
            ))
        }
    }

    Err(invalid_response_error(
        "OpenAI-style streaming response did not contain a terminal response event",
        "missing terminal response",
    ))
}

fn model_from_request(body: &Value) -> &str {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn provider_model_ref(provider: &str, body: &Value) -> String {
    format!("{provider}/{}", model_from_request(body))
}

fn early_done_protocol_violation_error() -> anyhow::Error {
    invalid_response_error(
        "OpenAI-style streaming response ended before a terminal response event",
        "[DONE] observed before terminal response",
    )
}

enum StreamingSseEvent {
    Continue,
    OutputItem(Value),
    Done,
    Terminal(Value),
}

fn push_streamed_output_item(
    streamed_output_items: &mut Vec<Value>,
    item: Value,
    max_items: usize,
) -> Result<()> {
    if streamed_output_items.len() >= max_items {
        return Err(invalid_response_error(
            "OpenAI-style streaming response emitted too many output items",
            format!("received more than {max_items} streamed output items"),
        ));
    }
    streamed_output_items.push(item);
    Ok(())
}

fn consume_openai_sse_event(data_lines: &mut Vec<String>) -> Result<StreamingSseEvent> {
    if data_lines.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }
    if trimmed == "[DONE]" {
        return Ok(StreamingSseEvent::Done);
    }

    let event: Value = serde_json::from_str(trimmed)
        .map_err(|error| invalid_response_error("invalid OpenAI-style streaming JSON", error))?;

    if event.get("type").and_then(Value::as_str) == Some("error") {
        return Err(classify_openai_streaming_error(
            "OpenAI-style streaming response reported an error event",
            event.get("error"),
            Some(&event),
        ));
    }

    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "response.output_item.done" {
        if let Some(item) = event.get("item") {
            return Ok(StreamingSseEvent::OutputItem(item.clone()));
        }
        return Err(invalid_response_error(
            "OpenAI-style streaming response output item event was missing item",
            "missing item",
        ));
    }
    if let Some(response) = event.get("response") {
        let status = response.get("status").and_then(Value::as_str);
        if event_type == "response.completed" || status == Some("completed") {
            return Ok(StreamingSseEvent::Terminal(response.clone()));
        }
        if event_type == "response.failed" || status == Some("failed") {
            return Err(classify_openai_streaming_error(
                "OpenAI-style streaming response failed",
                response.get("error"),
                Some(response),
            ));
        }
        if event_type == "response.incomplete" || matches!(status, Some("incomplete" | "cancelled"))
        {
            return Err(classify_openai_incomplete_response(response));
        }
    }

    Ok(StreamingSseEvent::Continue)
}

fn finalize_openai_terminal_response(
    mut response: Value,
    streamed_output_items: &[Value],
) -> Value {
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output || streamed_output_items.is_empty() {
        return response;
    }

    if let Some(object) = response.as_object_mut() {
        object.insert(
            "output".to_string(),
            Value::Array(streamed_output_items.to_vec()),
        );
    }
    response
}

fn classify_openai_streaming_error(
    context: &str,
    error: Option<&Value>,
    response: Option<&Value>,
) -> anyhow::Error {
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .and_then(|response| response.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown streaming error");
    let classification = match code {
        Some("rate_limit_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
        },
        Some("server_is_overloaded" | "slow_down") => ProviderFailureClassification {
            kind: ProviderFailureKind::ServerError,
            disposition: RetryDisposition::Retryable,
        },
        Some("insufficient_quota") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded" | "invalid_prompt") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        _ => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
    };
    let detail = code
        .map(|code| format!("{code}: {message}"))
        .unwrap_or_else(|| message.to_string());
    provider_transport_error(classification, None, None, format!("{context}: {detail}"))
}

fn classify_openai_incomplete_response(response: &Value) -> anyhow::Error {
    let reason = response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        .or_else(|| response.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("OpenAI-style streaming response did not complete successfully: {reason}"),
    )
}

#[allow(dead_code)]
pub(crate) fn parse_openai_response(response: Value) -> Result<ProviderTurnResponse> {
    parse_openai_response_with_transport_state(response).map(|parsed| parsed.response)
}

fn parse_openai_response_with_transport_state(response: Value) -> Result<ParsedOpenAiResponse> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI-style response did not contain an output array",
                "missing output array",
            )
        })?;
    let output_items = output.clone();
    let mut blocks = Vec::new();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for content_item in content {
                        match content_item.get("type").and_then(Value::as_str) {
                            Some("output_text") | Some("text") | Some("input_text") => {
                                if let Some(text) = content_item.get("text").and_then(Value::as_str)
                                {
                                    blocks.push(ModelBlock::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "function_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "function_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = match item.get("arguments") {
                    Some(Value::String(arguments)) if !arguments.trim().is_empty() => {
                        serde_json::from_str(arguments).map_err(|error| {
                            invalid_response_error("invalid function_call arguments", error)
                        })?
                    }
                    Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
                    _ => json!({}),
                };
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input,
                });
            }
            Some("custom_tool_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "custom_tool_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = item.get("input").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include string input",
                        "missing input",
                    )
                })?;
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: Value::String(input.to_string()),
                });
            }
            _ => {}
        }
    }

    if blocks.is_empty() {
        return Err(invalid_response_error(
            "OpenAI-style response contained no supported content blocks",
            "empty supported block set",
        ));
    }

    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0),
        creation_input_tokens: 0,
    });
    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason: response
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    response
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            input_tokens: usage
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

#[allow(dead_code)]
fn unsupported_streaming_transport_error(provider_name: &str) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::UnsupportedTransport,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("{provider_name} requires a streaming transport contract"),
    )
}

#[cfg(test)]
mod tests {
    use super::chat_completions_url;

    #[test]
    fn chat_completions_url_accepts_openai_compatible_base_urls() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://qianfan.baidubce.com/v2"),
            "https://qianfan.baidubce.com/v2/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://ark.ap-southeast.bytepluses.com/api/v3"),
            "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://proxy.example/chat/completions"),
            "https://proxy.example/chat/completions"
        );
    }
}
