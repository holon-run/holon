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
        http_trace::ProviderHttpTrace, AgentProvider, ConversationMessage, ModelBlock,
        ModelToolCallKind, ProviderCacheUsage, ProviderPromptCapability,
        ProviderRequestDiagnostics, ProviderTurnRequest, ProviderTurnResponse,
    },
};

use super::{build_http_client, request_send_timeout, response_body_timeout};
use crate::provider::retry::{
    classify_reqwest_transport_error_with_trace, classify_status_error_with_trace,
    invalid_response_error, timeout_transport_error_with_trace,
};

#[derive(Clone)]
pub struct GeminiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_output_tokens: u32,
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
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
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
    candidates: Vec<GeminiCandidate>,
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
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
            model: model.to_string(),
            max_output_tokens,
            trace_home_dir: trace_home_dir.to_path_buf(),
        })
    }
}

#[async_trait]
impl AgentProvider for GeminiProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let request_body = GenerateContentRequest {
            system_instruction: build_system_instruction(&request),
            contents: build_gemini_contents(&request),
            tools: build_gemini_tools(&request),
            generation_config: GeminiGenerationConfig {
                max_output_tokens: self.max_output_tokens,
            },
        };
        let request_payload = serde_json::to_value(&request_body)?;
        let model_ref = format!("gemini/{}", self.model);
        let url = format!(
            "{}/models/{}:generateContent",
            self.base_url,
            utf8_percent_encode(&self.model, NON_ALPHANUMERIC)
        );
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
        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .timeout(request_send_timeout())
            .json(&request_body)
            .send()
            .await
            .map_err(|error| {
                classify_reqwest_transport_error_with_trace(
                    "Gemini request failed",
                    "request_send",
                    "gemini",
                    Some(&model_ref),
                    Some(url.as_str()),
                    error,
                    request_trace.as_ref(),
                )
            })?;
        if let Some(trace) = request_trace.as_ref() {
            trace.write_response_headers(response.status(), response.headers());
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
                Ok(Ok(text)) => text,
                _ => String::new(),
            };
            if let Some(trace) = request_trace.as_ref() {
                trace.write_response_body(&body);
            }
            return Err(classify_status_error_with_trace(
                "Gemini request failed",
                "response_status",
                Some("gemini"),
                Some(&model_ref),
                Some(url.as_str()),
                status,
                body,
                request_trace.as_ref(),
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
        if let Some(trace) = request_trace.as_ref() {
            trace.write_response_body(&response_body);
        }
        let parsed: GenerateContentResponse = serde_json::from_str(&response_body)
            .map_err(|error| invalid_response_error("invalid Gemini JSON", error))?;
        gemini_response_to_provider_turn_response(parsed)
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("gemini/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        vec![ProviderPromptCapability::FullRequestOnly]
    }
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
            function_call: None,
            function_response: None,
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

fn conversation_message_to_gemini_content(message: &ConversationMessage) -> Option<GeminiContent> {
    match message {
        ConversationMessage::UserText(text) => (!text.trim().is_empty()).then(|| GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(text.clone()),
                function_call: None,
                function_response: None,
            }],
        }),
        ConversationMessage::UserBlocks(blocks) => {
            let parts = blocks
                .iter()
                .filter(|block| !block.text.trim().is_empty())
                .map(|block| GeminiPart {
                    text: Some(block.text.clone()),
                    function_call: None,
                    function_response: None,
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| GeminiContent {
                role: "user".to_string(),
                parts,
            })
        }
        ConversationMessage::UserImage { prompt, .. } => (!prompt.trim().is_empty()).then(|| {
            GeminiContent {
                role: "user".to_string(),
                parts: vec![GeminiPart {
                    text: Some(format!(
                        "{prompt}\n\n[image input omitted: this provider transport does not support image lowering yet]"
                    )),
                    function_call: None,
                    function_response: None,
                }],
            }
        }),
        ConversationMessage::AssistantBlocks(blocks) => {
            let parts = blocks
                .iter()
                .map(|block| match block {
                    ModelBlock::Text { text } => GeminiPart {
                        text: Some(text.clone()),
                        function_call: None,
                        function_response: None,
                    },
                    ModelBlock::ToolUse { name, input, .. } => GeminiPart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name: name.clone(),
                            args: input.clone(),
                        }),
                        function_response: None,
                    },
                    ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {
                        GeminiPart {
                            text: None,
                            function_call: None,
                            function_response: None,
                        }
                    }
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
                        function_call: None,
                        function_response: Some(GeminiFunctionResponse {
                            name: response_name,
                            response: json!({
                                "content": result.content,
                                "is_error": result.is_error,
                            }),
                        }),
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

fn gemini_response_to_provider_turn_response(
    parsed: GenerateContentResponse,
) -> Result<ProviderTurnResponse> {
    let candidate = parsed
        .candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Gemini response contained no candidates"))?;
    let blocks = candidate
        .content
        .map(|content| gemini_parts_to_model_blocks(content.parts))
        .unwrap_or_default();
    let usage = parsed.usage_metadata;
    Ok(ProviderTurnResponse {
        blocks,
        stop_reason: candidate.finish_reason,
        input_tokens: usage
            .as_ref()
            .and_then(|usage| usage.prompt_token_count)
            .unwrap_or(0),
        output_tokens: usage
            .as_ref()
            .and_then(|usage| usage.candidates_token_count)
            .unwrap_or(0),
        cache_usage: Some(ProviderCacheUsage {
            read_input_tokens: 0,
            creation_input_tokens: 0,
        }),
        provider_message_id: None,
        provider_request_id: None,
        request_diagnostics: Some(ProviderRequestDiagnostics {
            request_lowering_mode: "gemini_generate_content".to_string(),
            anthropic_cache: None,
            anthropic_context_management: None,
            openai_request_controls: None,
            incremental_continuation: None,
            openai_remote_compaction: None,
            native_web_search: None,
            response_format: None,
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
                "parameters": tool.input_schema,
            })
        }).collect::<Vec<_>>()
    })]
}

const GEMINI_TOOL_USE_ID_SEPARATOR: &str = "__holon_gemini_call_";

fn gemini_parts_to_model_blocks(parts: Vec<GeminiPart>) -> Vec<ModelBlock> {
    parts
        .into_iter()
        .enumerate()
        .filter_map(|(index, part)| {
            if let Some(text) = part.text {
                return Some(ModelBlock::Text { text });
            }
            part.function_call.map(|call| ModelBlock::ToolUse {
                id: gemini_tool_use_id(&call.name, index),
                name: call.name,
                input: call.args,
                kind: ModelToolCallKind::Function,
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
mod tests {
    use super::*;

    #[test]
    fn gemini_response_preserves_empty_blocks_with_finish_reason_and_usage() {
        let response = gemini_response_to_provider_turn_response(GenerateContentResponse {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: Vec::new(),
                }),
                finish_reason: Some("MAX_TOKENS".to_string()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(11),
                candidates_token_count: Some(7),
            }),
        })
        .expect("empty Gemini content should still be a valid response");

        assert!(response.blocks.is_empty());
        assert_eq!(response.stop_reason.as_deref(), Some("MAX_TOKENS"));
        assert_eq!(response.input_tokens, 11);
        assert_eq!(response.output_tokens, 7);
    }

    #[test]
    fn gemini_tool_use_ids_are_unique_and_responses_restore_function_name() {
        let blocks = gemini_parts_to_model_blocks(vec![
            GeminiPart {
                text: None,
                function_call: Some(GeminiFunctionCall {
                    name: "ProbeTool".to_string(),
                    args: json!({"first": true}),
                }),
                function_response: None,
            },
            GeminiPart {
                text: None,
                function_call: Some(GeminiFunctionCall {
                    name: "ProbeTool".to_string(),
                    args: json!({"second": true}),
                }),
                function_response: None,
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
}
