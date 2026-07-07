use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{prompt::PromptStability, tool::ToolError, tool::ToolSpec, types::TokenUsage};

mod catalog;
mod diagnostics;
mod fallback;
mod http_trace;
mod retry;
pub mod test_support;
mod tool_schema;
mod transports;

pub use catalog::{build_provider_from_config, build_provider_from_model_chain};
pub use diagnostics::{
    provider_doctor, resolved_model_availability, resolved_model_providers,
    resolved_provider_models,
};
pub(crate) use diagnostics::{
    provider_models_from_availability_for_runtime,
    resolved_model_providers_from_availability_for_runtime,
};
pub use http_trace::ProviderHttpTraceDiagnostics;
pub(crate) use retry::sanitize_transport_url;
pub use transports::{
    AnthropicProvider, GeminiProvider, OpenAiChatCompletionsProvider, OpenAiCodexProvider,
    OpenAiProvider,
};

#[derive(Debug, Clone)]
pub struct ProviderTurnRequest {
    pub prompt_frame: ProviderPromptFrame,
    pub conversation: Vec<ConversationMessage>,
    pub tools: Vec<ToolSpec>,
    pub native_web_search: Option<ProviderNativeWebSearchRequest>,
    pub response_format: Option<ProviderResponseFormatRequest>,
}

impl ProviderTurnRequest {
    pub fn plain(
        system_prompt: impl Into<String>,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolSpec>,
    ) -> Self {
        Self {
            prompt_frame: ProviderPromptFrame::plain(system_prompt),
            conversation,
            tools,
            native_web_search: None,
            response_format: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderTurnResponse {
    pub blocks: Vec<ModelBlock>,
    pub stop_reason: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_usage: Option<ProviderCacheUsage>,
    pub provider_message_id: Option<String>,
    pub provider_request_id: Option<String>,
    pub request_diagnostics: Option<ProviderRequestDiagnostics>,
}

#[derive(Debug, Clone)]
pub struct ProviderGenerateImageRequest {
    pub prompt: String,
    pub size: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderGeneratedImage {
    pub bytes: Vec<u8>,
    pub mime: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderGenerateImageResponse {
    pub provider: String,
    pub model: String,
    pub images: Vec<ProviderGeneratedImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPromptFrame {
    pub system_prompt: String,
    pub system_blocks: Vec<PromptContentBlock>,
    pub context_blocks: Vec<PromptContentBlock>,
    pub cache: Option<ProviderPromptCache>,
}

impl ProviderPromptFrame {
    pub fn structured(
        system_prompt: impl Into<String>,
        system_blocks: Vec<PromptContentBlock>,
        context_blocks: Vec<PromptContentBlock>,
        cache: Option<ProviderPromptCache>,
    ) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            system_blocks,
            context_blocks,
            cache,
        }
    }

    pub fn plain(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            system_blocks: Vec::new(),
            context_blocks: Vec::new(),
            cache: None,
        }
    }

    pub fn has_structured_system_blocks(&self) -> bool {
        !self.system_blocks.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptContentBlock {
    pub text: String,
    pub stability: PromptStability,
    #[serde(default)]
    pub cache_breakpoint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPromptCache {
    pub agent_id: String,
    pub prompt_cache_key: String,
    pub context_fingerprint: String,
    pub compression_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCacheUsage {
    #[serde(default)]
    pub read_input_tokens: u64,
    #[serde(default)]
    pub creation_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRequestDiagnostics {
    pub request_lowering_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_cache: Option<AnthropicPromptCacheDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_context_management: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_request_controls: Option<ProviderOpenAiRequestControlsDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental_continuation: Option<ProviderIncrementalContinuationDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_remote_compaction: Option<ProviderOpenAiRemoteCompactionDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_web_search: Option<ProviderNativeWebSearchDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ProviderResponseFormatDiagnostics>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeWebSearchKind {
    OpenAi,
    Anthropic,
    Gemini,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderNativeWebSearchRequest {
    pub kind: ProviderNativeWebSearchKind,
    pub provider_id: String,
    pub provider_model_ref: String,
    pub advertised_tool_type: String,
    pub backend_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderNativeWebSearchDiagnostics {
    pub kind: ProviderNativeWebSearchKind,
    pub provider_id: String,
    pub provider_model_ref: String,
    pub advertised_tool_type: String,
    pub backend_kind: String,
    pub lowered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderResponseFormatRequest {
    JsonSchema(ProviderJsonSchemaResponseFormat),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderJsonSchemaResponseFormat {
    pub name: String,
    pub schema: Value,
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderResponseFormatDiagnostics {
    pub requested: bool,
    pub lowered: bool,
    pub format_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderBuiltinWebSearchCapability {
    pub kind: ProviderNativeWebSearchKind,
    pub provider_id: String,
    pub provider_model_ref: String,
    pub provider_transport: String,
    pub provider_base_url: String,
    pub advertised_tool_type: String,
    pub backend_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnthropicPromptCacheDiagnostics {
    #[serde(default)]
    pub cache_strategy: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub betas: Vec<String>,
    pub tools_count: usize,
    pub tools_hash: String,
    pub system_hash: String,
    pub system_block_count: usize,
    pub estimated_system_tokens: u64,
    pub context_hash_by_stability: std::collections::BTreeMap<String, String>,
    pub conversation_message_count: usize,
    pub conversation_content_block_count: usize,
    #[serde(default)]
    pub system_cache_control_count: usize,
    #[serde(default)]
    pub message_cache_control_count: usize,
    pub cache_breakpoints: Vec<CacheBreakpointInfo>,
    pub tokens_before_last_breakpoint: u64,
    pub tokens_after_last_breakpoint: u64,
    pub automatic_cache_control_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheBreakpointInfo {
    pub location: String,
    pub provider_payload_path: String,
    pub block_kind: String,
    pub stability: String,
    pub estimated_prefix_tokens: u64,
    pub content_hash: String,
    pub canonical_prefix_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderIncrementalContinuationDiagnostics {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental_input_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_input_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_prefix_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_mismatch_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_item_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_item_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_shape_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_mismatch_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderOpenAiRequestControlsDiagnostics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    pub reasoning_sent: bool,
    pub include_reasoning_encrypted_content: bool,
    pub max_output_tokens_sent: bool,
    pub max_output_tokens_unsupported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderOpenAiRemoteCompactionDiagnostics {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_compaction_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content_hashes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content_bytes: Option<Vec<usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_shape_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderContextManagementPolicy {
    pub provider: String,
    pub strategy: String,
    pub keep_recent_tool_uses: usize,
    pub trigger_input_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_at_least_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPromptCapability {
    FullRequestOnly,
    PromptCacheKey,
    PromptCacheBlocks,
    IncrementalResponses,
    ContextManagement,
}

#[derive(Debug, Clone)]
pub enum ConversationMessage {
    UserText(String),
    UserBlocks(Vec<PromptContentBlock>),
    UserImage {
        prompt: String,
        media_type: String,
        data_base64: String,
    },
    AssistantBlocks(Vec<ModelBlock>),
    UserToolResults(Vec<ToolResultBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        text: String,
        /// Opaque signature returned by the provider (e.g. DeepSeek V4 Pro).
        /// Must be passed back verbatim in subsequent requests.
        signature: String,
    },
    RedactedThinking {
        /// Opaque encrypted/redacted thinking payload returned by the provider.
        /// Must be passed back verbatim in subsequent requests.
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse>;

    async fn generate_image(
        &self,
        _request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        Err(anyhow!("active provider does not support image generation"))
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        vec![ProviderPromptCapability::FullRequestOnly]
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        false
    }

    fn prompt_tool_specs(&self, tools: &[ToolSpec]) -> Vec<ToolSpec> {
        if self.supports_freeform_grammar_tools() {
            return tools.to_vec();
        }

        tools
            .iter()
            .cloned()
            .map(|mut tool| {
                tool.freeform_grammar = None;
                tool
            })
            .collect()
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        None
    }

    async fn probe_builtin_web_search(
        &self,
        _request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        Err(anyhow!(
            "active provider does not support builtin web search probing"
        ))
    }

    fn native_web_search_kind(&self) -> Option<ProviderNativeWebSearchKind> {
        self.builtin_web_search().map(|capability| capability.kind)
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        None
    }

    async fn complete_turn_with_diagnostics(
        &self,
        request: ProviderTurnRequest,
    ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
        self.complete_turn(request)
            .await
            .map(|response| (response, None))
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        Vec::new()
    }
}

pub(crate) fn builtin_web_search_probe_turn_request(
    native_web_search: ProviderNativeWebSearchRequest,
) -> ProviderTurnRequest {
    ProviderTurnRequest {
        prompt_frame: ProviderPromptFrame::plain("Reply with exactly OK."),
        conversation: vec![ConversationMessage::UserText(
            "Reply with exactly OK.".into(),
        )],
        tools: Vec::new(),
        native_web_search: Some(native_web_search),
        response_format: None,
    }
}

#[derive(Clone)]
pub struct StubProvider {
    reply: String,
}

impl StubProvider {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
        }
    }
}

#[async_trait]
impl AgentProvider for StubProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: self.reply.clone(),
            }],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec!["stub".into()]
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptOutcome {
    Retrying,
    RetriesExhausted,
    FailFastAborted,
    Succeeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAttemptRecord {
    pub provider: String,
    pub model_ref: String,
    pub attempt: usize,
    pub max_attempts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    pub outcome: ProviderAttemptOutcome,
    pub advanced_to_fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_diagnostics: Option<ProviderTransportDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAttemptTimeline {
    pub attempts: Vec<ProviderAttemptRecord>,
    #[serde(default)]
    pub requested_model_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_fallback_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregated_token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderTransportDiagnostics {
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reqwest: Option<ReqwestTransportDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_trace: Option<ProviderHttpTraceDiagnostics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReqwestTransportDiagnostics {
    pub is_timeout: bool,
    pub is_connect: bool,
    pub is_request: bool,
    pub is_body: bool,
    pub is_decode: bool,
    pub is_redirect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Debug, Error)]
#[error("{message}")]
struct ProviderTurnError {
    message: String,
    timeline: ProviderAttemptTimeline,
    #[source]
    source: anyhow::Error,
}

pub fn provider_attempt_timeline(error: &anyhow::Error) -> Option<&ProviderAttemptTimeline> {
    error
        .downcast_ref::<ProviderTurnError>()
        .map(|error| &error.timeline)
}

pub fn provider_transport_diagnostics(
    error: &anyhow::Error,
) -> Option<&ProviderTransportDiagnostics> {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<retry::ProviderTransportError>())
        .and_then(|error| error.diagnostics.as_ref())
        .or_else(|| {
            provider_attempt_timeline(error).and_then(|timeline| {
                timeline
                    .attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.transport_diagnostics.as_ref())
            })
        })
}

pub fn provider_error_contains_code(error: &anyhow::Error, code: &str) -> bool {
    error
        .chain()
        .any(|source| source.to_string().contains(code))
}

pub fn provider_error_is_context_length_exceeded(error: &anyhow::Error) -> bool {
    provider_error_contains_code(error, "context_length_exceeded")
}

pub(crate) fn provider_turn_error(
    message: impl Into<String>,
    timeline: ProviderAttemptTimeline,
    source: anyhow::Error,
) -> anyhow::Error {
    ProviderTurnError {
        message: message.into(),
        timeline,
        source,
    }
    .into()
}

pub(crate) fn aggregate_attempt_token_usage(
    attempts: &[ProviderAttemptRecord],
) -> Option<TokenUsage> {
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut saw_usage = false;
    for attempt in attempts {
        if let Some(usage) = attempt.token_usage.as_ref() {
            total_input_tokens = total_input_tokens.saturating_add(usage.input_tokens);
            total_output_tokens = total_output_tokens.saturating_add(usage.output_tokens);
            saw_usage = true;
        }
    }

    saw_usage.then(|| TokenUsage::new(total_input_tokens, total_output_tokens))
}

pub(crate) use catalog::build_candidate;
pub(crate) use retry::classify_provider_error;
#[cfg(test)]
pub(crate) use retry::provider_max_attempts;
#[cfg(test)]
pub(crate) use retry::{
    provider_transport_error, ProviderFailureClassification, ProviderFailureKind, RetryDisposition,
};
#[cfg(test)]
pub(crate) use tool_schema::validate_emitted_tool_schema;
pub(crate) use tool_schema::{emitted_tool_json_schema, ToolSchemaContract};
#[cfg(test)]
pub(crate) use transports::{
    build_openai_input, build_openai_responses_request, parse_openai_response,
};

#[cfg(test)]
mod tests;
