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
    ProviderQuotaIdentity, ProviderQuotaIdentityConfidence, ProviderTurnRequest,
    ProviderTurnResponse, ToolResultBlock, ToolSchemaContract,
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

#[test]
fn provider_quota_identity_hashes_are_stable_and_account_scoped() {
    let first = ProviderQuotaIdentity::exact("codex-account", "account-a")
        .expect("non-empty account should produce an exact identity");
    let same = ProviderQuotaIdentity::exact("codex-account", " account-a ")
        .expect("trimmed account should produce an exact identity");
    let different = ProviderQuotaIdentity::exact("codex-account", "account-b")
        .expect("different account should produce an exact identity");

    assert_eq!(first, same);
    assert_ne!(first, different);
    assert_eq!(first.confidence, ProviderQuotaIdentityConfidence::Exact);
    assert!(!first.scope.contains("account-a"));
    assert!(!first.scope.contains("account-b"));
}

#[test]
fn provider_quota_identity_rejects_empty_exact_values_and_round_trips_coarse_values() {
    assert!(ProviderQuotaIdentity::exact("", "account-a").is_none());
    assert!(ProviderQuotaIdentity::exact("codex-account", " ").is_none());

    let coarse = ProviderQuotaIdentity::coarse("provider-credential", "openai-codex:default");
    assert_eq!(coarse.confidence, ProviderQuotaIdentityConfidence::Coarse);

    let encoded = serde_json::to_value(&coarse).expect("quota identity should serialize");
    let decoded: ProviderQuotaIdentity =
        serde_json::from_value(encoded).expect("quota identity should deserialize");
    assert_eq!(decoded, coarse);
}
