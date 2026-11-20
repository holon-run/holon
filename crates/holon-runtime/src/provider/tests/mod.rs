//! Application provider wiring, model routing, and built-in tool contracts.

use super::{
    build_candidate, build_openai_responses_request, build_provider_from_config,
    emitted_tool_json_schema, provider_attempt_timeline, provider_doctor, provider_max_attempts,
    validate_emitted_tool_schema, AgentProvider, AnthropicProvider, ConversationMessage,
    GeminiProvider, ModelBlock, OpenAiCodexProvider, OpenAiProvider, PromptContentBlock,
    ProviderAttemptOutcome, ProviderPromptCache, ProviderPromptCapability, ProviderPromptFrame,
    ProviderTurnRequest, ProviderTurnResponse, ToolResultBlock, ToolSchemaContract,
};

#[cfg(holon_lib_test_shard = "c")]
mod anthropic_route_cache;
#[cfg(holon_lib_test_shard = "c")]
mod gemini_generate_content;
#[cfg(holon_lib_test_shard = "c")]
mod live_go_comparison;
#[cfg(holon_lib_test_shard = "c")]
mod openai_chat_completions;
#[cfg(holon_lib_test_shard = "c")]
mod openai_responses;
#[cfg(holon_lib_test_shard = "c")]
mod request_headers;
#[cfg(holon_lib_test_shard = "c")]
mod routing_auth_doctor;
mod support;
#[cfg(holon_lib_test_shard = "c")]
mod tool_schema;
