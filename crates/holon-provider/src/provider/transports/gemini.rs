use anyhow::{anyhow, Result};
use async_trait::async_trait;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::types::Citation;
use crate::{
    config::{ProviderBuiltinWebSearchConfig, ProviderRuntimeConfig},
    provider::{
        builtin_web_search_probe_turn_request,
        http_trace::{ProviderHttpTrace, ProviderHttpTraceRequest},
        native_web_search_turn_request, AgentProvider, ConversationMessage, ModelBlock,
        ModelToolCallKind, ProviderBlockData, ProviderBuiltinWebSearchCapability,
        ProviderCacheUsage, ProviderContextBudgetDiagnostics, ProviderImageData,
        ProviderNativeSearchFailure, ProviderNativeSearchOutcome,
        ProviderNativeWebSearchDiagnostics, ProviderNativeWebSearchKind,
        ProviderNativeWebSearchRequest, ProviderPromptCapability, ProviderRequestDiagnostics,
        ProviderResponseFormatDiagnostics, ProviderResponseFormatRequest,
        ProviderTransportDiagnostics, ProviderTransportTimeline, ProviderTurnRequest,
        ProviderTurnResponse,
    },
};

use super::{build_http_client, response_body_timeout, send_provider_request, RequestDeadline};
use crate::provider::budget::{effective_output_tokens, estimate_context_budget, ContextBudget};
use crate::provider::retry::{
    classify_reqwest_transport_error_with_trace, classify_status_error_with_trace,
    empty_response_error_with_trace, invalid_response_error_with_trace, parse_retry_after,
    provider_transport_error_with_code, provider_transport_error_with_evidence,
    sanitize_transport_url, timeout_transport_error_with_trace, ProviderFailureClassification,
    ProviderFailureKind, RetryDisposition,
};
use crate::types::TokenUsage;

#[derive(Clone)]
pub struct GeminiProvider {
    client: Client,
    request_headers: crate::provider::request_headers::ProviderRequestHeaders,
    base_url: String,
    api_key: String,
    provider_id: String,
    quota_identity: Option<crate::provider::ProviderQuotaIdentity>,
    model: String,
    model_ref: String,
    max_output_tokens: u32,
    context_window_tokens: Option<usize>,
    trace_home_dir: PathBuf,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest {
    system_instruction: GeminiContent,
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_json_schema: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    role: String,
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<GeminiInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    #[serde(deserialize_with = "deserialize_gemini_image_data")]
    data: ProviderImageData,
}

fn deserialize_gemini_image_data<'de, D>(
    deserializer: D,
) -> std::result::Result<ProviderImageData, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Into::into)
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    usage_metadata: Option<GeminiUsageMetadata>,
    prompt_feedback: Option<GeminiPromptFeedback>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPromptFeedback {
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    // Decode partial content only after refusal metadata has been checked.
    content: Option<Value>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
}

impl GeminiProvider {
    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        holon_version: &'static str,
    ) -> Result<Self> {
        Self::from_runtime_config_with_context_window(
            provider_config,
            model,
            max_output_tokens,
            None,
            trace_home_dir,
            holon_version,
        )
    }

    pub fn from_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
        holon_version: &'static str,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = provider_config
            .credential
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                let credential_name = provider_config
                    .auth
                    .env
                    .as_deref()
                    .or(provider_config.auth.profile.as_deref())
                    .unwrap_or("configured credential");
                anyhow!("missing {credential_name}")
            })?;
        Ok(Self {
            client,
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            request_headers: crate::provider::request_headers::ProviderRequestHeaders::new(
                provider_config,
                holon_version,
            ),
            api_key,
            provider_id: provider_config.id.as_str().to_string(),
            quota_identity: crate::provider::ProviderQuotaIdentity::for_api_credential(
                provider_config,
            ),
            model: model.to_string(),
            model_ref: format!(
                "{}@{}/{}",
                provider_config.route_provider.as_str(),
                provider_config.route_endpoint.as_str(),
                model
            ),
            max_output_tokens,
            context_window_tokens,
            trace_home_dir: trace_home_dir.to_path_buf(),
            builtin_web_search: provider_config.builtin_web_search.clone(),
        })
    }

    /// Send one minimal generateContent request that carries the native web
    /// search tool, and return the raw response JSON so callers can parse
    /// grounding metadata. Only the turn query crosses the boundary.
    async fn send_generate_content_for_native_search(
        &self,
        request: &ProviderTurnRequest,
    ) -> std::result::Result<Value, anyhow::Error> {
        let url = format!(
            "{}/models/{}:generateContent",
            self.base_url,
            utf8_percent_encode(&self.model, NON_ALPHANUMERIC)
        );
        let body = GenerateContentRequest {
            system_instruction: build_system_instruction(request),
            contents: build_gemini_contents(request),
            tools: build_gemini_tools(request),
            generation_config: GeminiGenerationConfig {
                max_output_tokens: GEMINI_NATIVE_SEARCH_MAX_OUTPUT_TOKENS,
                response_mime_type: None,
                response_json_schema: None,
            },
        };
        let request_payload = request.material.json_value(&body)?;
        let headers = self.request_headers.compose(
            vec![
                ("content-type", "application/json".to_string()),
                ("x-goog-api-key", self.api_key.clone()),
            ],
            request.provider_session_id.as_ref(),
        )?;
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let request_trace = trace.and_then(|trace| {
            trace.begin_request(
                request
                    .prompt_frame
                    .cache
                    .as_ref()
                    .map(|cache| cache.agent_id.as_str()),
                "gemini",
                Some(&self.model_ref),
                url.as_str(),
                "generate_content",
                &headers,
                &request_payload,
            )
        });
        let body_bytes = request.material.json_bytes(&request_payload)?;
        let mut request_builder = self.client.post(&url).body(body_bytes);
        for (name, value) in &headers {
            request_builder = request_builder.header(*name, value);
        }
        let response = send_provider_request(
            request_builder,
            "Gemini request failed",
            "request_send",
            "gemini",
            Some(&self.model_ref),
            Some(url.as_str()),
            RequestDeadline::FullRequest,
            request_trace.as_ref(),
            request.dispatch_guard.as_deref(),
        )
        .await?;
        if let Some(trace) = request_trace.as_ref() {
            trace.write_response_headers(response.status(), response.headers());
        }
        if !response.status().is_success() {
            let status = response.status();
            let retry_after = parse_retry_after(response.headers());
            let body = super::read_error_response_body(
                response,
                request_trace.as_ref(),
                &request.material,
            )
            .await?;
            return Err(classify_status_error_with_trace(
                "Gemini request failed",
                "response_status",
                Some("gemini"),
                Some(&self.model_ref),
                Some(url.as_str()),
                status,
                body,
                request_trace.as_ref(),
                retry_after,
            ));
        }
        let text = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            Ok(Err(error)) => {
                return Err(classify_reqwest_transport_error_with_trace(
                    "Gemini response body failed",
                    "response_body",
                    "gemini",
                    Some(&self.model_ref),
                    Some(url.as_str()),
                    error,
                    request_trace.as_ref(),
                ));
            }
            Err(_elapsed) => {
                return Err(timeout_transport_error_with_trace(
                    "Gemini response body read timed out",
                    "response_body",
                    "gemini",
                    Some(&self.model_ref),
                    Some(url.as_str()),
                    format!("timed out after {:?}", response_body_timeout()),
                    request_trace.as_ref(),
                ));
            }
        };
        if let Some(trace) = request_trace.as_ref() {
            trace.write_response_body(&text);
        }
        serde_json::from_str(&text).map_err(|error| {
            invalid_response_error_with_trace(
                "invalid Gemini JSON",
                "response_protocol",
                "gemini",
                Some(&self.model_ref),
                Some(url.as_str()),
                format!(
                    "JSON {:?} error at line {} column {}",
                    error.classify(),
                    error.line(),
                    error.column()
                ),
                request_trace.as_ref(),
            )
        })
    }
}

#[async_trait]
impl AgentProvider for GeminiProvider {
    fn quota_identity(&self) -> Option<crate::provider::ProviderQuotaIdentity> {
        self.quota_identity.clone()
    }

    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.request_headers
            .require_session(request.provider_session_id.as_ref())?;
        let material = request.material.clone();
        material.validate_images(&request.conversation)?;
        material
            .complete_turn(async {
                let model_ref = self.model_ref.clone();
                let url = format!(
                    "{}/models/{}:generateContent",
                    self.base_url,
                    utf8_percent_encode(&self.model, NON_ALPHANUMERIC)
                );
                for message in &request.conversation {
                    if let ConversationMessage::UserImage {
                        media_type,
                        data_base64,
                        optional_history: false,
                        ..
                    } = message
                    {
                        if let Some(reason) = gemini_image_input_error(media_type, data_base64) {
                            return Err(gemini_preflight_error(
                                &model_ref,
                                &url,
                                "image_input_invalid",
                                reason,
                                None,
                            ));
                        }
                    }
                }
                let response_json_schema = request.response_format.as_ref().map(|format| match format {
                    ProviderResponseFormatRequest::JsonSchema(format) => format.schema.clone(),
                });
                let request_body = GenerateContentRequest {
                    system_instruction: build_system_instruction(&request),
                    contents: build_gemini_contents(&request),
                    tools: build_gemini_tools(&request),
                    generation_config: GeminiGenerationConfig {
                        max_output_tokens: self.max_output_tokens,
                        response_mime_type: response_json_schema.as_ref().map(|_| "application/json"),
                        response_json_schema,
                    },
                };
                let mut request_payload = material.json_value(&request_body)?;
                let output_fields = &["generationConfig.maxOutputTokens"];
                let effective_output = effective_output_tokens(
                    self.context_window_tokens,
                    self.max_output_tokens,
                    &request_payload,
                    output_fields,
                );
                request_payload["generationConfig"]["maxOutputTokens"] = Value::from(effective_output);
                if let Some(budget) = estimate_context_budget(
                    self.context_window_tokens,
                    effective_output,
                    &request_payload,
                    output_fields,
                )
                .filter(|budget| budget.overflow_tokens() > 0)
                {
                    return Err(gemini_preflight_error(
                        &model_ref,
                        &url,
                        "context_length_exceeded",
                        "Gemini request exceeds the estimated context budget; reduce prompt/context or requested output tokens, or increase the provider context window",
                        Some(budget),
                    ));
                }
                let headers = self.request_headers.compose(vec![
                    ("content-type", "application/json".to_string()),
                    ("x-goog-api-key", self.api_key.clone()),
                ], request.provider_session_id.as_ref())?;
                let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
                let request_trace = trace.and_then(|trace| {
                    trace.begin_request(
                        request
                            .prompt_frame
                            .cache
                            .as_ref()
                            .map(|cache| cache.agent_id.as_str()),
                        "gemini",
                        Some(&model_ref),
                        url.as_str(),
                        "generate_content",
                        &headers,
                        &request_payload,
                    )
                });
                let request_started_at = chrono::Utc::now();
                let mut request_builder = self
                    .client
                    .post(&url)
                    .body(material.json_bytes(&request_payload)?);
                for (name, value) in &headers {
                    request_builder = request_builder.header(*name, value);
                }
                let response = send_provider_request(
                    request_builder,
                    "Gemini request failed",
                    "request_send",
                    "gemini",
                    Some(&model_ref),
                    Some(url.as_str()),
                    RequestDeadline::FullRequest,
                    request_trace.as_ref(),
                    request.dispatch_guard.as_deref(),
                )
                .await?;
                let response_headers_at = chrono::Utc::now();
                if let Some(trace) = request_trace.as_ref() {
                    trace.write_response_headers(response.status(), response.headers());
                }
                if !response.status().is_success() {
                    let status = response.status();
                    let retry_after = parse_retry_after(response.headers());
                    let body = super::read_error_response_body(response, request_trace.as_ref(), &material).await?;
                    return Err(classify_status_error_with_trace(
                        "Gemini request failed",
                        "response_status",
                        Some("gemini"),
                        Some(&model_ref),
                        Some(url.as_str()),
                        status,
                        body,
                        request_trace.as_ref(),
                        retry_after,
                    ));
                }

                let response_body =
                    match tokio::time::timeout(response_body_timeout(), response.text()).await {
                        Ok(Ok(text)) => text,
                        Ok(Err(error)) => {
                            return Err(classify_reqwest_transport_error_with_trace(
                                "Gemini response body failed",
                                "response_body",
                                "gemini",
                                Some(&model_ref),
                                Some(url.as_str()),
                                error,
                                request_trace.as_ref(),
                            ));
                        }
                        Err(_elapsed) => {
                            return Err(timeout_transport_error_with_trace(
                                "Gemini response body read timed out",
                                "response_body",
                                "gemini",
                                Some(&model_ref),
                                Some(url.as_str()),
                                format!("timed out after {:?}", response_body_timeout()),
                                request_trace.as_ref(),
                            ));
                        }
                    };
                let response_body_completed_at = chrono::Utc::now();
                if let Some(trace) = request_trace.as_ref() {
                    trace.write_response_body(&response_body);
                }
                let parsed: GenerateContentResponse = serde_json::from_str(&response_body)
                    .map_err(|error| {
                        invalid_response_error_with_trace(
                            "invalid Gemini JSON",
                            "response_protocol",
                            "gemini",
                            Some(&model_ref),
                            Some(url.as_str()),
                            format!(
                                "JSON {:?} error at line {} column {}",
                                error.classify(), error.line(), error.column()
                            ),
                            request_trace.as_ref(),
                        )
                    })?;
                let mut response = gemini_response_to_provider_turn_response(
                    parsed, Some(&model_ref), Some(url.as_str()), request_trace.as_ref()
                )?;
                let parse_completed_at = chrono::Utc::now();
                let diagnostics = response
                    .request_diagnostics
                    .as_mut()
                    .expect("Gemini responses include request diagnostics");
                diagnostics.native_web_search = gemini_native_web_search_diagnostics(&request);
                diagnostics.response_format = request.response_format.as_ref().map(|format| match format {
                    ProviderResponseFormatRequest::JsonSchema(format) => {
                        ProviderResponseFormatDiagnostics {
                            requested: true,
                            lowered: true,
                            format_type: "json_schema".into(),
                            schema_name: Some(format.name.clone()),
                            fallback_reason: None,
                        }
                    }
                });
                diagnostics.transport_timeline = Some(ProviderTransportTimeline {
                    request_started_at,
                    response_headers_at,
                    response_body_completed_at: Some(response_body_completed_at),
                    parse_completed_at,
                    streaming: false,
                });
                Ok(response)
            })
            .await
    }

    #[cfg(any(test, feature = "runtime-tests"))]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("gemini/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        vec![ProviderPromptCapability::FullRequestOnly]
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.provider_id.clone(),
            provider_model_ref: self.model_ref.clone(),
            provider_transport: "gemini_generate_content".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        self.request_headers.require_session(None)?;
        let probe_request = builtin_web_search_probe_turn_request(request);
        self.send_generate_content_for_native_search(&probe_request)
            .await
            .map(|_| ())
    }

    /// Run one standalone Gemini native web search: the query rides a
    /// minimal generateContent request with the `google_search` grounding
    /// tool, and only structured grounding metadata becomes citations.
    async fn native_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
        query: &str,
    ) -> std::result::Result<ProviderNativeSearchOutcome, ProviderNativeSearchFailure> {
        self.request_headers
            .require_session(None)
            .map_err(super::openai::native_search_transport_failure)?;
        let turn_request = native_web_search_turn_request(request, query);
        let response = self
            .send_generate_content_for_native_search(&turn_request)
            .await
            .map_err(super::openai::native_search_transport_failure)?;
        Ok(parse_gemini_native_web_search_output(&response))
    }
}

fn gemini_preflight_error(
    model_ref: &str,
    url: &str,
    code: &str,
    message: &str,
    budget: Option<ContextBudget>,
) -> anyhow::Error {
    let diagnostics = ProviderTransportDiagnostics {
        stage: "request_preflight".to_string(),
        streaming: Some(false),
        provider: Some("gemini".to_string()),
        model_ref: Some(model_ref.to_string()),
        url: Some(sanitize_transport_url(url)),
        status: None,
        reqwest: None,
        context_budget: budget.map(|budget| ProviderContextBudgetDiagnostics {
            estimated_input_tokens: budget.estimated_input_tokens,
            estimated_text_tokens: budget.estimated_text_tokens,
            image_reserve_tokens: budget.image_reserve_tokens,
            image_count: budget.image_count,
            inline_image_base64_bytes: budget.inline_image_base64_bytes,
            estimated_inline_image_decoded_bytes: budget.estimated_inline_image_decoded_bytes,
            requested_output_tokens: budget.requested_output_tokens,
            safety_headroom_tokens: budget.safety_headroom_tokens,
            required_tokens: budget.required_tokens(),
            context_window_tokens: budget.context_window_tokens,
            overflow_tokens: budget.overflow_tokens(),
            suggestion: "reduce prompt/context or requested output tokens, or increase the provider context window".to_string(),
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
        Some(code),
        None,
        Some(diagnostics),
        message.to_string(),
    )
}

fn build_system_instruction(request: &ProviderTurnRequest) -> GeminiContent {
    let text = if request.prompt_frame.has_structured_system_blocks() {
        // Canonical blocks already own their separators. Adding delimiters
        // here makes Gemini's wire prompt differ from the rendered prompt.
        request
            .prompt_frame
            .system_blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect()
    } else {
        request.prompt_frame.system_prompt.clone()
    };
    GeminiContent {
        role: "user".to_string(),
        parts: vec![GeminiPart {
            text: Some(text),
            inline_data: None,
            function_call: None,
            function_response: None,
            thought_signature: None,
        }],
    }
}

fn build_gemini_contents(request: &ProviderTurnRequest) -> Vec<GeminiContent> {
    request
        .conversation
        .iter()
        .filter_map(conversation_message_to_gemini_content)
        .collect()
}

fn gemini_image_input_error(media_type: &str, data_base64: &str) -> Option<&'static str> {
    if !matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Some("Gemini image input has an unsupported media type");
    }
    if data_base64.is_empty() {
        return Some("Gemini image input has no image data");
    }
    let max_bytes = crate::provider::image_delivery::MAX_VIEW_IMAGE_BYTES as usize;
    if data_base64.len() > max_bytes.div_ceil(3) * 4 {
        return Some("Gemini image input exceeds the runtime per-image byte limit");
    }
    None
}

fn conversation_message_to_gemini_content(message: &ConversationMessage) -> Option<GeminiContent> {
    match message {
        ConversationMessage::UserText(text) => (!text.trim().is_empty()).then(|| GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(text.clone()),
                inline_data: None,
                function_call: None,
                function_response: None,
                thought_signature: None,
            }],
        }),
        ConversationMessage::UserBlocks(blocks) => {
            let parts = blocks
                .iter()
                .filter(|block| !block.text.trim().is_empty())
                .map(|block| GeminiPart {
                    text: Some(block.text.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| GeminiContent {
                role: "user".to_string(),
                parts,
            })
        }
        ConversationMessage::UserImage {
            prompt,
            media_type,
            data_base64,
            ..
        } => {
            // Runtime loading already validates local files and attachment budgets.
            let invalid_image = gemini_image_input_error(media_type, data_base64);
            let mut parts = Vec::new();
            if !prompt.trim().is_empty() {
                parts.push(GeminiPart {
                    text: Some(prompt.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                });
            }
            if let Some(reason) = invalid_image {
                parts.push(GeminiPart {
                    text: Some(format!("[image input omitted: {reason}]")),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                });
            } else {
                parts.push(GeminiPart {
                    text: None,
                    inline_data: Some(GeminiInlineData {
                        mime_type: media_type.clone(),
                        data: data_base64.clone(),
                    }),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                });
            }
            Some(GeminiContent {
                role: "user".to_string(),
                parts,
            })
        }
        ConversationMessage::AssistantBlocks(blocks) => {
            let parts = blocks
                .iter()
                .map(|block| match block {
                    ModelBlock::Text { text } => GeminiPart {
                        text: Some(text.clone()),
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                        thought_signature: None,
                    },
                    ModelBlock::ToolUse {
                        name,
                        input,
                        provider_data,
                        ..
                    } => GeminiPart {
                        text: None,
                        inline_data: None,
                        function_call: Some(GeminiFunctionCall {
                            name: name.clone(),
                            args: input.clone(),
                        }),
                        function_response: None,
                        thought_signature: gemini_thought_signature(provider_data),
                    },
                    ModelBlock::Thinking { .. }
                    | ModelBlock::ReasoningText { .. }
                    | ModelBlock::RedactedThinking { .. }
                    | ModelBlock::ProviderToolUse { .. }
                    | ModelBlock::ProviderToolResult { .. } => GeminiPart {
                        text: None,
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                        thought_signature: None,
                    },
                    ModelBlock::Citations { .. } => GeminiPart {
                        text: None,
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                        thought_signature: None,
                    },
                })
                .filter(|part| part.text.is_some() || part.function_call.is_some())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| GeminiContent {
                role: "model".to_string(),
                parts,
            })
        }
        ConversationMessage::UserToolResults(results) => {
            let parts = results
                .iter()
                .map(|result| {
                    let response_name = gemini_function_response_name(&result.tool_use_id);
                    GeminiPart {
                        text: None,
                        inline_data: None,
                        function_call: None,
                        function_response: Some(GeminiFunctionResponse {
                            name: response_name,
                            response: json!({
                                "content": result.content,
                                "is_error": result.is_error,
                            }),
                        }),
                        thought_signature: None,
                    }
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| GeminiContent {
                role: "user".to_string(),
                parts,
            })
        }
    }
}

fn gemini_refusal_reason(reason: &str) -> Option<&'static str> {
    match reason {
        "SAFETY" => Some("SAFETY"),
        "RECITATION" => Some("RECITATION"),
        "BLOCKLIST" => Some("BLOCKLIST"),
        "PROHIBITED_CONTENT" => Some("PROHIBITED_CONTENT"),
        "SPII" => Some("SPII"),
        "IMAGE_SAFETY" => Some("IMAGE_SAFETY"),
        "IMAGE_PROHIBITED_CONTENT" => Some("IMAGE_PROHIBITED_CONTENT"),
        "IMAGE_RECITATION" => Some("IMAGE_RECITATION"),
        _ => None,
    }
}

fn gemini_response_to_provider_turn_response(
    parsed: GenerateContentResponse,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<ProviderTurnResponse> {
    let usage = TokenUsage::new(
        parsed
            .usage_metadata
            .as_ref()
            .and_then(|usage| usage.prompt_token_count)
            .unwrap_or(0),
        parsed
            .usage_metadata
            .as_ref()
            .and_then(|usage| usage.candidates_token_count)
            .unwrap_or(0),
    );
    let candidate = parsed.candidates.into_iter().next();
    let prompt_block = parsed
        .prompt_feedback
        .as_ref()
        .and_then(|feedback| feedback.block_reason.as_deref())
        .filter(|reason| !matches!(*reason, "" | "BLOCK_REASON_UNSPECIFIED"));
    let refusal = prompt_block
        .map(|reason| {
            gemini_refusal_reason(reason).unwrap_or(if reason == "OTHER" {
                "OTHER"
            } else {
                "UNKNOWN_BLOCK_REASON"
            })
        })
        .or_else(|| {
            candidate
                .as_ref()
                .and_then(|candidate| candidate.finish_reason.as_deref())
                .and_then(gemini_refusal_reason)
        });
    // Refusal metadata takes precedence over any partial text or tool calls.
    if let Some(reason) = refusal {
        let message = format!("Gemini content blocked ({reason})");
        return Err(provider_transport_error_with_evidence(
            ProviderFailureClassification {
                kind: ProviderFailureKind::InvalidResponse,
                disposition: RetryDisposition::FailFast,
            },
            Some("gemini_content_blocked"),
            None,
            Some(ProviderTransportDiagnostics {
                stage: "response_protocol".into(),
                streaming: Some(false),
                provider: Some("gemini".into()),
                model_ref: model_ref.map(ToString::to_string),
                url: url.map(sanitize_transport_url),
                status: None,
                reqwest: None,
                context_budget: None,
                http_trace: trace.and_then(|trace| trace.diagnostics(None)),
                quota_identity: None,
                retry_after_not_before: None,
                throttle: None,
                source_chain: vec![message.clone()],
            }),
            Some(usage),
            None,
            message,
        ));
    }
    let stop_reason = candidate
        .as_ref()
        .and_then(|candidate| candidate.finish_reason.clone());
    let blocks = candidate
        .and_then(|candidate| candidate.content)
        .map(|content| {
            serde_json::from_value::<GeminiContent>(content).map_err(|error| {
                invalid_response_error_with_trace(
                    "invalid Gemini content",
                    "response_protocol",
                    "gemini",
                    model_ref,
                    url,
                    format!("JSON {:?} error in candidate content", error.classify()),
                    trace,
                )
            })
        })
        .transpose()?
        .map(|content| gemini_parts_to_model_blocks(content.parts))
        .unwrap_or_default();
    if stop_reason.as_deref() != Some("MAX_TOKENS")
        && !blocks.iter().any(|block| match block {
            ModelBlock::Text { text } => !text.trim().is_empty(),
            ModelBlock::ToolUse { .. } => true,
            _ => false,
        })
    {
        return Err(empty_response_error_with_trace(
            "empty Gemini response",
            "response_protocol",
            "gemini",
            model_ref,
            url,
            "response contained no usable text or tool calls",
            trace,
            usage,
        ));
    }
    Ok(ProviderTurnResponse {
        material: Default::default(),
        blocks,
        stop_reason,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_usage: Some(ProviderCacheUsage {
            read_input_tokens: 0,
            creation_input_tokens: 0,
        }),
        provider_message_id: None,
        provider_request_id: None,
        request_diagnostics: Some(ProviderRequestDiagnostics {
            request_lowering_mode: "gemini_generate_content".to_string(),
            provider_id: Some("gemini".to_string()),
            provider_model_ref: model_ref.map(ToString::to_string),
            provider_transport: Some("gemini_generate_content".to_string()),
            endpoint_dialect: Some("gemini".to_string()),
            streaming: Some(false),
            reasoning_effort: None,
            anthropic_cache: None,
            anthropic_context_management: None,
            openai_request_controls: None,
            incremental_continuation: None,
            openai_remote_compaction: None,
            native_web_search: None,
            response_format: None,
            stable_prefix: None,
            transport_timeline: None,
        }),
    })
}

fn build_gemini_tools(request: &ProviderTurnRequest) -> Vec<Value> {
    let mut tools = Vec::new();
    if !request.tools.is_empty() {
        tools.push(json!({
            "function_declarations": request.tools.iter().map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": gemini_safe_schema(&tool.input_schema),
                })
            }).collect::<Vec<_>>()
        }));
    }
    if let Some(tool) = gemini_native_web_search_tool(request) {
        tools.push(tool);
    }
    tools
}

const GEMINI_NATIVE_SEARCH_MAX_OUTPUT_TOKENS: u32 = 256;

/// Gemini grounds native web search through the `google_search` tool
/// declaration keyed by the advertised tool type; other native search kinds
/// have no Gemini lowering.
fn gemini_native_web_search_tool(request: &ProviderTurnRequest) -> Option<Value> {
    let native = request.native_web_search.as_ref()?;
    if native.kind != ProviderNativeWebSearchKind::Gemini {
        return None;
    }
    let mut tool = serde_json::Map::new();
    tool.insert(
        native.advertised_tool_type.clone(),
        Value::Object(Default::default()),
    );
    Some(Value::Object(tool))
}

fn gemini_native_web_search_diagnostics(
    request: &ProviderTurnRequest,
) -> Option<ProviderNativeWebSearchDiagnostics> {
    let native = request.native_web_search.as_ref()?;
    let lowered = native.kind == ProviderNativeWebSearchKind::Gemini;
    Some(ProviderNativeWebSearchDiagnostics {
        kind: native.kind,
        provider_id: native.provider_id.clone(),
        provider_model_ref: native.provider_model_ref.clone(),
        advertised_tool_type: native.advertised_tool_type.clone(),
        backend_kind: native.backend_kind.clone(),
        lowered,
        fallback_reason: (!lowered)
            .then(|| "gemini transport only supports Gemini-native web search".into()),
    })
}

/// Parse one standalone Gemini native web search response: `search_executed`
/// is true only when the response carries explicit grounding evidence
/// (`webSearchQueries` or grounding chunks), and citations come only from
/// structured `groundingChunks[].web` entries.
fn parse_gemini_native_web_search_output(response: &Value) -> ProviderNativeSearchOutcome {
    let grounding = response
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("groundingMetadata"));
    let grounding_queries = grounding
        .and_then(|metadata| metadata.get("webSearchQueries"))
        .and_then(Value::as_array)
        .is_some_and(|queries| !queries.is_empty());
    let grounding_chunks = grounding
        .and_then(|metadata| metadata.get("groundingChunks"))
        .and_then(Value::as_array);
    let search_executed =
        grounding_queries || grounding_chunks.is_some_and(|chunks| !chunks.is_empty());
    let citations = grounding_chunks
        .map(|chunks| {
            chunks
                .iter()
                .filter_map(|chunk| {
                    let web = chunk.get("web")?;
                    let url = web.get("uri").and_then(Value::as_str)?;
                    if url.is_empty() {
                        return None;
                    }
                    Some(Citation {
                        url: url.to_string(),
                        title: web.get("title").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ProviderNativeSearchOutcome {
        search_executed,
        citations,
    }
}

/// Gemini deserializes function-declaration `parameters` into its proto
/// `Schema` message, so any JSON-Schema keyword outside that message is
/// rejected with `400 INVALID_ARGUMENT` ("Unknown name ... Cannot find
/// field"). Schemars-generated holon tool schemas routinely carry keywords
/// such as `additionalProperties`, so lower every schema into the
/// Gemini-supported subset before sending it.
fn gemini_safe_schema(schema: &Value) -> Value {
    const GEMINI_SCHEMA_FIELDS: &[&str] = &[
        "format",
        "description",
        "nullable",
        "enum",
        "example",
        "items",
        "properties",
        "required",
        "propertyOrdering",
        "anyOf",
        "minItems",
        "maxItems",
        "minLength",
        "maxLength",
        "minProperties",
        "maxProperties",
        "minimum",
        "maximum",
        "pattern",
    ];

    match schema {
        Value::Object(object) => {
            let mut lowered = serde_json::Map::new();
            // JSON Schema allows `type` arrays such as ["string", "null"];
            // Gemini takes a single type plus an explicit `nullable` flag.
            let (type_value, nullable_from_type) = match object.get("type") {
                Some(Value::Array(types)) => (
                    types
                        .iter()
                        .find(|type_name| type_name.as_str() != Some("null"))
                        .cloned(),
                    types
                        .iter()
                        .any(|type_name| type_name.as_str() == Some("null")),
                ),
                other => (other.cloned(), false),
            };
            if let Some(type_value) = type_value {
                lowered.insert("type".to_string(), type_value);
            }
            for field in GEMINI_SCHEMA_FIELDS {
                if let Some(value) = object.get(*field) {
                    lowered.insert((*field).to_string(), value.clone());
                }
            }
            if nullable_from_type {
                lowered.insert("nullable".to_string(), Value::Bool(true));
            }
            // Gemini has no `oneOf`; `anyOf` is its supported alternative.
            if !lowered.contains_key("anyOf") {
                if let Some(one_of) = object.get("oneOf") {
                    lowered.insert("anyOf".to_string(), one_of.clone());
                }
            }
            if let Some(properties) = lowered.get_mut("properties").and_then(Value::as_object_mut) {
                for value in properties.values_mut() {
                    let lowered_value = gemini_safe_schema(value);
                    *value = lowered_value;
                }
            }
            if let Some(items_value) = lowered.get_mut("items") {
                // Draft 07 tuple-style `items` arrays have no Gemini
                // equivalent; keep only the first entry's schema.
                let lowered_items = match items_value {
                    Value::Array(entries) => entries.first().map(|first| gemini_safe_schema(first)),
                    ref value @ Value::Object(_) => Some(gemini_safe_schema(value)),
                    _ => None,
                };
                if let Some(lowered_items) = lowered_items {
                    *items_value = lowered_items;
                }
            }
            if let Some(any_of) = lowered.get_mut("anyOf").and_then(Value::as_array_mut) {
                for variant in any_of.iter_mut() {
                    let lowered_variant = gemini_safe_schema(variant);
                    *variant = lowered_variant;
                }
            }
            if lowered.is_empty() {
                lowered.insert("type".to_string(), Value::String("object".to_string()));
            }
            Value::Object(lowered)
        }
        // Draft 07 boolean schemas have no Gemini equivalent.
        Value::Bool(true) | Value::Bool(false) => json!({"type": "object"}),
        other => other.clone(),
    }
}

const GEMINI_TOOL_USE_ID_SEPARATOR: &str = "__holon_gemini_call_";
const GEMINI_TOOL_USE_PROVIDER_DATA_FORMAT: &str = "gemini.function_call";

fn gemini_thought_signature(provider_data: &Option<ProviderBlockData>) -> Option<String> {
    provider_data
        .as_ref()
        .filter(|data| data.format == GEMINI_TOOL_USE_PROVIDER_DATA_FORMAT)
        .and_then(|data| data.payload.get("thoughtSignature"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn gemini_parts_to_model_blocks(parts: Vec<GeminiPart>) -> Vec<ModelBlock> {
    parts
        .into_iter()
        .enumerate()
        .filter_map(|(index, part)| {
            if let Some(text) = part.text {
                return Some(ModelBlock::Text { text });
            }
            part.function_call.map(|call| {
                let provider_data =
                    part.thought_signature
                        .map(|thought_signature| ProviderBlockData {
                            format: GEMINI_TOOL_USE_PROVIDER_DATA_FORMAT.to_string(),
                            payload: json!({"thoughtSignature": thought_signature}),
                        });
                ModelBlock::ToolUse {
                    id: gemini_tool_use_id(&call.name, index),
                    name: call.name,
                    input: call.args,
                    kind: ModelToolCallKind::Function,
                    provider_data,
                }
            })
        })
        .collect()
}

fn gemini_tool_use_id(name: &str, index: usize) -> String {
    format!("{name}{GEMINI_TOOL_USE_ID_SEPARATOR}{index}")
}

fn gemini_function_response_name(tool_use_id: &str) -> String {
    tool_use_id
        .rsplit_once(GEMINI_TOOL_USE_ID_SEPARATOR)
        .map(|(name, _)| name)
        .unwrap_or(tool_use_id)
        .to_string()
}

#[cfg(test)]
mod tests;
