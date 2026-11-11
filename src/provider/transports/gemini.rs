use anyhow::{anyhow, Result};
use async_trait::async_trait;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{
    config::ProviderRuntimeConfig,
    provider::{
        http_trace::{ProviderHttpTrace, ProviderHttpTraceRequest},
        AgentProvider, ConversationMessage, ModelBlock, ModelToolCallKind, ProviderBlockData,
        ProviderCacheUsage, ProviderContextBudgetDiagnostics, ProviderImageData,
        ProviderPromptCapability, ProviderRequestDiagnostics, ProviderResponseFormatDiagnostics,
        ProviderResponseFormatRequest, ProviderTransportDiagnostics, ProviderTransportTimeline,
        ProviderTurnRequest, ProviderTurnResponse,
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
    base_url: String,
    api_key: String,
    quota_identity: Option<crate::provider::ProviderQuotaIdentity>,
    model: String,
    model_ref: String,
    max_output_tokens: u32,
    context_window_tokens: Option<usize>,
    trace_home_dir: PathBuf,
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
    ) -> Result<Self> {
        Self::from_runtime_config_with_context_window(
            provider_config,
            model,
            max_output_tokens,
            None,
            trace_home_dir,
        )
    }

    pub fn from_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
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
            api_key,
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
        })
    }
}

#[async_trait]
impl AgentProvider for GeminiProvider {
    fn quota_identity(&self) -> Option<crate::provider::ProviderQuotaIdentity> {
        self.quota_identity.clone()
    }

    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
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
                let headers = vec![
                    ("content-type", "application/json".to_string()),
                    ("x-goog-api-key", "[redacted]".to_string()),
                ];
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
                let request_builder = self
                    .client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("x-goog-api-key", &self.api_key)
                    .body(material.json_bytes(&request_payload)?);
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

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("gemini/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        vec![ProviderPromptCapability::FullRequestOnly]
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
    let mut text = String::new();
    if request.prompt_frame.has_structured_system_blocks() {
        for block in &request.prompt_frame.system_blocks {
            append_text_block(&mut text, &block.text);
        }
    } else {
        text = request.prompt_frame.system_prompt.clone();
    }
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

fn append_text_block(target: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !target.trim().is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(text);
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
    let max_bytes = crate::tool::tools::view_image::MAX_IMAGE_BYTES as usize;
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
    if request.tools.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "function_declarations": request.tools.iter().map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": gemini_safe_schema(&tool.input_schema),
            })
        }).collect::<Vec<_>>()
    })]
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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_refusals_fail_closed_with_usage() {
        for reason in [
            "SAFETY",
            "RECITATION",
            "BLOCKLIST",
            "PROHIBITED_CONTENT",
            "SPII",
            "IMAGE_SAFETY",
            "IMAGE_PROHIBITED_CONTENT",
            "IMAGE_RECITATION",
        ] {
            for content in [
                Value::Null,
                json!({}),
                json!({"parts": []}),
                json!({"parts": [{"text": ""}]}),
                json!({"parts": [{"text": " \n\t"}]}),
                json!({"parts": [{"text": "partial response"}]}),
                json!({"parts": [{"functionCall": {"name": "ProbeTool", "args": {}}}]}),
                json!({"parts": [{"functionCall": {"args": {}}}]}),
                json!({"parts": "secret-invalid-parts"}),
                json!("secret-invalid-content"),
            ] {
                let parsed = serde_json::from_value(json!({
                    "candidates": [{"content": content, "finishReason": reason}],
                    "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 0}
                }))
                .unwrap();
                let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
                    .expect_err("rejected content must never become a successful round");
                let transport = error
                    .downcast_ref::<crate::provider::ProviderTransportError>()
                    .expect("typed refusal");
                assert_eq!(transport.code.as_deref(), Some("gemini_content_blocked"));
                assert_eq!(
                    transport.classification.kind,
                    ProviderFailureKind::InvalidResponse
                );
                assert_eq!(
                    transport.classification.disposition,
                    RetryDisposition::FailFast
                );
                assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 11);
                assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 0);
                assert!(error.to_string().contains(reason));
                assert!(!error.to_string().contains("partial response"));
                assert!(!error.to_string().contains("ProbeTool"));
            }
        }
    }

    #[test]
    fn gemini_refusal_keeps_trace_identity_and_sanitized_route_diagnostics() {
        let home = tempfile::tempdir().unwrap();
        let trace = ProviderHttpTrace::failure_only_for_tests(home.path());
        let model_ref = "gemini@alternate/test";
        let url = "https://user:secret-password@example.com/v1?key=secret-key#secret-fragment";
        let request_trace = trace
            .begin_request(
                Some("gemini-refusal-test"),
                "gemini",
                Some(model_ref),
                url,
                "gemini_generate_content",
                &[],
                &json!({}),
            )
            .unwrap();
        let parsed = serde_json::from_value(json!({
            "candidates": [{"finishReason": "PROHIBITED_CONTENT"}]
        }))
        .unwrap();
        let error = gemini_response_to_provider_turn_response(
            parsed,
            Some(model_ref),
            Some(url),
            Some(&request_trace),
        )
        .unwrap_err();
        let diagnostics = crate::provider::provider_transport_diagnostics(&error).unwrap();
        assert_eq!(diagnostics.stage, "response_protocol");
        assert_eq!(diagnostics.model_ref.as_deref(), Some(model_ref));
        assert_eq!(diagnostics.streaming, Some(false));
        let capture = diagnostics
            .http_trace
            .as_ref()
            .expect("failure trace identity");
        assert!(!capture.capture_id.is_empty());
        assert!(std::path::Path::new(&capture.path).is_file());
        assert!(!format!("{error:?} {diagnostics:?}").contains("secret-"));
    }

    #[test]
    fn gemini_response_preserves_empty_blocks_with_finish_reason_and_usage() {
        let response = gemini_response_to_provider_turn_response(
            GenerateContentResponse {
                candidates: vec![GeminiCandidate {
                    content: Some(json!({"role": "model", "parts": []})),
                    finish_reason: Some("MAX_TOKENS".to_string()),
                }],
                usage_metadata: Some(GeminiUsageMetadata {
                    prompt_token_count: Some(11),
                    candidates_token_count: Some(7),
                }),
                prompt_feedback: None,
            },
            None,
            None,
            None,
        )
        .expect("empty Gemini content should still be a valid response");

        assert!(response.blocks.is_empty());
        assert_eq!(response.stop_reason.as_deref(), Some("MAX_TOKENS"));
        assert_eq!(response.input_tokens, 11);
        assert_eq!(response.output_tokens, 7);
    }

    #[test]
    fn gemini_response_accepts_content_without_parts() {
        let parsed: GenerateContentResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": {},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 11,
                "candidatesTokenCount": 7
            }
        }))
        .expect("Gemini content without parts should still deserialize");

        let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
            .expect_err("decodable STOP content without parts is a retryable empty response");
        let transport = error
            .downcast_ref::<crate::provider::ProviderTransportError>()
            .unwrap();
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::EmptyResponse
        );
        assert_eq!(
            transport.classification.disposition,
            RetryDisposition::Retryable
        );
        assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 11);
        assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 7);
    }

    #[test]
    fn gemini_prompt_feedback_blocks_without_candidates_and_redacts_unknown_reasons() {
        for candidates in [
            None,
            Some(json!([])),
            Some(json!([{"content": {"parts": [{"functionCall": {"args": {}}}]}}])),
        ] {
            for reason in ["SAFETY", "OTHER", "secret-unrecognized-reason"] {
                let mut body = json!({
                    "promptFeedback": {"blockReason": reason, "blockReasonMessage": "secret-message"},
                    "usageMetadata": {"promptTokenCount": 13, "candidatesTokenCount": 2}
                });
                if let Some(candidates) = candidates.as_ref() {
                    body["candidates"] = candidates.clone();
                }
                let parsed = serde_json::from_value(body).unwrap();
                let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
                    .expect_err("prompt rejection must be typed even without candidates");
                let transport = error
                    .downcast_ref::<crate::provider::ProviderTransportError>()
                    .unwrap();
                assert_eq!(transport.code.as_deref(), Some("gemini_content_blocked"));
                assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 13);
                assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 2);
                assert!(!format!("{transport:?}").contains("secret-"));
                if reason.starts_with("secret-") {
                    assert!(error.to_string().contains("UNKNOWN_BLOCK_REASON"));
                }
            }
        }
    }

    #[test]
    fn gemini_semantic_empty_responses_are_retryable_but_max_tokens_is_preserved() {
        for reason in [
            Value::Null,
            json!("STOP"),
            json!("UNRECOGNIZED"),
            json!("MAX_TOKENS"),
        ] {
            for content in [
                Value::Null,
                json!({}),
                json!({"parts": []}),
                json!({"parts": [{"text": ""}]}),
                json!({"parts": [{"text": " \n\t"}]}),
            ] {
                let parsed = serde_json::from_value(json!({
                    "candidates": [{"content": content, "finishReason": reason}],
                    "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 3}
                }))
                .unwrap();
                let result = gemini_response_to_provider_turn_response(parsed, None, None, None);
                if reason == "MAX_TOKENS" {
                    let response = result.unwrap();
                    assert_eq!(response.stop_reason.as_deref(), Some("MAX_TOKENS"));
                    assert_eq!((response.input_tokens, response.output_tokens), (5, 3));
                } else {
                    let error = result.expect_err("no semantic output");
                    let transport = error
                        .downcast_ref::<crate::provider::ProviderTransportError>()
                        .unwrap();
                    assert_eq!(
                        transport.classification.kind,
                        ProviderFailureKind::EmptyResponse
                    );
                    assert_eq!(
                        transport.classification.disposition,
                        RetryDisposition::Retryable
                    );
                    assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 5);
                    assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 3);
                }
            }
        }
        for body in [
            json!({}),
            json!({"candidates": []}),
            json!({
                "promptFeedback": {"blockReason": "BLOCK_REASON_UNSPECIFIED"}
            }),
        ] {
            let parsed = serde_json::from_value(body).unwrap();
            let error =
                gemini_response_to_provider_turn_response(parsed, None, None, None).unwrap_err();
            assert_eq!(
                crate::provider::classify_provider_error(&error).kind,
                ProviderFailureKind::EmptyResponse
            );
        }
    }

    #[test]
    fn gemini_non_refusal_text_and_signed_tools_remain_successful() {
        for reason in ["STOP", "MAX_TOKENS", "UNKNOWN_FUTURE_REASON"] {
            let parsed = serde_json::from_value(json!({
                "candidates": [{
                    "finishReason": reason,
                    "content": {"parts": [
                        {"text": " unmodified text "},
                        {"functionCall": {"name": "ProbeTool", "args": {"ok": true}}, "thoughtSignature": "signature"}
                    ]}
                }]
            })).unwrap();
            let response =
                gemini_response_to_provider_turn_response(parsed, None, None, None).unwrap();
            assert!(
                matches!(&response.blocks[0], ModelBlock::Text { text } if text == " unmodified text ")
            );
            assert!(
                matches!(&response.blocks[1], ModelBlock::ToolUse { provider_data: Some(data), .. }
                if data.payload["thoughtSignature"] == "signature")
            );
            assert_eq!(response.stop_reason.as_deref(), Some(reason));
        }
    }

    #[test]
    fn gemini_tool_use_ids_are_unique_and_responses_restore_function_name() {
        let blocks = gemini_parts_to_model_blocks(vec![
            GeminiPart {
                text: None,
                inline_data: None,
                function_call: Some(GeminiFunctionCall {
                    name: "ProbeTool".to_string(),
                    args: json!({"first": true}),
                }),
                function_response: None,
                thought_signature: None,
            },
            GeminiPart {
                text: None,
                inline_data: None,
                function_call: Some(GeminiFunctionCall {
                    name: "ProbeTool".to_string(),
                    args: json!({"second": true}),
                }),
                function_response: None,
                thought_signature: None,
            },
        ]);

        let first_id = match &blocks[0] {
            ModelBlock::ToolUse { id, name, .. } => {
                assert_eq!(name, "ProbeTool");
                id.clone()
            }
            other => panic!("expected first tool use, got {other:?}"),
        };
        let second_id = match &blocks[1] {
            ModelBlock::ToolUse { id, name, .. } => {
                assert_eq!(name, "ProbeTool");
                id.clone()
            }
            other => panic!("expected second tool use, got {other:?}"),
        };

        assert_ne!(first_id, second_id);
        assert_eq!(gemini_function_response_name(&first_id), "ProbeTool");
        assert_eq!(gemini_function_response_name(&second_id), "ProbeTool");
    }

    #[test]
    fn gemini_response_content_role_is_optional() {
        let response: GenerateContentResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "ProbeTool",
                            "args": {"token": "probe"}
                        },
                        "thoughtSignature": "opaque-signature"
                    }]
                },
                "finishReason": "STOP"
            }]
        }))
        .expect("Gemini response content should not require role");

        let content: GeminiContent = serde_json::from_value(
            response.candidates[0]
                .content
                .clone()
                .expect("candidate should include content"),
        )
        .expect("Gemini response content should not require role");
        assert!(content.role.is_empty());
        assert_eq!(
            content.parts[0].thought_signature.as_deref(),
            Some("opaque-signature")
        );
    }

    #[test]
    fn gemini_tool_use_thought_signature_survives_persistence_and_replay() {
        let signature = "opaque-thought-signature";
        let blocks = gemini_parts_to_model_blocks(vec![GeminiPart {
            text: None,
            inline_data: None,
            function_call: Some(GeminiFunctionCall {
                name: "ProbeTool".to_string(),
                args: json!({"probe": true}),
            }),
            function_response: None,
            thought_signature: Some(signature.to_string()),
        }]);

        let persisted = serde_json::to_vec(&blocks).expect("tool use should serialize");
        let restored: Vec<ModelBlock> =
            serde_json::from_slice(&persisted).expect("tool use should deserialize");
        let content =
            conversation_message_to_gemini_content(&ConversationMessage::AssistantBlocks(restored))
                .expect("assistant tool use should produce Gemini content");
        let payload = serde_json::to_value(content).expect("Gemini content should serialize");

        assert_eq!(payload["parts"][0]["functionCall"]["name"], "ProbeTool");
        assert_eq!(
            payload["parts"][0]["functionCall"]["args"],
            json!({"probe": true})
        );
        assert_eq!(payload["parts"][0]["thoughtSignature"], signature);
    }

    #[test]
    fn gemini_tool_use_without_provider_data_omits_thought_signature() {
        let content =
            conversation_message_to_gemini_content(&ConversationMessage::AssistantBlocks(vec![
                ModelBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "ProbeTool".to_string(),
                    input: json!({"probe": true}),
                    kind: ModelToolCallKind::Function,
                    provider_data: None,
                },
            ]))
            .expect("assistant tool use should produce Gemini content");
        let payload = serde_json::to_value(content).expect("Gemini content should serialize");

        assert!(payload["parts"][0].get("thoughtSignature").is_none());
    }

    #[test]
    fn gemini_tool_schemas_strip_fields_gemini_rejects() {
        let request_tools = json!({
            "type": "object",
            "properties": {
                "cmd": {"type": "string", "description": "command"},
                "labels": {
                    "type": "object",
                    "additionalProperties": true,
                    "title": "Labels",
                },
                "mode": {
                    "oneOf": [
                        {"type": "string", "enum": ["a"]},
                        {"type": "string", "enum": ["b"]},
                    ],
                },
                "maybe": {"type": ["string", "null"]},
            },
            "required": ["cmd"],
            "additionalProperties": false,
            "$schema": "http://json-schema.org/draft-07/schema#",
        });

        let lowered = gemini_safe_schema(&request_tools);
        let lowered_text = lowered.to_string();
        assert!(!lowered_text.contains("additionalProperties"));
        assert!(!lowered_text.contains("$schema"));
        assert!(!lowered_text.contains("title"));
        assert_eq!(lowered["type"], "object");
        assert_eq!(lowered["properties"]["labels"]["type"], "object");
        let mode_variants = lowered["properties"]["mode"]["anyOf"]
            .as_array()
            .expect("oneOf should lower to anyOf");
        assert_eq!(mode_variants.len(), 2);
        assert_eq!(lowered["properties"]["maybe"]["type"], "string");
        assert_eq!(lowered["properties"]["maybe"]["nullable"], true);
    }

    #[test]
    fn gemini_tool_schemas_lower_boolean_and_tuple_schemas() {
        assert_eq!(
            gemini_safe_schema(&Value::Bool(true)),
            json!({"type": "object"})
        );
        let tuple = gemini_safe_schema(&json!({
            "type": "array",
            "items": [{"type": "string"}, {"type": "number"}],
        }));
        assert_eq!(tuple["items"], json!({"type": "string"}));
    }
}
