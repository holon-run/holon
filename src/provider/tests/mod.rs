//! Provider contract tests module.
//!
//! This module contains provider contract tests split into logical submodules:
//!
//! - `support`: Shared test fixtures and helper functions
//! - `tool_schema`: Tool schema contract tests and OpenAI request building tests
//! - `openai_responses`: OpenAI Responses API request/response lowering tests
//! - `anthropic_messages`: Anthropic Messages API cache/context-management tests
//! - `routing_auth_doctor`: Provider routing, fallback, auth, and doctor tests
//! - `openai_chat_completions`: OpenAI Chat Completions conversion, streaming, and error classification tests

// Import items from parent provider module for use in test submodules via `use super::*`
use super::{
    build_candidate, build_openai_input, build_openai_responses_request,
    build_provider_from_config, emitted_tool_json_schema, parse_openai_response,
    provider_attempt_timeline, provider_doctor, provider_max_attempts,
    provider_transport_diagnostics, validate_emitted_tool_schema, AgentProvider, AnthropicProvider,
    ConversationMessage, ModelBlock, OpenAiCodexProvider, OpenAiProvider, PromptContentBlock,
    ProviderAttemptOutcome, ProviderPromptCache, ProviderPromptCapability, ProviderPromptFrame,
    ProviderTurnRequest, ToolResultBlock, ToolSchemaContract,
};

mod anthropic_messages;
mod openai_chat_completions;
mod openai_responses;
mod routing_auth_doctor;
mod support;
mod tool_schema;
