use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use holon_domain::PromptStability;
use reqwest::{header::HeaderMap, Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tracing::warn;
use twox_hash::XxHash64;

use super::super::budget::{effective_output_tokens, estimate_context_budget, ContextBudget};
use crate::{
    config::{
        AnthropicCacheStrategy, AnthropicContextManagementConfig, CredentialKind,
        ProviderBuiltinWebSearchConfig, ProviderRuntimeConfig,
    },
    model_catalog::{AnthropicCacheControlSource, ResolvedAnthropicCachePolicy},
    provider::{
        builtin_web_search_probe_turn_request,
        http_trace::{ProviderHttpTrace, ProviderHttpTraceRequest},
        AgentProvider, AnthropicPromptCacheDiagnostics, AttemptTokenUsage, CacheBreakpointInfo,
        ConversationMessage, ModelBlock, ModelToolCallKind, PromptContentBlock, ProviderBlockData,
        ProviderBuiltinWebSearchCapability, ProviderCacheUsage, ProviderContextBudgetDiagnostics,
        ProviderContextManagementPolicy, ProviderNativeWebSearchDiagnostics,
        ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest, ProviderPromptCapability,
        ProviderResponseFormatDiagnostics, ProviderResponseFormatRequest,
        ProviderTransportDiagnostics, ProviderTransportTimeline, ProviderTurnRequest,
        ProviderTurnResponse, INCOMPLETE_TOOL_RESULT_SENTINEL,
    },
};

use super::{
    build_http_client, read_error_response_body, response_body_timeout, send_provider_request,
    stream_idle_timeout, RequestDeadline,
};
use crate::provider::retry::{
    classify_reqwest_transport_error_with_trace, classify_status_error_with_trace,
    empty_response_error_with_trace, invalid_response_error_with_trace, parse_retry_after,
    provider_transport_error, provider_transport_error_with_code,
    retryable_invalid_response_error_with_trace, sanitize_transport_url,
    set_provider_transport_usage_evidence, timeout_transport_error_with_trace,
    ProviderFailureClassification, ProviderFailureKind, RetryDisposition,
};

const ANTHROPIC_PROVIDER_BLOCK_FORMAT: &str = "anthropic_messages/v1";

#[derive(Clone)]
pub struct AnthropicProvider {
    client: Client,
    request_headers: crate::provider::request_headers::ProviderRequestHeaders,
    route_provider: String,
    route_endpoint: String,
    base_url: String,
    auth_token: Option<String>,
    quota_identity: Option<crate::provider::ProviderQuotaIdentity>,
    model: String,
    max_output_tokens: u32,
    context_window_tokens: Option<usize>,
    reasoning_effort: Option<String>,
    supports_reasoning: bool,
    context_management: AnthropicContextManagementConfig,
    cache_control_source: Option<AnthropicCacheControlSource>,
    model_route: String,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    trace_home_dir: PathBuf,
    #[cfg(any(test, feature = "runtime-tests"))]
    http_trace_override: Option<ProviderHttpTrace>,
}

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfigRequest<'a>>,
    stream: bool,
    system: Value,
    messages: Vec<ApiMessage>,
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
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
struct ThinkingRequest {
    #[serde(rename = "type")]
    kind: &'static str,
    budget_tokens: u32,
}

#[derive(Debug, Serialize)]
struct OutputConfigRequest<'a> {
    effort: &'a str,
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MessagesResponse {
    id: Option<String>,
    pub content: Vec<ApiResponseBlock>,
    stop_reason: Option<String>,
    usage: Option<ApiUsage>,
    context_management: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ApiResponseBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ApiUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl AnthropicProvider {
    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        supports_reasoning: bool,
        holon_version: &'static str,
    ) -> Result<Self> {
        Self::from_runtime_config_with_context_window(
            provider_config,
            model,
            max_output_tokens,
            None,
            trace_home_dir,
            supports_reasoning,
            holon_version,
        )
    }

    pub fn from_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
        supports_reasoning: bool,
        holon_version: &'static str,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let auth_token = provider_config
            .credential
            .clone()
            .filter(|value| !value.trim().is_empty());
        if auth_token.is_none()
            && provider_config.auth.kind != CredentialKind::None
            && !holon_domain::model_values::built_in_provider_allows_anonymous_access(
                provider_config.route_provider.as_str(),
            )
        {
            let credential_name = provider_config
                .auth
                .env
                .as_deref()
                .or(provider_config.auth.profile.as_deref())
                .or(provider_config.auth.external.as_deref())
                .unwrap_or("configured credential");
            return Err(anyhow!("missing {credential_name}"));
        }
        Ok(Self {
            client,
            route_provider: provider_config.route_provider.as_str().to_string(),
            request_headers: crate::provider::request_headers::ProviderRequestHeaders::new(
                provider_config,
                holon_version,
            ),
            route_endpoint: provider_config.route_endpoint.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            auth_token,
            quota_identity: crate::provider::ProviderQuotaIdentity::for_api_credential(
                provider_config,
            ),
            model: model.to_string(),
            max_output_tokens,
            context_window_tokens,
            reasoning_effort: provider_config.reasoning_effort.clone(),
            supports_reasoning,
            context_management: provider_config.context_management.clone(),
            cache_control_source: None,
            model_route: format!(
                "{}@{}/{}",
                provider_config.route_provider.as_str(),
                provider_config.route_endpoint.as_str(),
                model
            ),
            builtin_web_search: provider_config.builtin_web_search.clone(),
            trace_home_dir: trace_home_dir.to_path_buf(),
            #[cfg(any(test, feature = "runtime-tests"))]
            http_trace_override: None,
        })
    }

    pub fn with_cache_policy(
        mut self,
        cache_policy: ResolvedAnthropicCachePolicy,
        model_route: String,
    ) -> Self {
        self.context_management.cache_capabilities.cache_control = cache_policy.cache_control;
        self.cache_control_source = Some(cache_policy.source);
        self.model_route = model_route;
        self
    }

    fn effective_betas(&self) -> Vec<String> {
        self.context_management
            .betas
            .iter()
            .filter(|beta| {
                self.context_management.cache_capabilities.cache_control
                    || !matches!(
                        beta.as_str(),
                        "claude-code-20250219" | "prompt-caching-scope-2026-01-05"
                    )
            })
            .cloned()
            .collect()
    }

    #[cfg(any(test, feature = "runtime-tests"))]
    pub fn with_http_trace_for_tests(mut self, trace: ProviderHttpTrace) -> Self {
        self.http_trace_override = Some(trace);
        self
    }
}

/// Map an OpenAI-style `reasoning_effort` level to an Anthropic
/// `thinking.budget_tokens` value.
///
/// Returns `None` when thinking is not requested (`reasoning_effort` is absent).
/// The budget is computed as a percentage of `max_output_tokens`, clamped to
/// the Anthropic minimum of 1024 and capped at `max_output_tokens - 1`.
fn reasoning_effort_to_thinking(
    reasoning_effort: Option<&str>,
    max_output_tokens: u32,
) -> Option<ThinkingRequest> {
    let effort = reasoning_effort?;
    let ratio: f64 = match effort.to_ascii_lowercase().as_str() {
        "low" => 0.10,
        "medium" => 0.25,
        "high" => 0.50,
        "xhigh" | "max" => 0.80,
        // Unknown values: default to medium.
        _ => 0.25,
    };
    let budget = (max_output_tokens as f64 * ratio).round() as u32;
    // Anthropic requires budget_tokens >= 1024 and <= max_tokens - 1.
    let budget = budget.max(1024).min(max_output_tokens.saturating_sub(1));
    // If max_output_tokens is too small for thinking, skip it.
    if budget < 1024 {
        return None;
    }
    Some(ThinkingRequest {
        kind: "enabled",
        budget_tokens: budget,
    })
}

fn requires_always_on_adaptive_thinking(route_provider: &str, model: &str) -> bool {
    route_provider == "anthropic" && model == "claude-fable-5-2"
}

fn anthropic_cache_temperature(
    route_provider: &str,
    model: &str,
    cache_strategy: AnthropicCacheStrategy,
    cache_control: bool,
) -> Option<f32> {
    (cache_strategy == AnthropicCacheStrategy::ClaudeCodePromptCache
        && cache_control
        && !requires_always_on_adaptive_thinking(route_provider, model))
    .then_some(1.0)
}

fn anthropic_reasoning_controls<'a>(
    route_provider: &str,
    model: &str,
    supports_reasoning: bool,
    reasoning_effort: Option<&'a str>,
    max_output_tokens: u32,
) -> Result<(Option<ThinkingRequest>, Option<OutputConfigRequest<'a>>)> {
    if requires_always_on_adaptive_thinking(route_provider, model) {
        if let Some(effort) = reasoning_effort {
            if !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") {
                return Err(anyhow!(
                    "{model} requires thinking; reasoning effort must be low, medium, high, xhigh, or max"
                ));
            }
        }
        // Thinking is always adaptive; neither a manual budget nor a disable is valid.
        return Ok((
            None,
            reasoning_effort.map(|effort| OutputConfigRequest { effort }),
        ));
    }
    if route_provider == "dashscope" && model == "qwen4-max" {
        let effort = match reasoning_effort {
            None => None,
            Some(effort) => Some(match (model, effort) {
                (_, "low" | "xhigh") => effort,
                ("qwen4-max", "medium" | "high") => "high",
                (_, "max") => "xhigh",
                _ => {
                    return Err(anyhow!(
                        "DashScope {model} reasoning effort must be low, medium, high, xhigh, or max"
                    ));
                }
            }),
        };
        // Omission preserves each model's default; a manual budget is not an effort.
        return Ok((None, effort.map(|effort| OutputConfigRequest { effort })));
    }
    if route_provider == "dashscope" && model == "kimi-k3.1-pro" {
        if let Some(effort) = reasoning_effort {
            if !matches!(effort, "low" | "medium" | "high" | "max") {
                return Err(anyhow!(
                    "DashScope {model} reasoning effort must be low, medium, high, or max"
                ));
            }
        }
        return Ok((
            None,
            reasoning_effort.map(|effort| OutputConfigRequest { effort }),
        ));
    }
    if matches!(route_provider, "minimax" | "minimax-cn")
        && matches!(model, "MiniMax-M3.1-Flash-Preview" | "MiniMax-M3.1-Flash")
    {
        if let Some(effort) = reasoning_effort {
            if !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") {
                return Err(anyhow!(
                    "{model} requires thinking; reasoning effort must be low, medium, high, xhigh, or max"
                ));
            }
        }
        // MiniMax always thinks for this model; depth is not a token budget.
        return Ok((
            None,
            reasoning_effort.map(|effort| OutputConfigRequest { effort }),
        ));
    }
    if route_provider == "deepseek" {
        return Ok((
            None,
            reasoning_effort.map(|effort| OutputConfigRequest { effort }),
        ));
    }
    Ok((
        supports_reasoning
            .then(|| reasoning_effort_to_thinking(reasoning_effort, max_output_tokens))
            .flatten(),
        None,
    ))
}

fn context_budget_overflow_error(
    route_provider: &str,
    model_ref: &str,
    url: &str,
    budget: ContextBudget,
) -> anyhow::Error {
    const SUGGESTION: &str =
        "reduce prompt/context or requested output tokens, or increase the provider context window";
    let required_tokens = budget.required_tokens();
    let diagnostics = ProviderTransportDiagnostics {
        stage: "request_preflight".to_string(),
        streaming: None,
        provider: Some(route_provider.to_string()),
        model_ref: Some(model_ref.to_string()),
        url: Some(sanitize_transport_url(url)),
        status: None,
        reqwest: None,
        context_budget: Some(ProviderContextBudgetDiagnostics {
            estimated_input_tokens: budget.estimated_input_tokens,
            estimated_text_tokens: budget.estimated_text_tokens,
            image_reserve_tokens: budget.image_reserve_tokens,
            image_count: budget.image_count,
            inline_image_base64_bytes: budget.inline_image_base64_bytes,
            estimated_inline_image_decoded_bytes: budget.estimated_inline_image_decoded_bytes,
            requested_output_tokens: budget.requested_output_tokens,
            safety_headroom_tokens: budget.safety_headroom_tokens,
            required_tokens,
            context_window_tokens: budget.context_window_tokens,
            overflow_tokens: budget.overflow_tokens(),
            suggestion: SUGGESTION.to_string(),
        }),
        http_trace: None,
        quota_identity: None,
        retry_after_not_before: None,
        throttle: None,
        source_chain: Vec::new(),
    };
    provider_transport_error_with_code(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded"),
        None,
        Some(diagnostics),
        format!(
            "provider request exceeds context window: estimated input {} + requested output {} + safety headroom {} = {} tokens, context window {} tokens; {SUGGESTION}",
            budget.estimated_input_tokens,
            budget.requested_output_tokens,
            budget.safety_headroom_tokens,
            required_tokens,
            budget.context_window_tokens,
        ),
    )
}

struct PreparedAnthropicRequest {
    payload: Value,
    effective_output_tokens: u32,
    wire_conversation: Vec<ConversationMessage>,
    rolling_cache_marker: Option<(usize, usize)>,
}

impl AnthropicProvider {
    fn prepare_request_payload(
        &self,
        request: &ProviderTurnRequest,
    ) -> Result<PreparedAnthropicRequest> {
        let material = &request.material;
        material.admit_value(&(&request.prompt_frame, &request.tools))?;
        material.read(|| {
            use crate::resource_io;
            resource_io::charge_owned_metadata(
                u64::try_from(request.conversation.len())?
                    .checked_mul(u64::try_from(std::mem::size_of::<ConversationMessage>())?)
                    .context("Anthropic conversation material overflow")?,
                request.conversation.len(),
            )?;
            for message in &request.conversation {
                match message {
                    ConversationMessage::UserText(text) => resource_io::admit_cloned_value(text)?,
                    ConversationMessage::UserBlocks(blocks) => {
                        resource_io::admit_cloned_value(blocks)?
                    }
                    ConversationMessage::UserImage {
                        prompt, media_type, ..
                    } => resource_io::admit_cloned_value(&(prompt, media_type))?,
                    ConversationMessage::AssistantBlocks(blocks) => {
                        resource_io::admit_cloned_value(blocks)?
                    }
                    ConversationMessage::UserToolResults(results) => {
                        resource_io::admit_cloned_value(results)?
                    }
                }
            }
            Ok(())
        })?;
        let cache_strategy = self.context_management.cache_strategy;
        let cache_control = self.context_management.cache_capabilities.cache_control;
        let system = build_anthropic_system(request, cache_strategy, cache_control);
        let mark_context = can_mark_turn_scoped_context(&system, cache_control);
        let wire_conversation = build_anthropic_wire_conversation(request, mark_context);
        let rolling_cache_marker =
            rolling_conversation_cache_marker(&wire_conversation, cache_strategy, cache_control);
        material.read(|| {
            for message in &wire_conversation {
                if let ConversationMessage::UserImage {
                    prompt,
                    media_type,
                    data_base64,
                    ..
                } = message
                {
                    crate::resource_io::admit_cloned_value(&(prompt, media_type, &**data_base64))?;
                }
            }
            Ok(())
        })?;
        let mut messages =
            build_anthropic_messages(&wire_conversation, rolling_cache_marker, cache_control);
        if needs_placeholder_user_text(&self.route_provider) {
            ensure_placeholder_user_text(&mut messages);
        }
        let response_format_tool_choice = anthropic_response_format_tool_choice_for_model(
            request,
            &self.route_provider,
            &self.model,
        )?;
        let (thinking, output_config) = anthropic_reasoning_controls(
            &self.route_provider,
            &self.model,
            self.supports_reasoning,
            self.reasoning_effort.as_deref(),
            self.max_output_tokens,
        )?;
        let request_body = MessagesRequest {
            model: &self.model,
            max_tokens: self.max_output_tokens,
            thinking,
            output_config,
            stream: true,
            system,
            messages,
            tools: build_anthropic_tools(request),
            tool_choice: response_format_tool_choice,
            betas: self.effective_betas(),
            metadata: build_anthropic_metadata(request, cache_strategy, cache_control),
            temperature: anthropic_cache_temperature(
                &self.route_provider,
                &self.model,
                cache_strategy,
                cache_control,
            ),
            context_management: build_context_management_request(&self.context_management),
        };
        let mut request_payload = material.json_value(&request_body)?;
        let effective_output = effective_output_tokens(
            self.context_window_tokens,
            self.max_output_tokens,
            &request_payload,
            &["max_tokens"],
        );
        request_payload["max_tokens"] = Value::from(effective_output);
        if let Some(budget_tokens) = request_payload
            .get_mut("thinking")
            .and_then(Value::as_object_mut)
            .and_then(|thinking| thinking.get_mut("budget_tokens"))
        {
            *budget_tokens = Value::from(
                (*budget_tokens)
                    .as_u64()
                    .unwrap_or(0)
                    .min(effective_output as u64),
            );
        }

        Ok(PreparedAnthropicRequest {
            payload: request_payload,
            effective_output_tokens: effective_output,
            wire_conversation,
            rolling_cache_marker,
        })
    }
}

#[async_trait]
impl AgentProvider for AnthropicProvider {
    fn transport_id(&self) -> Option<&'static str> {
        Some("anthropic_messages")
    }

    fn quota_identity(&self) -> Option<crate::provider::ProviderQuotaIdentity> {
        self.quota_identity.clone()
    }

    async fn complete_turn(
        &self,
        mut request: ProviderTurnRequest,
    ) -> Result<ProviderTurnResponse> {
        self.request_headers
            .require_session(request.provider_session_id.as_ref())?;
        let material = request.material.clone();
        material.validate_images(&request.conversation)?;
        material
            .complete_turn(async {
                let mut prepared = self.prepare_request_payload(&request)?;
                let mut overflow = estimate_context_budget(
                    self.context_window_tokens,
                    prepared.effective_output_tokens,
                    &prepared.payload,
                    &["max_tokens"],
                )
                .filter(|budget| budget.overflow_tokens() > 0);
                if overflow.is_some() {
                    let first_optional_image = request.conversation.iter().position(|message| {
                        matches!(
                            message,
                            ConversationMessage::UserImage {
                                optional_history: true,
                                ..
                            }
                        )
                    });
                    if let Some(index) = first_optional_image {
                        request.conversation.retain(|message| {
                            !matches!(
                                message,
                                ConversationMessage::UserImage {
                                    optional_history: true,
                                    ..
                                }
                            )
                        });
                        // Replace the first omitted image without moving the dynamic time
                        // tail or introducing a new rolling cache breakpoint.
                        request.conversation.insert(
                            index,
                            ConversationMessage::UserBlocks(vec![PromptContentBlock {
                                text: "Historical original images were omitted to fit the context budget. \
                                       Use prior textual observations only if present in this context; \
                                       request ViewImage explicitly if image details need inspection."
                                    .to_string(),
                                stability: PromptStability::TurnScoped,
                                cache_breakpoint: false,
                            }]),
                        );
                        prepared = self.prepare_request_payload(&request)?;
                        overflow = estimate_context_budget(
                            self.context_window_tokens,
                            prepared.effective_output_tokens,
                            &prepared.payload,
                            &["max_tokens"],
                        )
                        .filter(|budget| budget.overflow_tokens() > 0);
                    }
                }
                // Accept both origin-style bases and version-prefix bases from
                // persisted compatible-provider configurations.
                let url = if self.base_url.ends_with("/v1") {
                    format!("{}/messages", self.base_url)
                } else {
                    format!("{}/v1/messages", self.base_url)
                };
                let model_ref = self.model_route.clone();
                if let Some(budget) = overflow {
                    return Err(context_budget_overflow_error(
                        &self.route_provider,
                        &model_ref,
                        &url,
                        budget,
                    ));
                }
                let PreparedAnthropicRequest {
                    payload: request_payload,
                    wire_conversation,
                    rolling_cache_marker,
                    ..
                } = prepared;
                let cache_strategy = self.context_management.cache_strategy;
                let mut headers = vec![
                    ("content-type", "application/json".to_string()),
                    ("anthropic-version", "2023-06-01".to_string()),
                ];
                if let Some(auth_token) = self.auth_token.as_deref() {
                    headers.push(("authorization", format!("Bearer {auth_token}")));
                }
                if self.context_management.enabled {
                    headers.push((
                        "anthropic-beta",
                        "context-management-2025-06-27".to_string(),
                    ));
                }
                let headers = self.request_headers.compose(headers, request.provider_session_id.as_ref())?;
                #[cfg(any(test, feature = "runtime-tests"))]
                let trace = self
                    .http_trace_override
                    .clone()
                    .or_else(|| ProviderHttpTrace::from_env(self.trace_home_dir.clone()));
                #[cfg(not(any(test, feature = "runtime-tests")))]
                let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
                let request_trace = trace
                    .map(|trace| {
                        trace.begin_request_with_material(
                            request
                                .prompt_frame
                                .cache
                                .as_ref()
                                .map(|cache| cache.agent_id.as_str()),
                            &self.route_provider,
                            Some(&model_ref),
                            url.as_str(),
                            "messages",
                            &headers,
                            &request_payload,
                            &material,
                        )
                    })
                    .transpose()?
                    .flatten();
                let mut request_builder = self.client.post(&url);
                for (name, value) in &headers {
                    request_builder = request_builder.header(*name, value);
                }
                let request_started_at = chrono::Utc::now();
                let response = send_provider_request(
                    request_builder.body(material.json_bytes(&request_payload)?),
                    "Anthropic request failed",
                    "request_send",
                    &self.route_provider,
                    Some(&model_ref),
                    Some(url.as_str()),
                    RequestDeadline::TextStream,
                    request_trace.as_ref(),
                    request.dispatch_guard.as_deref(),
                )
                .await?;
                let response_headers_at = chrono::Utc::now();
                if let Some(trace) = request_trace.as_ref() {
                    trace.write_response_headers(response.status(), response.headers());
                }
                let provider_request_id = provider_request_id_from_headers(response.headers());

                if !response.status().is_success() {
                    let status = response.status();
                    let retry_after = parse_retry_after(response.headers());
                    let body = read_error_response_body(response, request_trace.as_ref(), &material).await?;
                    return Err(crate::provider::retry::set_provider_transport_streaming(
                        classify_status_error_with_trace(
                            "Anthropic request failed",
                            "response_status",
                            Some(&self.route_provider),
                            Some(&model_ref),
                            Some(url.as_str()),
                            status,
                            body,
                            request_trace.as_ref(),
                            retry_after,
                        ),
                        true,
                    ));
                }

                let streaming = anthropic_response_is_sse(&response);
                let parsed = if streaming {
                    read_anthropic_streaming_response(
                        response,
                        &self.route_provider,
                        &model_ref,
                        url.as_str(),
                        request_trace.as_ref(),
                        &material,
                    )
                    .await?
                } else {
                    read_anthropic_json_response(
                        response,
                        &self.route_provider,
                        &model_ref,
                        url.as_str(),
                        request_trace.as_ref(),
                        &material,
                    )
                    .await?
                };
                let response_body_completed_at = chrono::Utc::now();
                material.admit_value(&parsed)?;
                let mut response = anthropic_messages_response_to_turn_response(
                    parsed,
                    provider_request_id,
                    &request,
                    &wire_conversation,
                    rolling_cache_marker,
                    &request_payload,
                    self.route_provider.as_str(),
                    self.model.as_str(),
                    cache_strategy,
                    &self.effective_betas(),
                    &model_ref,
                    url.as_str(),
                    request_trace.as_ref(),
                )?;
                let parse_completed_at = chrono::Utc::now();
                let cache_diagnostics = response
                    .request_diagnostics
                    .as_mut()
                    .and_then(|diagnostics| diagnostics.anthropic_cache.as_mut())
                    .expect("Anthropic responses include cache diagnostics");
                cache_diagnostics.explicit_cache_control_enabled =
                    Some(self.context_management.cache_capabilities.cache_control);
                cache_diagnostics.cache_control_source = self.cache_control_source;
                cache_diagnostics.model_route = Some(self.model_route.clone());
                response
                    .request_diagnostics
                    .as_mut()
                    .expect("Anthropic responses include request diagnostics")
                    .transport_timeline = Some(ProviderTransportTimeline {
                    request_started_at,
                    response_headers_at,
                    response_body_completed_at: Some(response_body_completed_at),
                    parse_completed_at,
                    streaming,
                });
                Ok(response)
            })
            .await
    }

    #[cfg(any(test, feature = "runtime-tests"))]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!(
            "{}@{}/{}",
            self.route_provider, self.route_endpoint, self.model
        )]
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

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.route_provider.clone(),
            provider_model_ref: format!(
                "{}@{}/{}",
                self.route_provider, self.route_endpoint, self.model
            ),
            provider_transport: "anthropic_messages".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        self.complete_turn(builtin_web_search_probe_turn_request(request))
            .await?;
        Ok(())
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        self.context_management
            .enabled
            .then(|| ProviderContextManagementPolicy {
                provider: self.route_provider.clone(),
                strategy: "clear_tool_uses_20250919".to_string(),
                keep_recent_tool_uses: self.context_management.keep_recent_tool_uses as usize,
                trigger_input_tokens: self.context_management.trigger_input_tokens,
                clear_at_least_input_tokens: self.context_management.clear_at_least_input_tokens,
            })
    }
}

fn can_mark_turn_scoped_context(system: &Value, cache_control: bool) -> bool {
    // Reserve one breakpoint for the rolling history marker (Anthropic
    // permits at most four explicit breakpoints per request).
    let system_breakpoints = system
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("cache_control").is_some())
                .count()
        })
        .unwrap_or(0);
    cache_control && system_breakpoints < 3
}

/// Wire conversation for both cache strategies: history without the
/// materialized context head. Non-TurnScoped context is carried by the
/// system prefix; TurnScoped context precedes history so subsequent rounds of
/// the same turn can reuse it at a fixed cache breakpoint.
fn build_anthropic_wire_conversation(
    request: &ProviderTurnRequest,
    mark_context: bool,
) -> Vec<ConversationMessage> {
    let mut conversation = strip_initial_context_message(request);
    let mut blocks = turn_scoped_context_blocks(request);
    if blocks.is_empty() {
        return conversation;
    }
    for block in &mut blocks {
        block.cache_breakpoint = false;
    }
    if mark_context {
        if let Some(last) = blocks.last_mut() {
            last.cache_breakpoint = true;
        }
    }
    match conversation.first_mut() {
        Some(ConversationMessage::UserBlocks(existing)) => {
            blocks.append(existing);
            *existing = blocks;
        }
        Some(ConversationMessage::UserText(text)) => {
            blocks.push(PromptContentBlock {
                text: text.clone(),
                stability: PromptStability::TurnScoped,
                cache_breakpoint: false,
            });
            conversation[0] = ConversationMessage::UserBlocks(blocks);
        }
        _ => conversation.insert(0, ConversationMessage::UserBlocks(blocks)),
    }
    conversation
}

fn anthropic_response_is_sse(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

async fn read_anthropic_json_response(
    response: Response,
    provider: &str,
    model_ref: &str,
    url: &str,
    trace: Option<&ProviderHttpTraceRequest>,
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<MessagesResponse> {
    let response_body =
        match tokio::time::timeout(response_body_timeout(), material.response_text(response)).await
        {
            Ok(Ok(text)) => text,
            Ok(Err(error)) if error.downcast_ref::<reqwest::Error>().is_none() => {
                return Err(error)
            }
            Ok(Err(error)) => {
                return Err(classify_reqwest_transport_error_with_trace(
                    "Anthropic response body failed",
                    "response_body",
                    provider,
                    Some(model_ref),
                    Some(url),
                    match error.downcast::<reqwest::Error>() {
                        Ok(error) => error,
                        Err(error) => return Err(error),
                    },
                    trace,
                ));
            }
            Err(_elapsed) => {
                return Err(timeout_transport_error_with_trace(
                    "Anthropic response body read timed out",
                    "response_body",
                    provider,
                    Some(model_ref),
                    Some(url),
                    format!("timed out after {:?}", response_body_timeout()),
                    trace,
                ));
            }
        };
    if let Some(trace) = trace {
        trace.write_response_body(&response_body);
    }
    material.read(|| crate::resource_io::admit_json_payload(&response_body))?;
    serde_json::from_str(&response_body).map_err(|error| {
        invalid_response_error_with_trace(
            "invalid Anthropic JSON",
            "response_parse",
            provider,
            Some(model_ref),
            Some(url),
            error.to_string(),
            trace,
        )
    })
}

async fn read_anthropic_streaming_response(
    mut response: Response,
    provider: &str,
    model_ref: &str,
    url: &str,
    trace: Option<&ProviderHttpTraceRequest>,
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<MessagesResponse> {
    let mut parser = SseParser {
        material: material.clone(),
        ..Default::default()
    };
    let mut accumulator = AnthropicStreamAccumulator {
        material: material.clone(),
        ..Default::default()
    };
    let idle_timeout = stream_idle_timeout();

    while !accumulator.saw_message_stop {
        let next = tokio::time::timeout(idle_timeout, response.chunk())
            .await
            .map_err(|_| {
                timeout_transport_error_with_trace(
                    "Anthropic streaming response timed out",
                    "streaming_response_body",
                    provider,
                    Some(model_ref),
                    Some(url),
                    format!(
                        "no Anthropic stream bytes received for {} ms",
                        idle_timeout.as_millis()
                    ),
                    trace,
                )
            })?;

        match next {
            Ok(Some(chunk)) => {
                if let Some(trace) = trace {
                    trace.write_stream_chunk(&chunk);
                }
                for event in parser.push(&chunk).map_err(|error| {
                    classify_anthropic_sse_parse_error(error, |detail| {
                        invalid_response_error_with_trace(
                            "invalid Anthropic stream event",
                            "streaming_response_parse",
                            provider,
                            Some(model_ref),
                            Some(url),
                            detail,
                            trace,
                        )
                    })
                })? {
                    accumulator.apply(event, model_ref, url, trace)?;
                }
            }
            Err(error) => {
                return Err(classify_reqwest_transport_error_with_trace(
                    "Anthropic streaming response body failed",
                    "streaming_response_body",
                    provider,
                    Some(model_ref),
                    Some(url),
                    error,
                    trace,
                ));
            }
            Ok(None) => break,
        }
    }

    if !accumulator.saw_message_stop {
        for event in parser.finish().map_err(|error| {
            classify_anthropic_sse_parse_error(error, |detail| {
                invalid_response_error_with_trace(
                    "invalid Anthropic stream event",
                    "streaming_response_parse",
                    provider,
                    Some(model_ref),
                    Some(url),
                    detail,
                    trace,
                )
            })
        })? {
            accumulator.apply(event, model_ref, url, trace)?;
        }
    }
    let response = accumulator.finish(provider, model_ref, url, trace)?;
    if let Some(trace) = trace {
        trace.write_stream_terminal(&material.json_value(&response)?);
    }
    Ok(response)
}

fn anthropic_messages_response_to_turn_response(
    mut parsed: MessagesResponse,
    provider_request_id: Option<String>,
    request: &ProviderTurnRequest,
    wire_conversation: &[ConversationMessage],
    rolling_cache_marker: Option<(usize, usize)>,
    request_payload: &Value,
    provider_id: &str,
    model: &str,
    cache_strategy: AnthropicCacheStrategy,
    betas: &[String],
    model_ref: &str,
    url: &str,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<ProviderTurnResponse> {
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
    // Anthropic `input_tokens` already excludes cache reads/writes, so treat it
    // as the uncached portion directly. Anthropic does not report reasoning
    // separately; `output_tokens` already includes thinking.
    let normalized_token_usage = parsed.usage.as_ref().map(|usage| {
        AttemptTokenUsage::from_exclusive_input(
            usage.input_tokens.unwrap_or(0),
            usage.cache_read_input_tokens.unwrap_or(0),
            usage.cache_creation_input_tokens.unwrap_or(0),
            usage.output_tokens.unwrap_or(0),
            None,
        )
    });
    if parsed.content.is_empty() {
        return Err(set_provider_transport_usage_evidence(
            empty_response_error_with_trace(
                "empty Anthropic response",
                "response_protocol",
                provider_id,
                Some(model_ref),
                Some(url),
                "Anthropic response contained no content blocks",
                trace,
                crate::types::TokenUsage::new(input_tokens, output_tokens),
            ),
            cache_usage,
            normalized_token_usage,
        ));
    }
    let tools_available = !request.tools.is_empty();
    for (block_index, block) in parsed.content.iter().enumerate() {
        warn_unsupported_anthropic_response_block(
            block,
            provider_id,
            model,
            block_index,
            parsed.stop_reason.as_deref(),
            tools_available,
        );
    }
    if anthropic_response_has_text_form_tool_call_violation(
        &parsed.content,
        parsed.stop_reason.as_deref(),
        tools_available,
    ) {
        warn!(
            provider = provider_id,
            model = model,
            stop_reason = ?parsed.stop_reason,
            tools_available,
            "anthropic response stopped for tool_use but returned only text-form tool-call markup"
        );
        return Err(invalid_response_error_with_trace(
            "invalid Anthropic tool response",
            "response_protocol",
            provider_id,
            Some(model_ref),
            Some(url),
            "stop_reason=tool_use without native tool_use block; text block contains tool-call-looking markup",
            trace,
        ));
    }

    if anthropic_response_missing_tool_call_blocks(
        &parsed.content,
        parsed.stop_reason.as_deref(),
        tools_available,
    ) {
        warn!(
            provider = provider_id,
            model = model,
            stop_reason = ?parsed.stop_reason,
            tools_available,
            "anthropic response stopped for tool_use but returned no tool call blocks"
        );
        return Err(set_provider_transport_usage_evidence(
            retryable_invalid_response_error_with_trace(
                "invalid Anthropic tool response",
                "response_protocol",
                provider_id,
                Some(model_ref),
                Some(url),
                "stop_reason=tool_use without any tool_use block; retrying as protocol violation",
                trace,
                crate::types::TokenUsage::new(input_tokens, output_tokens),
            ),
            cache_usage,
            normalized_token_usage,
        ));
    }

    strip_zai_redundant_provider_tool_text(&mut parsed.content);
    let response_format_tool_name = anthropic_response_format_tool_name(request);
    let blocks = parsed
        .content
        .into_iter()
        .filter_map(|block| {
            api_response_block_to_model(block, response_format_tool_name.as_deref())
        })
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        return Err(invalid_response_error_with_trace(
            "invalid Anthropic response",
            "response_protocol",
            provider_id,
            Some(model_ref),
            Some(url),
            "Anthropic response contained no supported content blocks",
            trace,
        ));
    }

    let cache_diagnostics = collect_anthropic_cache_diagnostics(
        request,
        wire_conversation,
        rolling_cache_marker,
        request_payload,
        model,
        cache_strategy,
        betas,
    );
    let request_lowering_mode = anthropic_request_lowering_mode(request, cache_strategy);
    let (stable_history_prefix, dynamic_tail_items) =
        anthropic_stable_history_prefix(request_payload, rolling_cache_marker);
    let stable_prefix = crate::provider::wire_fingerprint::stable_prefix_diagnostics(
        request_payload,
        request_payload,
        &stable_history_prefix,
        dynamic_tail_items,
        &json!({
            "provider_transport": "anthropic_messages",
            "endpoint_dialect": "anthropic_messages",
            "request_lowering_mode": request_lowering_mode,
            "contract_version": 1,
        }),
    );
    Ok(ProviderTurnResponse {
        material: Default::default(),
        blocks,
        stop_reason: parsed.stop_reason,
        input_tokens,
        output_tokens,
        cache_usage,
        normalized_token_usage,
        provider_message_id: parsed.id,
        provider_request_id,
        request_diagnostics: Some(crate::provider::ProviderRequestDiagnostics {
            request_lowering_mode: request_lowering_mode.to_string(),
            provider_id: Some(provider_id.to_string()),
            provider_model_ref: Some(model_ref.to_string()),
            provider_transport: Some("anthropic_messages".to_string()),
            endpoint_dialect: Some("anthropic_messages".to_string()),
            streaming: Some(true),
            reasoning_effort: request_payload
                .pointer("/output_config/effort")
                .or_else(|| request_payload.pointer("/thinking/type"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
            anthropic_cache: Some(cache_diagnostics),
            anthropic_context_management: parsed.context_management,
            openai_request_controls: None,
            incremental_continuation: None,
            openai_remote_compaction: None,
            native_web_search: native_web_search_diagnostics(request),
            response_format: response_format_diagnostics(request),
            stable_prefix: Some(stable_prefix),
            transport_timeline: None,
        }),
    })
}

fn anthropic_stable_history_prefix(
    request_payload: &Value,
    rolling_cache_marker: Option<(usize, usize)>,
) -> (Vec<Value>, usize) {
    let messages = request_payload
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some((message_index, content_index)) = rolling_cache_marker else {
        let prefix_items = messages.len().saturating_sub(1);
        return (
            messages[..prefix_items].to_vec(),
            messages.len().saturating_sub(prefix_items),
        );
    };
    let end = message_index.saturating_add(1).min(messages.len());
    let mut prefix = messages[..end].to_vec();
    if let Some(message) = prefix.get_mut(message_index) {
        if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
            content.truncate(content_index.saturating_add(1));
        }
    }
    (prefix, messages.len().saturating_sub(end))
}

fn provider_request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    saw_message_stop: bool,
    material: crate::provider::ProviderRequestMaterial,
}

impl SseParser {
    #[cfg(feature = "runtime-tests")]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    #[cfg(feature = "runtime-tests")]
    pub fn with_material(material: crate::provider::ProviderRequestMaterial) -> Self {
        Self {
            material,
            ..Self::default()
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<AnthropicStreamEvent>> {
        if self.saw_message_stop {
            return Ok(Vec::new());
        }
        self.material.read(|| {
            crate::resource_io::charge_owned_payload(u64::try_from(chunk.len())?)?;
            Ok(crate::resource_io::charge_owned_metadata(128, 1)?)
        })?;
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let Some(end) = find_sse_event_end(&self.buffer) else {
                break;
            };
            self.material.read(|| {
                crate::resource_io::charge_owned_payload(u64::try_from(end)?)?;
                Ok(crate::resource_io::charge_owned_metadata(128, 1)?)
            })?;
            let raw = self.buffer.drain(..end).collect::<Vec<_>>();
            let separator_len = if self.buffer.starts_with(b"\r\n\r\n") {
                4
            } else {
                2
            };
            self.buffer.drain(..separator_len);
            if let Some(event) = parse_sse_event_with_material(&raw, &self.material)? {
                self.admit_event_slot()?;
                self.saw_message_stop = event.kind == "message_stop";
                events.push(event);
                if self.saw_message_stop {
                    // Do not parse trailing bytes from the same HTTP chunk.
                    self.buffer.clear();
                    break;
                }
            }
        }
        Ok(events)
    }

    fn admit_event_slot(&self) -> Result<()> {
        self.material.read(|| {
            Ok(crate::resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<AnthropicStreamEvent>())?,
                1,
            )?)
        })
    }

    pub fn finish(mut self) -> Result<Vec<AnthropicStreamEvent>> {
        if self.saw_message_stop || self.buffer.is_empty() {
            return Ok(Vec::new());
        }
        let raw = std::mem::take(&mut self.buffer);
        let event = parse_sse_event_with_material(&raw, &self.material)?;
        if event.is_some() {
            self.admit_event_slot()?;
        }
        Ok(event.into_iter().collect())
    }
}

fn find_sse_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| buffer.windows(2).position(|window| window == b"\n\n"))
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct AnthropicSseParseError(String);

pub fn classify_anthropic_sse_parse_error(
    error: anyhow::Error,
    invalid: impl FnOnce(String) -> anyhow::Error,
) -> anyhow::Error {
    match error.downcast::<AnthropicSseParseError>() {
        Ok(error) => invalid(error.0),
        Err(error) => error,
    }
}

#[cfg(test)]
fn parse_sse_event(raw: &[u8]) -> Result<Option<AnthropicStreamEvent>> {
    parse_sse_event_with_material(raw, &Default::default())
}

fn parse_sse_event_with_material(
    raw: &[u8],
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<Option<AnthropicStreamEvent>> {
    let text =
        std::str::from_utf8(raw).map_err(|error| AnthropicSseParseError(error.to_string()))?;
    material.read(|| {
        let lines = text.lines().count();
        crate::resource_io::charge_owned_metadata(
            u64::try_from(lines)?
                .checked_mul(32)
                .context("SSE line material overflow")?,
            lines,
        )?;
        crate::resource_io::charge_owned_payload(u64::try_from(raw.len())?)?;
        Ok(())
    })?;
    let mut data = Vec::new();
    let mut event_type: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        } else if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim());
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    let data = data.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(None);
    }
    material.read(|| crate::resource_io::admit_json_payload(&data))?;
    match serde_json::from_str::<AnthropicStreamEvent>(&data) {
        Ok(event) => Ok(Some(event)),
        Err(_) if event_type == Some("error") => {
            material.read(|| crate::resource_io::admit_json_payload(&data))?;
            parse_fallback_error_event(&data).map_err(|error| AnthropicSseParseError(error).into())
        }
        Err(error) => Err(AnthropicSseParseError(error.to_string()).into()),
    }
}

/// Fallback parser for non-standard SSE error events where `message` is a string
/// (e.g. DashScope sends `{"code":"InvalidParameter","message":"..."}` with an
/// `event: error` SSE line, instead of the Anthropic `{"error": {...}}` shape).
fn parse_fallback_error_event(
    data: &str,
) -> std::result::Result<Option<AnthropicStreamEvent>, String> {
    #[derive(Deserialize)]
    struct FallbackError {
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
        error: Option<AnthropicStreamError>,
    }
    let parsed: FallbackError = serde_json::from_str(data).map_err(|e| e.to_string())?;
    Ok(Some(AnthropicStreamEvent {
        kind: parsed.kind.unwrap_or_else(|| "error".into()),
        message: None,
        index: None,
        content_block: None,
        delta: None,
        usage: None,
        error: Some(AnthropicStreamError {
            kind: parsed
                .code
                .or_else(|| parsed.error.as_ref().and_then(|e| e.kind.clone())),
            message: parsed
                .message
                .or_else(|| parsed.error.and_then(|e| e.message)),
        }),
    }))
}

#[derive(Debug, Deserialize)]
pub struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    message: Option<MessagesResponse>,
    pub index: Option<usize>,
    pub content_block: Option<ApiResponseBlock>,
    pub delta: Option<AnthropicStreamDelta>,
    usage: Option<ApiUsage>,
    error: Option<AnthropicStreamError>,
}

#[derive(Debug, Deserialize)]
pub struct AnthropicStreamDelta {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub text: Option<String>,
    pub thinking: Option<String>,
    pub signature: Option<String>,
    pub partial_json: Option<String>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamError {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

#[derive(Default)]
pub struct AnthropicStreamAccumulator {
    response: Option<MessagesResponse>,
    pub content: Vec<ApiResponseBlock>,
    pub saw_message_stop: bool,
    pub material: crate::provider::ProviderRequestMaterial,
}

impl AnthropicStreamAccumulator {
    #[cfg(feature = "runtime-tests")]
    pub fn with_material(material: crate::provider::ProviderRequestMaterial) -> Self {
        Self {
            material,
            ..Self::default()
        }
    }

    pub fn apply(
        &mut self,
        event: AnthropicStreamEvent,
        model_ref: &str,
        url: &str,
        trace: Option<&ProviderHttpTraceRequest>,
    ) -> Result<()> {
        match event.kind.as_str() {
            "message_start" => {
                if let Some(mut message) = event.message {
                    message.content.clear();
                    self.response = Some(message);
                }
            }
            "content_block_start" => {
                let index = event.index.unwrap_or(self.content.len());
                let Some(mut block) = event.content_block else {
                    return Ok(());
                };
                normalize_stream_start_block(&mut block);
                self.ensure_content_index(index)?;
                self.content[index] = block;
            }
            "content_block_delta" => {
                let Some(index) = event.index else {
                    return Ok(());
                };
                self.ensure_content_index(index)?;
                self.material
                    .read(|| apply_stream_delta(&mut self.content[index], event.delta))?;
            }
            "message_delta" => {
                let response = self.response.get_or_insert_with(MessagesResponse::default);
                if let Some(delta) = event.delta {
                    if let Some(stop_reason) = delta.stop_reason {
                        response.stop_reason = Some(stop_reason);
                    }
                }
                if let Some(usage) = event.usage {
                    merge_anthropic_usage(
                        response.usage.get_or_insert_with(ApiUsage::default),
                        usage,
                    );
                }
            }
            "message_stop" => {
                self.saw_message_stop = true;
            }
            "ping" | "content_block_stop" => {}
            "error" => {
                self.material.admit_value(
                    &event
                        .error
                        .as_ref()
                        .map(|error| (&error.kind, &error.message)),
                )?;
                let detail = event
                    .error
                    .map(|error| {
                        format!(
                            "{}: {}",
                            error.kind.unwrap_or_else(|| "error".into()),
                            error.message.unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|| "Anthropic stream error event".into());
                return Err(provider_transport_error(
                    ProviderFailureClassification {
                        kind: ProviderFailureKind::ServerError,
                        disposition: RetryDisposition::Retryable,
                    },
                    None,
                    None,
                    format!("Anthropic stream failed: {detail}"),
                ));
            }
            _ => {}
        }
        let _ = (model_ref, url, trace);
        Ok(())
    }

    pub fn ensure_content_index(&mut self, index: usize) -> Result<()> {
        if self.content.len() <= index {
            let length = index
                .checked_add(1)
                .context("ResourceExhausted: SSE content index")?;
            let added = length - self.content.len();
            self.material.read(|| {
                Ok(crate::resource_io::charge_owned_metadata(
                    u64::try_from(added)?
                        .checked_mul(u64::try_from(std::mem::size_of::<ApiResponseBlock>())?)
                        .context("ResourceExhausted: SSE content material")?,
                    added,
                )?)
            })?;
            self.content.resize_with(length, ApiResponseBlock::default);
        }
        Ok(())
    }

    pub fn finish(
        mut self,
        provider: &str,
        model_ref: &str,
        url: &str,
        trace: Option<&ProviderHttpTraceRequest>,
    ) -> Result<MessagesResponse> {
        let Some(mut response) = self.response.take() else {
            return Err(invalid_response_error_with_trace(
                "invalid Anthropic stream",
                "streaming_response_protocol",
                provider,
                Some(model_ref),
                Some(url),
                "stream completed without message_start",
                trace,
            ));
        };
        if !self.saw_message_stop {
            return Err(invalid_response_error_with_trace(
                "invalid Anthropic stream",
                "streaming_response_protocol",
                provider,
                Some(model_ref),
                Some(url),
                "stream completed without message_stop",
                trace,
            ));
        }
        self.material.read(|| {
            Ok(crate::resource_io::charge_owned_metadata(
                u64::try_from(self.content.len())?
                    .checked_mul(u64::try_from(std::mem::size_of::<ApiResponseBlock>())?)
                    .context("SSE content material overflow")?,
                self.content.len(),
            )?)
        })?;
        response.content = self
            .content
            .into_iter()
            .filter(|block| !block.kind.is_empty())
            .map(|block| self.material.read(|| finalize_stream_block(block)))
            .collect::<Result<Vec<_>>>()?;
        Ok(response)
    }
}

fn normalize_stream_start_block(block: &mut ApiResponseBlock) {
    match block.kind.as_str() {
        "text" => block.text = Some(String::new()),
        "thinking" => {
            block.thinking = Some(String::new());
            block.signature = None;
        }
        "tool_use" => block.input = Some(Value::String(String::new())),
        _ => {}
    }
}

pub fn apply_stream_delta(
    block: &mut ApiResponseBlock,
    delta: Option<AnthropicStreamDelta>,
) -> Result<()> {
    let Some(delta) = delta else {
        return Ok(());
    };
    let appended = match delta.kind.as_deref() {
        Some("text_delta") => delta.text.as_deref().unwrap_or_default(),
        Some("thinking_delta") => delta.thinking.as_deref().unwrap_or_default(),
        Some("input_json_delta") => delta.partial_json.as_deref().unwrap_or_default(),
        _ => "",
    };
    crate::resource_io::charge_owned_payload(u64::try_from(appended.len())?)?;
    crate::resource_io::charge_owned_metadata(128, 1)?;
    match delta.kind.as_deref() {
        Some("text_delta") => {
            block
                .text
                .get_or_insert_with(String::new)
                .push_str(delta.text.as_deref().unwrap_or_default());
        }
        Some("thinking_delta") => {
            block
                .thinking
                .get_or_insert_with(String::new)
                .push_str(delta.thinking.as_deref().unwrap_or_default());
        }
        Some("signature_delta") => {
            block.signature = delta.signature;
        }
        Some("input_json_delta") => {
            let current = block
                .input
                .get_or_insert_with(|| Value::String(String::new()));
            if !current.is_string() {
                *current = Value::String(String::new());
            }
            if let Value::String(current) = current {
                current.push_str(delta.partial_json.as_deref().unwrap_or_default());
            }
        }
        _ => {}
    }
    Ok(())
}

fn finalize_stream_block(mut block: ApiResponseBlock) -> Result<ApiResponseBlock> {
    if block.kind == "tool_use" {
        if let Some(Value::String(raw)) = block.input.take() {
            crate::resource_io::admit_json_payload(&raw)?;
            block.input = Some(serde_json::from_str(&raw).unwrap_or_else(|_| json!({})));
        }
    }
    Ok(block)
}

fn merge_anthropic_usage(target: &mut ApiUsage, update: ApiUsage) {
    if update.input_tokens.is_some() {
        target.input_tokens = update.input_tokens;
    }
    if update.output_tokens.is_some() {
        target.output_tokens = update.output_tokens;
    }
    if update.cache_read_input_tokens.is_some() {
        target.cache_read_input_tokens = update.cache_read_input_tokens;
    }
    if update.cache_creation_input_tokens.is_some() {
        target.cache_creation_input_tokens = update.cache_creation_input_tokens;
    }
}

/// Strips the materialized context head message when present.
///
/// The runtime materializes `prompt_frame.context_blocks` into
/// `conversation[0]` (see `runtime::turn::projection`), so the head is
/// stripped whenever it matches exactly. Both cache strategies re-lay out
/// context on the wire instead of relying on upper-layer materialization:
/// non-TurnScoped context moves into the system prefix
/// (`claude_code_prompt_cache_anthropic_system` / `current_anthropic_system`)
/// and TurnScoped context is re-attached ahead of history, so unmaterialized
/// frames lose no context on either path.
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

fn turn_scoped_context_blocks(request: &ProviderTurnRequest) -> Vec<PromptContentBlock> {
    request
        .prompt_frame
        .context_blocks
        .iter()
        .filter(|block| block.stability == PromptStability::TurnScoped)
        .cloned()
        .collect()
}

fn build_anthropic_system(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
    cache_control: bool,
) -> Value {
    match cache_strategy {
        AnthropicCacheStrategy::MessagesNative => current_anthropic_system(request, cache_control),
        AnthropicCacheStrategy::ClaudeCodePromptCache => {
            claude_code_prompt_cache_anthropic_system(request, cache_control)
        }
    }
}

/// Native-strategy system prefix: structured system blocks plus
/// non-TurnScoped context blocks. Folding context into the system prefix
/// keeps the two cache strategies symmetric: the transport owns context
/// layout on both paths instead of silently depending on upper-layer
/// materialization into `conversation[0]` (which dropped context entirely
/// for unmaterialized frames).
fn current_anthropic_system(request: &ProviderTurnRequest, cache_control: bool) -> Value {
    let context_blocks: Vec<Value> = request
        .prompt_frame
        .context_blocks
        .iter()
        .filter(|block| block.stability != PromptStability::TurnScoped)
        .map(|block| prompt_block_to_anthropic_content(block, cache_control))
        .collect();
    if !request.prompt_frame.has_structured_system_blocks() && context_blocks.is_empty() {
        return Value::String(request.prompt_frame.system_prompt.clone());
    }
    let mut blocks: Vec<Value> = Vec::new();
    if request.prompt_frame.has_structured_system_blocks() {
        blocks.extend(
            request
                .prompt_frame
                .system_blocks
                .iter()
                .map(|block| prompt_block_to_anthropic_content(block, cache_control)),
        );
    } else if !request.prompt_frame.system_prompt.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": request.prompt_frame.system_prompt }));
    }
    blocks.extend(context_blocks);
    Value::Array(blocks)
}

fn claude_code_prompt_cache_anthropic_system(
    request: &ProviderTurnRequest,
    cache_control: bool,
) -> Value {
    let mut system = Vec::new();
    // Claude Code's billing marker is part of the cache-mimicry surface; it
    // only pays off where explicit `cache_control` breakpoints work.
    if cache_control {
        system.push(json!({
            "type": "text",
            "text": "x-anthropic-billing-header: holon",
        }));
    }

    if !request.prompt_frame.system_blocks.is_empty() {
        system.extend(
            request
                .prompt_frame
                .system_blocks
                .iter()
                .map(|block| prompt_block_to_anthropic_content(block, cache_control)),
        );
    } else if !request.prompt_frame.system_prompt.trim().is_empty() {
        system.push(if cache_control {
            cacheable_text_block(&request.prompt_frame.system_prompt)
        } else {
            json!({ "type": "text", "text": request.prompt_frame.system_prompt })
        });
    }

    system.extend(
        request
            .prompt_frame
            .context_blocks
            .iter()
            .filter(|block| block.stability != PromptStability::TurnScoped)
            .map(|block| prompt_block_to_anthropic_content(block, cache_control)),
    );

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
    cache_control: bool,
) -> Option<Value> {
    if cache_strategy != AnthropicCacheStrategy::ClaudeCodePromptCache || !cache_control {
        return None;
    }
    let session_id = normalize_anthropic_session_id(
        request
            .prompt_frame
            .cache
            .as_ref()
            .map(|cache| cache.prompt_cache_key.as_str()),
    );
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

const ANTHROPIC_SESSION_ID_DEFAULT: &str = "holon-default";
const ANTHROPIC_SESSION_ID_MIN_LEN: usize = 6;
const ANTHROPIC_SESSION_ID_MAX_LEN: usize = 64;
const ANTHROPIC_SESSION_ID_HASH_LEN: usize = 16;

fn normalize_anthropic_session_id(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or_default().trim();
    if raw.is_empty() {
        return ANTHROPIC_SESSION_ID_DEFAULT.to_string();
    }

    if is_anthropic_session_id_safe(raw)
        && raw.len() >= ANTHROPIC_SESSION_ID_MIN_LEN
        && raw.len() <= ANTHROPIC_SESSION_ID_MAX_LEN
    {
        return raw.to_string();
    }

    let digest = sha256_hex(raw.as_bytes());
    let hash = &digest[..ANTHROPIC_SESSION_ID_HASH_LEN];
    let max_prefix_len = ANTHROPIC_SESSION_ID_MAX_LEN - ANTHROPIC_SESSION_ID_HASH_LEN - 1;
    let mut prefix = sanitized_anthropic_session_id_prefix(raw, max_prefix_len);
    if prefix.len() > max_prefix_len {
        prefix.truncate(max_prefix_len);
        trim_anthropic_session_id_suffix(&mut prefix);
    }
    if prefix.is_empty() {
        prefix.push_str("holon");
    }

    format!("{prefix}-{hash}")
}

fn is_anthropic_session_id_safe(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn sanitized_anthropic_session_id_prefix(raw: &str, max_prefix_len: usize) -> String {
    let mut prefix = String::with_capacity(raw.len().min(max_prefix_len));
    for byte in raw.bytes() {
        if prefix.len() >= max_prefix_len {
            break;
        }

        if byte.is_ascii_alphanumeric() {
            prefix.push(byte.to_ascii_lowercase() as char);
        } else if byte == b'-' || byte == b'_' {
            prefix.push(byte as char);
        } else if !prefix.ends_with('-') {
            prefix.push('-');
        }
    }
    trim_anthropic_session_id_suffix(&mut prefix);
    prefix
}

fn trim_anthropic_session_id_suffix(value: &mut String) {
    while value.ends_with('-') || value.ends_with('_') {
        value.pop();
    }
}

fn anthropic_request_lowering_mode(
    request: &ProviderTurnRequest,
    cache_strategy: AnthropicCacheStrategy,
) -> &'static str {
    match cache_strategy {
        AnthropicCacheStrategy::ClaudeCodePromptCache => "claude_code_prompt_cache",
        AnthropicCacheStrategy::MessagesNative
            if request.prompt_frame.has_structured_system_blocks() =>
        {
            "prompt_cache_blocks"
        }
        AnthropicCacheStrategy::MessagesNative => "plain_system",
    }
}

fn build_anthropic_tools(request: &ProviderTurnRequest) -> Vec<Value> {
    let mut tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    if matches!(
        request.native_web_search.as_ref().map(|native| native.kind),
        Some(ProviderNativeWebSearchKind::Anthropic | ProviderNativeWebSearchKind::DeepSeek)
    ) {
        tools.push(json!({
            "type": request
                .native_web_search
                .as_ref()
                .map(|native| native.advertised_tool_type.as_str())
                .unwrap_or("web_search_20250305"),
            "name": "web_search"
        }));
    }
    if let Some(tool) = anthropic_response_format_tool(request) {
        tools.push(tool);
    }
    tools
}

fn anthropic_response_format_tool(request: &ProviderTurnRequest) -> Option<Value> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => Some(json!({
            "name": anthropic_response_format_tool_name(request)?,
            "description": format!(
                "Return the final response as a JSON object matching the `{}` schema.",
                format.name
            ),
            "input_schema": format.schema,
            "strict": format.strict,
        })),
    }
}

fn anthropic_response_format_tool_name(request: &ProviderTurnRequest) -> Option<String> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => {
            Some(format!("structured_response_{}", format.name))
        }
    }
}

fn anthropic_response_format_tool_choice(request: &ProviderTurnRequest) -> Option<Value> {
    Some(json!({
        "type": "tool",
        "name": anthropic_response_format_tool_name(request)?,
    }))
}

fn anthropic_response_format_tool_choice_for_model(
    request: &ProviderTurnRequest,
    route_provider: &str,
    model: &str,
) -> Result<Option<Value>> {
    let choice = anthropic_response_format_tool_choice(request);
    if requires_always_on_adaptive_thinking(route_provider, model) && choice.is_some() {
        return Err(anyhow!(
            "{model} does not support forced-tool response formats"
        ));
    }
    Ok(choice)
}

fn native_web_search_diagnostics(
    request: &ProviderTurnRequest,
) -> Option<ProviderNativeWebSearchDiagnostics> {
    let native = request.native_web_search.as_ref()?;
    let lowered = matches!(
        native.kind,
        ProviderNativeWebSearchKind::Anthropic | ProviderNativeWebSearchKind::DeepSeek
    );
    Some(ProviderNativeWebSearchDiagnostics {
        kind: native.kind,
        provider_id: native.provider_id.clone(),
        provider_model_ref: native.provider_model_ref.clone(),
        advertised_tool_type: native.advertised_tool_type.clone(),
        backend_kind: native.backend_kind.clone(),
        lowered,
        fallback_reason: (!lowered).then(|| {
            "anthropic transport only supports Anthropic/DeepSeek-native web search".into()
        }),
    })
}

fn response_format_diagnostics(
    request: &ProviderTurnRequest,
) -> Option<ProviderResponseFormatDiagnostics> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => {
            Some(ProviderResponseFormatDiagnostics {
                requested: true,
                lowered: true,
                format_type: "json_schema".into(),
                schema_name: Some(format.name.clone()),
                fallback_reason: None,
            })
        }
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
            exclude_tools: vec!["ApplyPatch"],
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
    cache_control: bool,
) -> Vec<ApiMessage> {
    repair_orphaned_tool_use_blocks(
        conversation
            .iter()
            .enumerate()
            .filter(|(_, message)| conversation_message_has_content(message))
            .map(|(message_index, message)| {
                conversation_message_to_api(
                    message,
                    rolling_cache_marker
                        .filter(|(marker_message_index, _)| *marker_message_index == message_index)
                        .map(|(_, marker_block_index)| marker_block_index),
                    cache_control,
                )
            })
            .collect(),
    )
}

/// Ensure every tool_use block in an assistant message is answered by
/// tool_result blocks in the immediately following user message. Turn
/// projection already synthesizes results for incomplete tool calls, so an
/// orphan here means context escaped that repair; sending it as-is would
/// make tool_use/tool_result-pairing providers reject the whole request
/// with HTTP 400. Repair by synthesizing failed tool_results before send.
fn repair_orphaned_tool_use_blocks(mut messages: Vec<ApiMessage>) -> Vec<ApiMessage> {
    for index in (0..messages.len()).rev() {
        if messages[index].role != "assistant" {
            continue;
        }
        let tool_use_ids = assistant_message_tool_use_ids(&messages[index]);
        if tool_use_ids.is_empty() {
            continue;
        }
        let following_is_user = messages
            .get(index + 1)
            .is_some_and(|message| message.role == "user");
        let missing_ids: Vec<String> = if following_is_user {
            let answered_ids = user_message_tool_result_ids(&messages[index + 1]);
            tool_use_ids
                .iter()
                .filter(|id| !answered_ids.contains(*id))
                .cloned()
                .collect()
        } else {
            tool_use_ids.clone()
        };
        if missing_ids.is_empty() {
            continue;
        }
        warn!(
            provider_transport = "anthropic_messages",
            tool_use_ids = ?missing_ids,
            "tool_use without matching tool_result; synthesizing failed tool_results before send"
        );
        if following_is_user {
            prepend_user_tool_result_blocks(&mut messages[index + 1], &missing_ids);
        } else {
            let mut message = ApiMessage {
                role: "user",
                content: Value::Array(
                    missing_ids
                        .iter()
                        .map(|id| synthesized_tool_result_block(id))
                        .collect(),
                ),
            };
            // The rolling cache marker may already mark the orphaned
            // assistant tail; move it so cache_control stays on the final
            // block of the repaired request.
            transfer_tail_cache_marker(&mut messages[index], &mut message);
            messages.insert(index + 1, message);
        }
    }
    messages
}

fn assistant_message_tool_use_ids(message: &ApiMessage) -> Vec<String> {
    let Value::Array(blocks) = &message.content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| {
            block
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect()
}

fn user_message_tool_result_ids(message: &ApiMessage) -> Vec<String> {
    let Value::Array(blocks) = &message.content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|block| {
            block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect()
}

fn synthesized_tool_result_block(tool_use_id: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": INCOMPLETE_TOOL_RESULT_SENTINEL,
        "is_error": true,
    })
}

/// Prepend synthesized tool_result blocks so a cache_control marker on the
/// message's last block stays last.
fn prepend_user_tool_result_blocks(message: &mut ApiMessage, tool_use_ids: &[String]) {
    let mut blocks: Vec<Value> = tool_use_ids
        .iter()
        .map(|id| synthesized_tool_result_block(id))
        .collect();
    match message.content.take() {
        Value::Array(existing) => blocks.extend(existing),
        Value::String(text) if !text.trim().is_empty() => {
            blocks.push(json!({ "type": "text", "text": text }))
        }
        _ => {}
    }
    message.content = Value::Array(blocks);
}

/// Move a `cache_control` marker from an orphaned assistant message onto the
/// synthesized tool_result message inserted after it, so the rolling marker
/// stays anchored to the conversation tail after repair.
fn transfer_tail_cache_marker(assistant: &mut ApiMessage, inserted: &mut ApiMessage) {
    let Value::Array(blocks) = &mut assistant.content else {
        return;
    };
    let Some(marker) = blocks
        .iter_mut()
        .filter_map(|block| {
            block
                .as_object_mut()
                .and_then(|object| object.remove("cache_control"))
        })
        .next()
    else {
        return;
    };
    let Value::Array(inserted_blocks) = &mut inserted.content else {
        return;
    };
    if let Some(last) = inserted_blocks.last_mut() {
        last["cache_control"] = marker;
    }
}

/// Ollama's Anthropic-compatible `/v1/messages` endpoint rejects message
/// lists without any user text (e.g. pure tool-result rounds) with
/// `500 no user query found in messages`; see ollama/ollama#18303. Until
/// upstream ships a fix, keep non-empty user text in the latest user turn;
/// older user text may be discarded by Ollama's context truncation.
fn needs_placeholder_user_text(route_provider: &str) -> bool {
    route_provider == "ollama"
}

fn ensure_placeholder_user_text(messages: &mut Vec<ApiMessage>) {
    const PLACEHOLDER_USER_TEXT: &str = "(continue)";
    if messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .is_some_and(api_message_has_nonempty_user_text)
    {
        return;
    }
    let placeholder_block = || json!({ "type": "text", "text": PLACEHOLDER_USER_TEXT });
    if let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "user")
    {
        if let Some(blocks) = message.content.as_array_mut() {
            blocks.push(placeholder_block());
            return;
        }
    }
    messages.push(ApiMessage {
        role: "user",
        content: Value::Array(vec![placeholder_block()]),
    });
}

fn api_message_has_nonempty_user_text(message: &ApiMessage) -> bool {
    if message.role != "user" {
        return false;
    }
    match &message.content {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(blocks) => blocks.iter().any(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
        }),
        _ => false,
    }
}

fn conversation_message_has_content(message: &ConversationMessage) -> bool {
    match message {
        ConversationMessage::UserText(text) => !text.trim().is_empty(),
        ConversationMessage::UserBlocks(blocks) => !blocks.is_empty(),
        ConversationMessage::UserImage {
            prompt,
            data_base64,
            ..
        } => !prompt.trim().is_empty() || !data_base64.is_empty(),
        ConversationMessage::AssistantBlocks(blocks) => !blocks.is_empty(),
        ConversationMessage::UserToolResults(results) => !results.is_empty(),
    }
}

fn conversation_message_to_api(
    message: &ConversationMessage,
    rolling_cache_block_index: Option<usize>,
    cache_control: bool,
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
                            prompt_block_to_anthropic_content(block, cache_control),
                            rolling_cache_block_index == Some(block_index),
                        )
                    })
                    .collect(),
            ),
        },
        ConversationMessage::UserImage {
            prompt,
            media_type,
            data_base64,
            ..
        } => ApiMessage {
            role: "user",
            content: Value::Array(
                [
                    (!prompt.trim().is_empty()).then(|| {
                        maybe_mark_cache_control(
                            json!({
                                "type": "text",
                                "text": prompt,
                            }),
                            rolling_cache_block_index == Some(0),
                        )
                    }),
                    (!data_base64.is_empty()).then(|| {
                        json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data_base64,
                            },
                        })
                    }),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
        },
        ConversationMessage::AssistantBlocks(blocks) => ApiMessage {
            role: "assistant",
            content: Value::Array(
                blocks
                    .iter()
                    .filter(|block| {
                        !matches!(
                            block,
                            ModelBlock::ReasoningText { .. } | ModelBlock::Citations { .. }
                        )
                    })
                    .enumerate()
                    .map(|(block_index, block)| {
                        maybe_mark_cache_control(
                            match block {
                                ModelBlock::Text { text } => json!({
                                    "type": "text",
                                    "text": text,
                                }),
                                ModelBlock::ToolUse {
                                    id, name, input, ..
                                } => json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input,
                                }),
                                ModelBlock::ProviderToolUse {
                                    id,
                                    name,
                                    input,
                                    status,
                                    provider_data,
                                } => anthropic_provider_block_payload(provider_data)
                                    .unwrap_or_else(|| {
                                        let mut value = json!({
                                            "type": "server_tool_use",
                                            "id": id,
                                            "name": name,
                                            "input": input,
                                        });
                                        if let Some(status) = status {
                                            value["status"] = json!(status);
                                        }
                                        value
                                    }),
                                ModelBlock::ProviderToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                    status,
                                    provider_data,
                                } => anthropic_provider_block_payload(provider_data)
                                    .unwrap_or_else(|| {
                                        let mut value = json!({
                                            "type": "tool_result",
                                            "content": content,
                                        });
                                        if let Some(tool_use_id) = tool_use_id {
                                            value["tool_use_id"] = json!(tool_use_id);
                                        }
                                        if *is_error {
                                            value["is_error"] = json!(true);
                                        }
                                        if let Some(status) = status {
                                            value["status"] = json!(status);
                                        }
                                        value
                                    }),
                                ModelBlock::Thinking { text, signature } => {
                                    let mut v = json!({
                                        "type": "thinking",
                                        "thinking": text,
                                    });
                                    if !signature.is_empty() {
                                        v.as_object_mut()
                                            .unwrap()
                                            .insert("signature".into(), json!(signature));
                                    }
                                    v
                                }
                                ModelBlock::RedactedThinking { data } => json!({
                                    "type": "redacted_thinking",
                                    "data": data,
                                }),
                                ModelBlock::ReasoningText { .. } => unreachable!(
                                    "reasoning text blocks are filtered before Anthropic encoding"
                                ),
                                ModelBlock::Citations { .. } => unreachable!(
                                    "citation blocks are filtered before Anthropic encoding"
                                ),
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

fn anthropic_provider_block_payload(provider_data: &Option<ProviderBlockData>) -> Option<Value> {
    provider_data
        .as_ref()
        .filter(|data| data.format == ANTHROPIC_PROVIDER_BLOCK_FORMAT)
        .map(|data| data.payload.clone())
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
    cache_control: bool,
) -> Option<(usize, usize)> {
    if !cache_control {
        return None;
    }
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
        ConversationMessage::UserBlocks(blocks) => blocks.iter().rposition(|block| {
            block.stability != PromptStability::TurnScoped || block.cache_breakpoint
        }),
        ConversationMessage::UserImage { prompt, .. } => (!prompt.trim().is_empty()).then_some(0),
        ConversationMessage::AssistantBlocks(blocks) => match cache_strategy {
            AnthropicCacheStrategy::MessagesNative => {
                (!blocks.is_empty()).then_some(blocks.len() - 1)
            }
            AnthropicCacheStrategy::ClaudeCodePromptCache => {
                blocks.iter().enumerate().rev().find_map(|(index, block)| {
                    matches!(block, ModelBlock::Text { .. }).then_some(index)
                })
            }
        },
        ConversationMessage::UserToolResults(results) => match cache_strategy {
            AnthropicCacheStrategy::MessagesNative => {
                (!results.is_empty()).then_some(results.len() - 1)
            }
            AnthropicCacheStrategy::ClaudeCodePromptCache => {
                (!results.is_empty()).then_some(results.len() - 1)
            }
        },
    }
}

fn prompt_block_to_anthropic_content(block: &PromptContentBlock, cache_control: bool) -> Value {
    let mut content = json!({
        "type": "text",
        "text": block.text,
    });
    if block.cache_breakpoint && cache_control {
        content["cache_control"] = json!({ "type": "ephemeral" });
    }
    content
}

fn warn_unsupported_anthropic_response_block(
    block: &ApiResponseBlock,
    provider_id: &str,
    model: &str,
    block_index: usize,
    stop_reason: Option<&str>,
    tools_available: bool,
) {
    if matches!(
        block.kind.as_str(),
        "text" | "tool_use" | "thinking" | "redacted_thinking" | "server_tool_use"
    ) || is_provider_tool_result_kind(&block.kind)
    {
        return;
    }

    warn!(
        provider = provider_id,
        model,
        block_index,
        block_type = %block.kind,
        stop_reason,
        tools_available,
        "anthropic response contained unsupported content block"
    );
}

fn anthropic_response_has_text_form_tool_call_violation(
    blocks: &[ApiResponseBlock],
    stop_reason: Option<&str>,
    tools_available: bool,
) -> bool {
    if stop_reason != Some("tool_use") || !tools_available {
        return false;
    }
    let mut has_text_form_tool_call = false;
    for block in blocks {
        if block.kind == "tool_use" {
            return false;
        }
        if block.kind == "text"
            && block
                .text
                .as_deref()
                .is_some_and(text_contains_tool_call_markup)
        {
            has_text_form_tool_call = true;
        }
    }
    has_text_form_tool_call
}

/// #2902: some Anthropic-compatible gateways intermittently return
/// `stop_reason=tool_use` (with tools available) but zero tool-call blocks,
/// so the round cannot be replayed as a tool round. Unlike the text-form
/// markup violation this is retryable: production evidence shows the same
/// binary passes on retry, and completing it as a text-only round would
/// silently drop the tool step.
fn anthropic_response_missing_tool_call_blocks(
    blocks: &[ApiResponseBlock],
    stop_reason: Option<&str>,
    tools_available: bool,
) -> bool {
    stop_reason == Some("tool_use")
        && tools_available
        && !blocks
            .iter()
            .any(|block| matches!(block.kind.as_str(), "tool_use" | "server_tool_use"))
}

fn text_contains_tool_call_markup(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("<tool_call")
        || lowered.contains("<function=")
        || lowered.contains("<function name=")
        || lowered.contains("&lt;tool_call")
        || lowered.contains("&lt;function=")
        || lowered.contains("&lt;function name=")
        || text.contains("<｜｜DSML｜｜tool_calls>")
        || text.contains("<｜｜DSML｜｜invoke")
}

fn api_response_block_to_model(
    block: ApiResponseBlock,
    response_format_tool_name: Option<&str>,
) -> Option<ModelBlock> {
    let provider_data = serde_json::to_value(&block)
        .ok()
        .map(|payload| ProviderBlockData {
            format: ANTHROPIC_PROVIDER_BLOCK_FORMAT.to_string(),
            payload,
        });
    match block.kind.as_str() {
        "text" => Some(ModelBlock::Text {
            text: block.text.unwrap_or_default(),
        }),
        "tool_use" => {
            let name = block.name?;
            let input = block.input.unwrap_or_else(|| json!({}));
            if Some(name.as_str()) == response_format_tool_name {
                return Some(ModelBlock::Text {
                    text: serde_json::to_string(&input).ok()?,
                });
            }
            Some(ModelBlock::ToolUse {
                id: block.id?,
                name,
                input,
                kind: ModelToolCallKind::Function,
                provider_data: None,
            })
        }
        "thinking" => Some(ModelBlock::Thinking {
            text: block.thinking.unwrap_or_default(),
            signature: block.signature.unwrap_or_default(),
        }),
        "redacted_thinking" => Some(ModelBlock::RedactedThinking {
            data: block.data.unwrap_or_default(),
        }),
        "server_tool_use" => Some(ModelBlock::ProviderToolUse {
            id: block.id?,
            name: block.name?,
            input: block.input.unwrap_or_else(|| json!({})),
            status: block.status,
            provider_data,
        }),
        kind if is_provider_tool_result_kind(kind) => Some(ModelBlock::ProviderToolResult {
            tool_use_id: block.tool_use_id,
            content: block.content.unwrap_or(Value::Null),
            is_error: block.is_error.unwrap_or(false),
            status: block.status,
            provider_data,
        }),
        _ => None,
    }
}

fn is_provider_tool_result_kind(kind: &str) -> bool {
    kind == "tool_result" || kind.ends_with("_tool_result")
}

fn strip_zai_redundant_provider_tool_text(blocks: &mut Vec<ApiResponseBlock>) {
    let mut remove = vec![false; blocks.len()];
    for (index, window) in blocks.windows(4).enumerate() {
        let [input_text, tool_use, output_text, tool_result] = window else {
            continue;
        };
        if is_zai_provider_tool_display_sequence(input_text, tool_use, output_text, tool_result) {
            remove[index] = true;
            remove[index + 2] = true;
        }
    }

    let mut index = 0;
    blocks.retain(|_| {
        let keep = !remove[index];
        index += 1;
        keep
    });
}

fn is_zai_provider_tool_display_sequence(
    input_text: &ApiResponseBlock,
    tool_use: &ApiResponseBlock,
    output_text: &ApiResponseBlock,
    tool_result: &ApiResponseBlock,
) -> bool {
    if input_text.kind != "text"
        || tool_use.kind != "server_tool_use"
        || output_text.kind != "text"
        || !is_provider_tool_result_kind(&tool_result.kind)
        || tool_use.id.as_deref() != tool_result.tool_use_id.as_deref()
    {
        return false;
    }

    let Some(name) = tool_use.name.as_deref() else {
        return false;
    };
    let Some(input) = tool_use.input.as_ref() else {
        return false;
    };
    let Some(displayed_input) =
        parse_zai_provider_tool_input_display(input_text.text.as_deref().unwrap_or_default(), name)
    else {
        return false;
    };
    if &displayed_input != input {
        return false;
    }

    let output_prefix = format!("**Output:**\n**{name}_result_summary:** ");
    output_text
        .text
        .as_deref()
        .is_some_and(|text| text.starts_with(&output_prefix))
}

fn parse_zai_provider_tool_input_display(text: &str, expected_name: &str) -> Option<Value> {
    let name = text
        .strip_prefix("**🌐 Z.ai Built-in Tool: ")?
        .split_once("**\n\n**Input:**\n```json\n")?;
    if name.0 != expected_name {
        return None;
    }
    let json = name.1.strip_suffix("\n```\n*Executing on server...*\n")?;
    serde_json::from_str(json).ok()
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
    let (rolling_marker_lag_messages, rolling_marker_at_tail) =
        rolling_marker_diagnostics(conversation, rolling_cache_marker);

    let turn_scoped_context_prefix_blocks = request
        .prompt_frame
        .context_blocks
        .iter()
        .filter(|block| block.stability == PromptStability::TurnScoped)
        .count();

    AnthropicPromptCacheDiagnostics {
        cache_strategy: cache_strategy.as_str().to_string(),
        model: model.to_string(),
        model_route: None,
        explicit_cache_control_enabled: None,
        cache_control_source: None,
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
        rolling_marker_lag_messages,
        rolling_marker_at_tail,
        turn_scoped_context_tail_blocks: 0,
        turn_scoped_context_prefix_blocks,
        cache_breakpoints,
        tokens_before_last_breakpoint,
        tokens_after_last_breakpoint,
        automatic_cache_control_requested: false,
    }
}

fn rolling_marker_diagnostics(
    conversation: &[ConversationMessage],
    rolling_cache_marker: Option<(usize, usize)>,
) -> (usize, bool) {
    let Some((marker_message_index, _)) = rolling_cache_marker else {
        return (conversation.len(), false);
    };
    let Some(last_content_message_index) = conversation
        .iter()
        .rposition(conversation_message_has_content)
    else {
        return (0, false);
    };
    (
        last_content_message_index.saturating_sub(marker_message_index),
        marker_message_index == last_content_message_index,
    )
}

fn hash_tools(tools: &[holon_domain::ToolSpec]) -> String {
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

fn stability_label(stability: holon_domain::PromptStability) -> &'static str {
    match stability {
        holon_domain::PromptStability::Stable => "stable",
        holon_domain::PromptStability::AgentScoped => "agent_scoped",
        holon_domain::PromptStability::TurnScoped => "turn_scoped",
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
            ConversationMessage::UserImage { prompt, .. } => {
                let block_tokens = estimate_tokens_from_chars(prompt.len());
                if rolling_cache_marker == Some((msg_idx, 0)) {
                    breakpoints.push(CacheBreakpointInfo {
                        location: format!("messages[{}].content[0]", msg_idx),
                        provider_payload_path: format!("messages[{}].content[0]", msg_idx),
                        block_kind: "user_text".to_string(),
                        stability: "conversation_tail".to_string(),
                        estimated_prefix_tokens: token_offset + block_tokens,
                        content_hash: hash_secret_safe_string(prompt),
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
    if cache_strategy == AnthropicCacheStrategy::MessagesNative {
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
        ModelBlock::ProviderToolUse { .. } => "provider_tool_use",
        ModelBlock::ProviderToolResult { .. } => "provider_tool_result",
        ModelBlock::Thinking { .. } => "thinking",
        ModelBlock::ReasoningText { .. } => "reasoning_text",
        ModelBlock::RedactedThinking { .. } => "redacted_thinking",
        ModelBlock::Citations { .. } => "citations",
    }
}

fn hash_model_block(block: &ModelBlock) -> String {
    match block {
        ModelBlock::Text { text } => {
            let value = json!({ "type": "text", "text": text });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::Thinking { text, .. } => {
            // Include a stable prefix so thinking blocks hash differently from text blocks
            let value = json!({ "type": "thinking", "thinking": text });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::ReasoningText { text } => {
            let value = json!({ "type": "reasoning", "text": text });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::RedactedThinking { data } => {
            let value = json!({ "type": "redacted_thinking", "data": data });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::ToolUse {
            id, name, input, ..
        } => {
            let value = json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::ProviderToolUse {
            id,
            name,
            input,
            status,
            provider_data,
        } => {
            let value = json!({
                "type": "provider_tool_use",
                "id": id,
                "name": name,
                "input": input,
                "status": status,
                "provider_data": provider_data,
            });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::ProviderToolResult {
            tool_use_id,
            content,
            is_error,
            status,
            provider_data,
        } => {
            let value = json!({
                "type": "provider_tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
                "status": status,
                "provider_data": provider_data,
            });
            sha256_hex(canonical_json(&value).as_bytes())
        }
        ModelBlock::Citations { citations } => {
            let value = json!({ "type": "citations", "citations": citations });
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
        ModelBlock::ProviderToolUse {
            input,
            provider_data,
            ..
        } => provider_data
            .as_ref()
            .and_then(|data| serde_json::to_string(&data.payload).ok())
            .map_or_else(
                || estimate_tokens_from_chars(input.to_string().len()),
                |payload| estimate_tokens_from_chars(payload.len()),
            ),
        ModelBlock::ProviderToolResult {
            content,
            provider_data,
            ..
        } => provider_data
            .as_ref()
            .and_then(|data| serde_json::to_string(&data.payload).ok())
            .map_or_else(
                || estimate_tokens_from_chars(content.to_string().len()),
                |payload| estimate_tokens_from_chars(payload.len()),
            ),
        ModelBlock::Thinking { text, .. } => estimate_tokens_from_chars(text.len()),
        ModelBlock::ReasoningText { text } => estimate_tokens_from_chars(text.len()),
        ModelBlock::RedactedThinking { data } => estimate_tokens_from_chars(data.len()),
        ModelBlock::Citations { citations } => citations
            .iter()
            .map(|citation| {
                citation.url.len() + citation.title.as_ref().map(String::len).unwrap_or_default()
            })
            .map(estimate_tokens_from_chars)
            .sum(),
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
mod tests;
