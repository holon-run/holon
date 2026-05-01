use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    auth::load_codex_cli_credential,
    config::{AppConfig, CredentialKind, CredentialSource, ProviderId, ProviderRuntimeConfig},
    provider::{
        emitted_tool_json_schema, AgentProvider, ConversationMessage, ModelBlock,
        ProviderCacheUsage, ProviderIncrementalContinuationDiagnostics,
        ProviderOpenAiRemoteCompactionDiagnostics, ProviderOpenAiRequestControlsDiagnostics,
        ProviderPromptFrame, ProviderRequestDiagnostics, ProviderTurnRequest, ProviderTurnResponse,
        ToolSchemaContract,
    },
};

use super::build_http_client;
use crate::provider::retry::{
    classify_reqwest_transport_error, classify_status_error, invalid_response_error,
    provider_transport_error, ProviderFailureClassification, ProviderFailureKind,
    ProviderTransportError, RetryDisposition,
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
    reasoning_effort: Option<String>,
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
}

const OPENAI_REMOTE_COMPACTION_TRIGGER_ITEMS: usize = 8;
const OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND: &str = "responses_compact";

#[derive(Debug, Default)]
struct OpenAiContinuationState {
    windows: HashMap<OpenAiContinuationScope, OpenAiProviderWindow>,
    unsupported_compact_endpoints: HashMap<String, u16>,
    next_generation: u64,
}

fn openai_responses_url(base_url: &str) -> String {
    format!("{}/responses", base_url.trim_end_matches('/'))
}

fn openai_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", base_url.trim_end_matches('/'))
}

fn openai_codex_api_base_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/codex") {
        base_url.to_string()
    } else {
        format!("{base_url}/codex")
    }
}

fn openai_codex_responses_url(base_url: &str) -> String {
    format!("{}/responses", openai_codex_api_base_url(base_url))
}

fn openai_codex_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", openai_codex_api_base_url(base_url))
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct OpenAiContinuationScope {
    agent_id: String,
    prompt_cache_key: String,
}

#[derive(Debug, Clone)]
struct OpenAiProviderWindow {
    response_id: Option<String>,
    request_shape: OpenAiRequestShape,
    items: Vec<Value>,
    append_match_items: Vec<Value>,
    latest_compaction_index: Option<usize>,
    generation: u64,
}

#[derive(Debug, Clone)]
struct OpenAiCompactionCandidate {
    items: Vec<Value>,
    retained_tail: Vec<Value>,
    latest_compaction_index: Option<usize>,
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
    append_match_input: Vec<Value>,
    provider_input: Vec<Value>,
    request_shape: OpenAiRequestShape,
    diagnostics: ProviderRequestDiagnostics,
}

#[derive(Debug, Clone, Default)]
struct OpenAiContinuationMismatchDiagnostics {
    expected_prefix_items: usize,
    first_mismatch_index: Option<usize>,
    previous_item_type: Option<String>,
    current_item_type: Option<String>,
    previous_item_id: Option<String>,
    current_item_id: Option<String>,
    previous_item_hash: Option<String>,
    current_item_hash: Option<String>,
    request_shape_hash: Option<String>,
    first_mismatch_path: Option<String>,
    mismatch_kind: Option<String>,
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
            reasoning_effort: provider_config.reasoning_effort.clone(),
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
            None,
        )?;
        let mut plan = plan_openai_responses_request(body, &request, &self.continuation, true)?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            &self.client,
            openai_responses_compact_url(&self.base_url),
            headers.clone(),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let parsed = match send_openai_responses_request(
            &self.client,
            openai_responses_url(&self.base_url),
            plan.body,
            headers.clone(),
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
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            plan.provider_input,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                &self.client,
                openai_responses_compact_url(&self.base_url),
                headers,
            )
            .await;
        }
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
            self.reasoning_effort.as_deref(),
        )?;
        let mut plan = plan_openai_responses_request(body, &request, &self.continuation, false)?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let headers = vec![
            (
                "authorization",
                format!("Bearer {}", credential.access_token),
            ),
            ("chatgpt-account-id", credential.account_id.clone()),
            ("OpenAI-Beta", "responses=experimental".to_string()),
            ("originator", self.originator.clone()),
        ];
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            &self.client,
            openai_codex_responses_compact_url(&self.base_url),
            headers.clone(),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let parsed = match send_openai_responses_streaming_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            plan.body,
            headers.clone(),
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
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            plan.provider_input,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                &self.client,
                openai_codex_responses_compact_url(&self.base_url),
                headers,
            )
            .await;
        }
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
            plan.append_match_input,
            plan.provider_input,
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
    let full_body = build_chat_completion_request(
        model,
        max_output_tokens,
        request,
        tool_schema_contract,
        stream,
    )?;

    let body_messages = full_body
        .get("messages")
        .and_then(|messages| messages.as_array())
        .cloned()
        .unwrap_or_default();

    // Calculate continuation scope
    let scope = continuation_scope(request);
    let full_messages = body_messages;
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
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "missing_continuation_scope",
                    None,
                    full_message_count,
                    None,
                    None,
                ),
            },
        ));
    };

    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();

    let Some(previous) = previous else {
        // No previous state - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "not_applicable_initial_request",
                    None,
                    full_message_count,
                    None,
                    None,
                ),
            },
        ));
    };

    // Check if request shape changed
    if previous.request_shape != request_shape {
        // Request changed - send full request
        let request_shape_hash = request_shape_hash(&request_shape);
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "request_shape_changed",
                    None,
                    full_message_count,
                    Some(OpenAiContinuationMismatchDiagnostics {
                        request_shape_hash: Some(request_shape_hash),
                        ..OpenAiContinuationMismatchDiagnostics::default()
                    }),
                    None,
                ),
            },
        ));
    }

    // Chat Completions continuation currently cannot safely reconstruct an
    // assistant message from the provider window for prefix matching.
    // `full_messages` contains message objects, but `response_output` is not
    // guaranteed to store a comparable message value, so incremental
    // continuation would be unreliable here. Send the full request instead.
    let request_shape_hash = request_shape_hash(&request_shape);
    return Ok((
        full_body.clone(),
        OpenAiRequestPlan {
            body: full_body,
            scope,
            append_match_input: full_messages.clone(),
            provider_input: full_messages,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "chat_completions_incremental_continuation_unsupported",
                None,
                full_message_count,
                Some(OpenAiContinuationMismatchDiagnostics {
                    request_shape_hash: Some(request_shape_hash),
                    ..OpenAiContinuationMismatchDiagnostics::default()
                }),
                None,
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
    reasoning_effort: Option<&str>,
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
            if let Some(reasoning_effort) = reasoning_effort {
                body["reasoning"] = json!({ "effort": reasoning_effort });
                body["include"] = json!(["reasoning.encrypted_content"]);
            } else {
                body["reasoning"] = Value::Null;
                body["include"] = Value::Array(Vec::new());
            }
        }
    }
    Ok(body)
}

fn openai_request_controls_diagnostics(body: &Value) -> ProviderOpenAiRequestControlsDiagnostics {
    let reasoning_effort = body
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let include_reasoning_encrypted_content = body
        .get("include")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        });
    let max_output_tokens_sent = body.get("max_output_tokens").is_some();
    let codex_streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    ProviderOpenAiRequestControlsDiagnostics {
        reasoning_sent: reasoning_effort.is_some(),
        reasoning_effort,
        include_reasoning_encrypted_content,
        max_output_tokens_sent,
        max_output_tokens_unsupported: codex_streaming,
    }
}

fn plan_openai_responses_request(
    mut body: Value,
    request: &ProviderTurnRequest,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    allow_previous_response_id: bool,
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
    let append_match_input = openai_append_match_input_items(&full_input);
    let request_shape = request_shape_without_input(&body, request);
    let scope = continuation_scope(request);
    let request_controls = Some(openai_request_controls_diagnostics(&body));
    let Some(scope_ref) = scope.as_ref() else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "missing_continuation_scope",
                None,
                full_input_items,
                None,
                request_controls,
            ),
        });
    };
    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();
    let Some(previous) = previous else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "not_applicable_initial_request",
                None,
                full_input_items,
                None,
                request_controls,
            ),
        });
    };

    if previous.request_shape != request_shape {
        let request_shape_hash = request_shape_hash(&request_shape);
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "request_shape_changed",
                None,
                full_input_items,
                Some(OpenAiContinuationMismatchDiagnostics {
                    request_shape_hash: Some(request_shape_hash),
                    ..OpenAiContinuationMismatchDiagnostics::default()
                }),
                request_controls,
            ),
        });
    }

    let expected_prefix = previous.append_match_items.clone();
    let mismatch = openai_continuation_mismatch_diagnostics(
        &expected_prefix,
        &append_match_input,
        &request_shape,
    );
    if expected_prefix.is_empty()
        || append_match_input.len() <= expected_prefix.len()
        || !append_match_input.starts_with(&expected_prefix)
    {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "conversation_not_strict_append_only",
                None,
                full_input_items,
                Some(mismatch),
                request_controls,
            ),
        });
    }

    let incremental_input = full_input[expected_prefix.len()..].to_vec();
    let response_id = allow_previous_response_id
        .then(|| previous.response_id.clone())
        .flatten();
    let has_response_id = response_id.is_some();
    let replay_is_compacted = previous.latest_compaction_index.is_some();
    let provider_input = if let Some(response_id) = response_id {
        body["input"] = Value::Array(incremental_input.clone());
        body["previous_response_id"] = Value::String(response_id);
        incremental_input.clone()
    } else {
        let mut provider_input = previous.items.clone();
        provider_input.extend(incremental_input.clone());
        body["input"] = Value::Array(provider_input.clone());
        provider_input
    };
    let request_shape_hash = request_shape_hash(&request_shape);
    Ok(OpenAiRequestPlan {
        body,
        scope,
        append_match_input,
        provider_input,
        request_shape,
        diagnostics: ProviderRequestDiagnostics {
            request_lowering_mode: openai_append_match_lowering_mode(
                has_response_id,
                replay_is_compacted,
            ),
            anthropic_cache: None,
            anthropic_context_management: None,
            openai_request_controls: request_controls,
            openai_remote_compaction: None,
            incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
                status: "hit".into(),
                fallback_reason: None,
                incremental_input_items: Some(incremental_input.len()),
                full_input_items: Some(full_input_items),
                expected_prefix_items: Some(expected_prefix.len()),
                first_mismatch_index: None,
                previous_item_type: None,
                current_item_type: None,
                previous_item_id: None,
                current_item_id: None,
                previous_item_hash: None,
                current_item_hash: None,
                request_shape_hash: Some(request_shape_hash),
                first_mismatch_path: None,
                mismatch_kind: None,
            }),
        },
    })
}

fn openai_append_match_lowering_mode(has_response_id: bool, replay_is_compacted: bool) -> String {
    if has_response_id {
        "incremental_continuation".into()
    } else if replay_is_compacted {
        "provider_window_compacted".into()
    } else {
        "provider_window_replay".into()
    }
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

fn openai_continuation_mismatch_diagnostics(
    expected_prefix: &[Value],
    full_input: &[Value],
    request_shape: &OpenAiRequestShape,
) -> OpenAiContinuationMismatchDiagnostics {
    let first_mismatch_index = expected_prefix
        .iter()
        .zip(full_input.iter())
        .position(|(previous, current)| previous != current)
        .or_else(|| {
            (expected_prefix.len() != full_input.len())
                .then_some(expected_prefix.len().min(full_input.len()))
        });
    let previous = first_mismatch_index.and_then(|index| expected_prefix.get(index));
    let current = first_mismatch_index.and_then(|index| full_input.get(index));
    let item_path = match (first_mismatch_index, previous, current) {
        (Some(index), Some(previous), Some(current)) => {
            let suffix = first_json_mismatch_path(previous, current).unwrap_or_default();
            Some(format!("/{index}{suffix}"))
        }
        (Some(index), _, _) => Some(format!("/{index}")),
        _ => None,
    };
    OpenAiContinuationMismatchDiagnostics {
        expected_prefix_items: expected_prefix.len(),
        first_mismatch_index,
        previous_item_type: previous.map(openai_item_type),
        current_item_type: current.map(openai_item_type),
        previous_item_id: previous.and_then(openai_item_stable_id),
        current_item_id: current.and_then(openai_item_stable_id),
        previous_item_hash: previous.map(openai_item_hash),
        current_item_hash: current.map(openai_item_hash),
        request_shape_hash: Some(request_shape_hash(request_shape)),
        first_mismatch_path: item_path.clone(),
        mismatch_kind: Some(openai_mismatch_kind(
            previous,
            current,
            item_path.as_deref(),
        )),
    }
}

fn first_json_mismatch_path(previous: &Value, current: &Value) -> Option<String> {
    if previous == current {
        return None;
    }
    match (previous, current) {
        (Value::Array(previous), Value::Array(current)) => {
            let shared = previous.len().min(current.len());
            for index in 0..shared {
                if let Some(path) = first_json_mismatch_path(&previous[index], &current[index]) {
                    return Some(format!("/{index}{path}"));
                }
            }
            Some(format!("/{shared}"))
        }
        (Value::Object(previous), Value::Object(current)) => {
            let keys = previous
                .keys()
                .chain(current.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                match (previous.get(key), current.get(key)) {
                    (Some(previous), Some(current)) => {
                        if let Some(path) = first_json_mismatch_path(previous, current) {
                            return Some(format!("/{}{}", json_pointer_escape(key), path));
                        }
                    }
                    _ => return Some(format!("/{}", json_pointer_escape(key))),
                }
            }
            Some(String::new())
        }
        _ => Some(String::new()),
    }
}

fn json_pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn openai_mismatch_kind(
    previous: Option<&Value>,
    current: Option<&Value>,
    path: Option<&str>,
) -> String {
    let Some(previous) = previous else {
        return "length_mismatch".into();
    };
    let Some(current) = current else {
        return "length_mismatch".into();
    };
    let previous_type = openai_item_type(previous);
    let current_type = openai_item_type(current);
    if previous_type != current_type {
        return "semantic_mismatch".into();
    }
    let path = path.unwrap_or_default();
    if path.contains("/id")
        || path.contains("/status")
        || path.contains("/metadata")
        || path.contains("/annotations")
        || path.contains("/logprobs")
    {
        return "provider_metadata_only".into();
    }
    match previous_type.as_str() {
        "message" => {
            if previous.get("role").and_then(Value::as_str) == Some("assistant")
                && path.contains("/content")
                && !path.ends_with("/text")
            {
                "assistant_text_shape".into()
            } else {
                "semantic_mismatch".into()
            }
        }
        "function_call" | "custom_tool_call" => "tool_call_shape".into(),
        "function_call_output" | "custom_tool_call_output" => "tool_result_shape".into(),
        _ => "semantic_mismatch".into(),
    }
}

fn openai_item_type(item: &Value) -> String {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| match item {
            Value::Object(_) => "object",
            Value::Array(_) => "array",
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        })
        .to_string()
}

fn openai_item_stable_id(item: &Value) -> Option<String> {
    if let Some(id) = ["id", "call_id"]
        .into_iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
    {
        return Some(id.to_string());
    }
    item.get("role").and_then(Value::as_str).map(|role| {
        let item_type = openai_item_type(item);
        format!("{item_type}:{role}")
    })
}

fn openai_item_hash(item: &Value) -> String {
    sha256_hex(canonical_json(item).as_bytes())
}

fn latest_openai_compaction_index(items: &[Value]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| openai_is_compaction_item(item).then_some(index))
}

fn openai_is_compaction_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}

fn request_shape_hash(request_shape: &OpenAiRequestShape) -> String {
    let value = json!({
        "wire_shape": request_shape.wire_shape,
        "prompt_frame": request_shape.prompt_frame,
    });
    sha256_hex(canonical_json(&value).as_bytes())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(values) => {
            let items = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let ordered = map
                .iter()
                .map(|(key, value)| (key, value))
                .collect::<BTreeMap<_, _>>();
            let items = ordered
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", items.join(","))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn incremental_diagnostics(
    request_lowering_mode: &str,
    fallback_reason: &str,
    incremental_input_items: Option<usize>,
    full_input_items: usize,
    mismatch: Option<OpenAiContinuationMismatchDiagnostics>,
    openai_request_controls: Option<ProviderOpenAiRequestControlsDiagnostics>,
) -> ProviderRequestDiagnostics {
    let mismatch = mismatch.unwrap_or_default();
    ProviderRequestDiagnostics {
        request_lowering_mode: request_lowering_mode.into(),
        anthropic_cache: None,
        anthropic_context_management: None,
        openai_request_controls,
        openai_remote_compaction: None,
        incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
            status: "fallback_full_request".into(),
            fallback_reason: Some(fallback_reason.into()),
            incremental_input_items,
            full_input_items: Some(full_input_items),
            expected_prefix_items: Some(mismatch.expected_prefix_items),
            first_mismatch_index: mismatch.first_mismatch_index,
            previous_item_type: mismatch.previous_item_type,
            current_item_type: mismatch.current_item_type,
            previous_item_id: mismatch.previous_item_id,
            current_item_id: mismatch.current_item_id,
            previous_item_hash: mismatch.previous_item_hash,
            current_item_hash: mismatch.current_item_hash,
            request_shape_hash: mismatch.request_shape_hash,
            first_mismatch_path: mismatch.first_mismatch_path,
            mismatch_kind: mismatch.mismatch_kind,
        }),
    }
}

fn update_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<OpenAiContinuationScope>,
    request_shape: OpenAiRequestShape,
    append_match_input: Vec<Value>,
    provider_input: Vec<Value>,
    parsed: &ParsedOpenAiResponse,
) {
    let Some(scope) = scope else {
        return;
    };
    let mut state = lock_openai_continuation(continuation);
    let next = match (parsed.response_id.as_ref(), parsed.output_items.is_empty()) {
        (Some(response_id), false) => {
            state.next_generation = state.next_generation.saturating_add(1);
            let mut items = provider_input
                .into_iter()
                .map(|item| canonicalize_openai_provider_item(&item))
                .collect::<Vec<_>>();
            items.extend(
                parsed
                    .output_items
                    .iter()
                    .map(canonicalize_openai_provider_item),
            );
            let mut append_match_items = append_match_input;
            append_match_items.extend(openai_append_match_output_items(&parsed.output_items));
            Some(OpenAiProviderWindow {
                response_id: Some(response_id.clone()),
                request_shape,
                latest_compaction_index: latest_openai_compaction_index(&items),
                items,
                append_match_items,
                generation: state.next_generation,
            })
        }
        _ => None,
    };
    if let Some(next) = next {
        state.windows.insert(scope, next);
    } else {
        state.windows.remove(&scope);
    }
}

fn openai_append_match_output_items(output_items: &[Value]) -> Vec<Value> {
    output_items
        .iter()
        .filter_map(openai_append_match_output_item)
        .collect()
}

fn openai_append_match_output_item(item: &Value) -> Option<Value> {
    if matches!(item.get("type").and_then(Value::as_str), Some("reasoning")) {
        None
    } else {
        Some(canonicalize_openai_append_match_item(item))
    }
}

async fn maybe_compact_openai_provider_window(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&OpenAiContinuationScope>,
    request_shape: &OpenAiRequestShape,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    let Some(scope) = scope else {
        return None;
    };
    let window = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    let candidate = match openai_provider_window_compaction_candidate(&window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            if skip_reason == "below_item_threshold" {
                return None;
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some("provider_window_item_threshold".into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: window.latest_compaction_index,
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(request_shape)),
                continuation_generation: Some(window.generation),
                error: None,
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            "provider_window_item_threshold",
            input_items,
            candidate.latest_compaction_index,
            Some(request_shape_hash),
            Some(window.generation),
            http_status,
            None,
        ));
    }
    let compact_body = build_openai_compact_request_body(request_shape, &candidate.items);
    let compacted =
        match send_openai_compact_request(client, compact_url.clone(), compact_body, headers).await
        {
            Ok(compacted) => compacted,
            Err(error) => {
                if is_non_persisted_compact_item_id_error(&error) {
                    return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                        status: "invalid_non_persisted_item_id".into(),
                        trigger_reason: Some("provider_window_item_threshold".into()),
                        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                        http_status: error_status(&error),
                        input_items: Some(input_items),
                        output_items: None,
                        compaction_items: None,
                        latest_compaction_index: candidate.latest_compaction_index,
                        encrypted_content_hashes: None,
                        encrypted_content_bytes: None,
                        request_shape_hash: Some(request_shape_hash),
                        continuation_generation: Some(window.generation),
                        error: Some(error.to_string()),
                    });
                }
                if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                    mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                    return Some(openai_compact_unsupported_diagnostics(
                        "unsupported_endpoint",
                        "provider_window_item_threshold",
                        input_items,
                        candidate.latest_compaction_index,
                        Some(request_shape_hash),
                        Some(window.generation),
                        http_status,
                        Some(error.to_string()),
                    ));
                }
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "failed".into(),
                    trigger_reason: Some("provider_window_item_threshold".into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(window.generation),
                    error: Some(error.to_string()),
                });
            }
        };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some("provider_window_item_threshold".into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some("provider_window_item_threshold".into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        });
    }

    let output_items = compacted.len();
    let generation = {
        let mut state = lock_openai_continuation(continuation);
        let current_generation = state.windows.get(scope).map(|current| current.generation);
        if current_generation != Some(window.generation) {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "stale_generation".into(),
                trigger_reason: Some("provider_window_item_threshold".into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(input_items),
                output_items: Some(output_items),
                compaction_items: Some(compaction_items),
                latest_compaction_index,
                encrypted_content_hashes: Some(encrypted_content_hashes),
                encrypted_content_bytes: Some(encrypted_content_bytes),
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: current_generation,
                error: Some(format!(
                    "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                    window.generation
                )),
            });
        }
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        let mut items = openai_compacted_replay_items(&compacted);
        items.extend(candidate.retained_tail.clone());
        let latest_compaction_index = latest_openai_compaction_index(&items);
        state.windows.insert(
            scope.clone(),
            OpenAiProviderWindow {
                response_id: None,
                request_shape: request_shape.clone(),
                items,
                append_match_items: window.append_match_items,
                latest_compaction_index,
                generation,
            },
        );
        generation
    };

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some("provider_window_item_threshold".into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(generation),
        error: None,
    })
}

async fn maybe_compact_openai_request_plan(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    plan: &mut OpenAiRequestPlan,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    if plan.diagnostics.request_lowering_mode != "incremental_continuation" {
        return None;
    }
    let scope = plan.scope.as_ref()?;
    let previous = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    previous.response_id.as_ref()?;

    let mut compactable_items = previous.items.clone();
    compactable_items.extend(plan.provider_input.clone());
    let compactable_window = OpenAiProviderWindow {
        response_id: None,
        request_shape: plan.request_shape.clone(),
        latest_compaction_index: latest_openai_compaction_index(&compactable_items),
        items: compactable_items,
        append_match_items: plan.append_match_input.clone(),
        generation: previous.generation,
    };
    let candidate = match openai_provider_window_compaction_candidate(&compactable_window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            if skip_reason == "below_item_threshold" {
                return None;
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some("provider_window_item_threshold_before_request".into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(compactable_window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: compactable_window.latest_compaction_index,
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(&plan.request_shape)),
                continuation_generation: Some(previous.generation),
                error: None,
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(&plan.request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            "provider_window_item_threshold_before_request",
            input_items,
            candidate.latest_compaction_index,
            Some(request_shape_hash),
            Some(previous.generation),
            http_status,
            None,
        ));
    }
    let compact_body = build_openai_compact_request_body(&plan.request_shape, &candidate.items);
    let compacted =
        match send_openai_compact_request(client, compact_url.clone(), compact_body, headers).await
        {
            Ok(compacted) => compacted,
            Err(error) => {
                if is_non_persisted_compact_item_id_error(&error) {
                    return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                        status: "invalid_non_persisted_item_id".into(),
                        trigger_reason: Some(
                            "provider_window_item_threshold_before_request".into(),
                        ),
                        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                        http_status: error_status(&error),
                        input_items: Some(input_items),
                        output_items: None,
                        compaction_items: None,
                        latest_compaction_index: candidate.latest_compaction_index,
                        encrypted_content_hashes: None,
                        encrypted_content_bytes: None,
                        request_shape_hash: Some(request_shape_hash),
                        continuation_generation: Some(previous.generation),
                        error: Some(error.to_string()),
                    });
                }
                if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                    mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                    return Some(openai_compact_unsupported_diagnostics(
                        "unsupported_endpoint",
                        "provider_window_item_threshold_before_request",
                        input_items,
                        candidate.latest_compaction_index,
                        Some(request_shape_hash),
                        Some(previous.generation),
                        http_status,
                        Some(error.to_string()),
                    ));
                }
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "failed".into(),
                    trigger_reason: Some("provider_window_item_threshold_before_request".into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(previous.generation),
                    error: Some(error.to_string()),
                });
            }
        };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some("provider_window_item_threshold_before_request".into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some("provider_window_item_threshold_before_request".into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        });
    }

    let current_generation = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).map(|current| current.generation)
    };
    if current_generation != Some(previous.generation) {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "stale_generation".into(),
            trigger_reason: Some("provider_window_item_threshold_before_request".into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(compaction_items),
            latest_compaction_index,
            encrypted_content_hashes: Some(encrypted_content_hashes),
            encrypted_content_bytes: Some(encrypted_content_bytes),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: current_generation,
            error: Some(format!(
                "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                previous.generation
            )),
        });
    }

    let output_items = compacted.len();
    let mut provider_input = openai_compacted_replay_items(&compacted);
    provider_input.extend(candidate.retained_tail.clone());
    plan.body["input"] = Value::Array(provider_input.clone());
    if let Some(object) = plan.body.as_object_mut() {
        object.remove("previous_response_id");
    }
    plan.provider_input = provider_input;
    plan.diagnostics.request_lowering_mode = "provider_window_compacted".into();

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some("provider_window_item_threshold_before_request".into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(previous.generation),
        error: None,
    })
}

fn compact_endpoint_unsupported_status(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
) -> Option<u16> {
    let state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .get(compact_url)
        .copied()
}

fn mark_compact_endpoint_unsupported(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
    http_status: u16,
) {
    let mut state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .insert(compact_url.to_string(), http_status);
}

fn unsupported_compact_endpoint_status(error: &anyhow::Error) -> Option<u16> {
    if is_non_persisted_compact_item_id_error(error) {
        return None;
    }
    let status = error_status(error)?;
    match status {
        404 | 405 | 410 | 501 => Some(status),
        _ => None,
    }
}

fn is_non_persisted_compact_item_id_error(error: &anyhow::Error) -> bool {
    error_status(error) == Some(404)
        && error
            .to_string()
            .contains("Items are not persisted when `store` is set to false")
}

fn error_status(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.status)
}

fn openai_compact_unsupported_diagnostics(
    status: &str,
    trigger_reason: &str,
    input_items: usize,
    latest_compaction_index: Option<usize>,
    request_shape_hash: Option<String>,
    continuation_generation: Option<u64>,
    http_status: u16,
    error: Option<String>,
) -> ProviderOpenAiRemoteCompactionDiagnostics {
    ProviderOpenAiRemoteCompactionDiagnostics {
        status: status.into(),
        trigger_reason: Some(trigger_reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: Some(http_status),
        input_items: Some(input_items),
        output_items: None,
        compaction_items: None,
        latest_compaction_index,
        encrypted_content_hashes: None,
        encrypted_content_bytes: None,
        request_shape_hash,
        continuation_generation,
        error,
    }
}

fn openai_provider_window_compaction_candidate(
    window: &OpenAiProviderWindow,
) -> std::result::Result<OpenAiCompactionCandidate, &'static str> {
    if items_since_latest_openai_compaction(&window.items) < OPENAI_REMOTE_COMPACTION_TRIGGER_ITEMS
    {
        return Err("below_item_threshold");
    }

    let boundary =
        latest_complete_openai_tool_call_boundary(&window.items).ok_or("unpaired_tool_call")?;
    debug_assert!(boundary > 0);

    let compact_items = window.items[..boundary].to_vec();
    if items_since_latest_openai_compaction(&compact_items) < OPENAI_REMOTE_COMPACTION_TRIGGER_ITEMS
    {
        return Err("unpaired_tool_call");
    }
    if has_unpaired_openai_tool_call(&compact_items) {
        return Err("unpaired_tool_call");
    }

    Ok(OpenAiCompactionCandidate {
        latest_compaction_index: latest_openai_compaction_index(&compact_items),
        items: compact_items,
        retained_tail: window.items[boundary..].to_vec(),
    })
}

fn openai_compacted_replay_items(compacted: &[Value]) -> Vec<Value> {
    latest_openai_compaction_index(compacted)
        .map(|index| compacted[index..].to_vec())
        .unwrap_or_else(|| compacted.to_vec())
}

fn items_since_latest_openai_compaction(items: &[Value]) -> usize {
    latest_openai_compaction_index(items)
        .map(|index| items.len().saturating_sub(index + 1))
        .unwrap_or(items.len())
}

fn latest_complete_openai_tool_call_boundary(items: &[Value]) -> Option<usize> {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();
    let mut latest_complete_boundary = None;

    for (index, item) in items.iter().enumerate() {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            if openai_tool_call_sets_are_complete(
                &function_calls,
                &function_outputs,
                &custom_tool_calls,
                &custom_tool_outputs,
            ) {
                latest_complete_boundary = Some(index + 1);
            }
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
            }
            _ => {}
        }
        if openai_tool_call_sets_are_complete(
            &function_calls,
            &function_outputs,
            &custom_tool_calls,
            &custom_tool_outputs,
        ) {
            latest_complete_boundary = Some(index + 1);
        }
    }

    latest_complete_boundary
}

fn openai_tool_call_sets_are_complete(
    function_calls: &HashSet<String>,
    function_outputs: &HashSet<String>,
    custom_tool_calls: &HashSet<String>,
    custom_tool_outputs: &HashSet<String>,
) -> bool {
    function_calls.is_subset(function_outputs) && custom_tool_calls.is_subset(custom_tool_outputs)
}

fn has_unpaired_openai_tool_call(items: &[Value]) -> bool {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();

    for item in items {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
            }
            _ => {}
        }
    }

    !openai_tool_call_sets_are_complete(
        &function_calls,
        &function_outputs,
        &custom_tool_calls,
        &custom_tool_outputs,
    )
}

fn build_openai_compact_request_body(request_shape: &OpenAiRequestShape, items: &[Value]) -> Value {
    let compact_items = sanitize_openai_store_false_compact_items(items);
    let mut body = json!({
        "model": request_shape.wire_shape.get("model").cloned().unwrap_or(Value::Null),
        "input": compact_items,
        "instructions": request_shape
            .wire_shape
            .get("instructions")
            .cloned()
            .unwrap_or_else(|| Value::String(request_shape.prompt_frame.system_prompt.clone())),
        "tools": request_shape
            .wire_shape
            .get("tools")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "parallel_tool_calls": request_shape
            .wire_shape
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(Value::Bool(false)),
    });
    if let Some(reasoning) = request_shape.wire_shape.get("reasoning") {
        if !reasoning.is_null() {
            body["reasoning"] = reasoning.clone();
        }
    }
    if let Some(text) = request_shape.wire_shape.get("text") {
        body["text"] = text.clone();
    }
    body
}

fn sanitize_openai_store_false_compact_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect()
}

fn openai_append_match_input_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_append_match_item)
        .collect()
}

// Provider windows are replayed into future OpenAI requests and compact calls.
// Keep their wire shape close to OpenAI's item contract while stripping fields
// that are not accepted on replay.
fn canonicalize_openai_provider_item(item: &Value) -> Value {
    let mut item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    if object.get("type").and_then(Value::as_str) == Some("compaction_summary") {
        object.insert("type".into(), Value::String("compaction".into()));
    }
    if matches!(
        object.get("type").and_then(Value::as_str),
        Some("message" | "function_call" | "custom_tool_call" | "reasoning" | "compaction")
    ) {
        object.remove("status");
    }
    if object.get("type").and_then(Value::as_str) == Some("function_call") {
        if let Some(arguments) = object.get("arguments").and_then(Value::as_str) {
            if let Ok(parsed_arguments) = serde_json::from_str::<Value>(arguments) {
                object.insert(
                    "arguments".into(),
                    Value::String(canonical_json(&parsed_arguments)),
                );
            }
        }
    }
    item
}

// Append matching compares provider outputs against Holon-rebuilt input. Use a
// semantic form that preserves item order and conversational meaning while
// ignoring provider-only metadata and nested text decorations.
fn canonicalize_openai_append_match_item(item: &Value) -> Value {
    let item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object() else {
        return item;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("message") => canonicalize_openai_append_match_message(object),
        Some("function_call") => canonicalize_openai_append_match_function_call(object),
        Some("custom_tool_call") => canonicalize_openai_append_match_custom_tool_call(object),
        Some("function_call_output" | "custom_tool_call_output") => json!({
            "type": object.get("type").cloned().unwrap_or(Value::Null),
            "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
            "output": object.get("output").cloned().unwrap_or(Value::Null),
        }),
        Some("compaction_summary") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some("compaction") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some(_) | None => canonicalize_openai_provider_item(&item),
    }
}

fn canonicalize_openai_append_match_message(object: &serde_json::Map<String, Value>) -> Value {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| canonicalize_openai_append_match_content_item(role, item))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn canonicalize_openai_append_match_content_item(role: &str, item: &Value) -> Value {
    let Some(object) = item.as_object() else {
        return item.clone();
    };
    let item_type = object.get("type").and_then(Value::as_str);
    if matches!(
        item_type,
        Some("output_text" | "input_text" | "text" | "message_text")
    ) {
        let normalized_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        return json!({
            "type": normalized_type,
            "text": object.get("text").cloned().unwrap_or(Value::String(String::new())),
        });
    }
    let mut canonical = serde_json::Map::new();
    if let Some(item_type) = object.get("type") {
        canonical.insert("type".into(), item_type.clone());
    }
    for key in ["text", "image_url", "file_id", "filename"] {
        if let Some(value) = object.get(key) {
            canonical.insert(key.into(), value.clone());
        }
    }
    Value::Object(canonical)
}

fn canonicalize_openai_append_match_function_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .map(canonicalize_openai_arguments_string)
        .map(Value::String)
        .unwrap_or_else(|| object.get("arguments").cloned().unwrap_or(Value::Null));
    json!({
        "type": "function_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "arguments": arguments,
    })
}

fn canonicalize_openai_append_match_custom_tool_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    json!({
        "type": "custom_tool_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "input": object.get("input").cloned().unwrap_or(Value::Null),
    })
}

fn canonicalize_openai_arguments_string(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .map(|parsed| canonical_json(&parsed))
        .unwrap_or_else(|_| arguments.to_string())
}

fn openai_without_provider_item_id(item: &Value) -> Value {
    let mut item = item.clone();
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(id) = id.as_deref() {
        match object.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") if !object.contains_key("call_id") => {
                object.insert("call_id".into(), Value::String(id.to_string()));
            }
            _ => {}
        }
    }
    object.remove("id");
    item
}

fn openai_compaction_encrypted_content_hashes(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(|content| sha256_hex(content.as_bytes()))
        .collect()
}

fn openai_compaction_encrypted_content_bytes(items: &[Value]) -> Vec<usize> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(str::len)
        .collect()
}

fn invalidate_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&OpenAiContinuationScope>,
) {
    let mut state = lock_openai_continuation(continuation);
    if let Some(scope) = scope {
        state.windows.remove(scope);
    } else {
        state.windows.clear();
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
                                    "arguments": canonical_json(input),
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

async fn send_openai_compact_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<Vec<Value>> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI compact request failed",
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
            "OpenAI compact request failed",
            status,
            body,
        ));
    }

    let response_body = response.text().await.map_err(|error| {
        classify_reqwest_transport_error(
            "OpenAI compact response body failed",
            "response_body",
            "openai",
            Some(&provider_model_ref("openai", &body)),
            Some(url.as_str()),
            error,
        )
    })?;
    let parsed: Value = serde_json::from_str(&response_body)
        .map_err(|error| invalid_response_error("invalid OpenAI compact JSON", error))?;
    let output = parsed
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI compact response did not contain output array",
                "missing output array",
            )
        })?;
    Ok(output
        .into_iter()
        .map(|item| canonicalize_openai_provider_item(&item))
        .collect())
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
                    StreamingSseEvent::Incomplete(response) => {
                        return recover_openai_incomplete_response(response, &streamed_output_items)
                    }
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
        StreamingSseEvent::Incomplete(response) => {
            return recover_openai_incomplete_response(response, &streamed_output_items)
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
    Incomplete(Value),
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
        if event_type == "response.incomplete" || status == Some("incomplete") {
            if openai_incomplete_reason(response) == Some("max_output_tokens") {
                return Ok(StreamingSseEvent::Incomplete(response.clone()));
            }
            return Err(classify_openai_incomplete_response(response));
        }
        if status == Some("cancelled") {
            return Err(classify_openai_incomplete_response(response));
        }
    }

    Ok(StreamingSseEvent::Continue)
}

fn recover_openai_incomplete_response(
    response: Value,
    streamed_output_items: &[Value],
) -> Result<Value> {
    let response = finalize_openai_terminal_response(response, streamed_output_items);
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output {
        Ok(response)
    } else {
        Err(classify_openai_incomplete_response(&response))
    }
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
    let reason = openai_incomplete_reason(response)
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

fn openai_incomplete_reason(response: &Value) -> Option<&str> {
    response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
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
    let output_items = output
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect::<Vec<_>>();
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
                .get("incomplete_details")
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    response
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
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
    use super::{chat_completions_url, latest_openai_compaction_index};
    use serde_json::json;

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

    #[test]
    fn openai_provider_window_tracks_latest_compaction_item() {
        let items = vec![
            json!({ "type": "message", "role": "user" }),
            json!({ "type": "compaction", "encrypted_content": "first" }),
            json!({ "type": "message", "role": "user" }),
            json!({ "type": "compaction", "encrypted_content": "second" }),
        ];

        assert_eq!(latest_openai_compaction_index(&items), Some(3));
    }
}
