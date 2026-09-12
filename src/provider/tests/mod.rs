//! Provider contract tests module.
//!
//! This module contains provider contract tests split into logical submodules:
//!
//! - `support`: Shared test fixtures and helper functions
//! - `tool_schema`: Tool schema contract tests and OpenAI request building tests
//! - `openai_responses`: OpenAI Responses API request/response lowering tests
//! - `anthropic_messages`: Anthropic Messages API cache/context-management tests
//! - `gemini_generate_content`: Gemini GenerateContent wire request tests
//! - `routing_auth_doctor`: Provider routing, fallback, auth, and doctor tests
//! - `openai_chat_completions`: OpenAI Chat Completions conversion, streaming, and error classification tests

// Import items from parent provider module for use in test submodules via `use super::*`
use super::{
    build_candidate, build_openai_input, build_openai_responses_request,
    build_provider_from_config, emitted_tool_json_schema, parse_openai_response,
    provider_attempt_timeline, provider_doctor, provider_max_attempts,
    validate_emitted_tool_schema, AgentProvider, AnthropicProvider, ConversationMessage,
    GeminiProvider, ModelBlock, OpenAiCodexProvider, OpenAiProvider, PromptContentBlock,
    ProviderAttemptOutcome, ProviderPromptCache, ProviderPromptCapability, ProviderPromptFrame,
    ProviderTurnRequest, ProviderTurnResponse, ToolResultBlock, ToolSchemaContract,
};

mod anthropic_messages;
mod gemini_generate_content;
mod openai_chat_completions;
mod openai_responses;
mod routing_auth_doctor;
mod support;
mod tool_schema;

#[test]
fn provider_attempt_outcome_labels_match_serialized_values() {
    for outcome in [
        ProviderAttemptOutcome::Retrying,
        ProviderAttemptOutcome::RetriesExhausted,
        ProviderAttemptOutcome::FailFastAborted,
        ProviderAttemptOutcome::Succeeded,
    ] {
        assert_eq!(
            outcome.as_str(),
            serde_json::to_value(outcome)
                .unwrap()
                .as_str()
                .expect("provider attempt outcome should serialize as a string")
        );
    }
}
