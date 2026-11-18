//! Protocol contract tests for the provider adapters.

use super::fallback::{FallbackProvider, ProviderCandidate};
use super::tool_schema::{emitted_tool_json_schema, ToolSchemaContract};
use super::transports::{AnthropicProvider, GeminiProvider, OpenAiCodexProvider, OpenAiProvider};
use super::*;
use std::sync::Arc;

mod anthropic_messages;
mod anthropic_route_cache;
mod anthropic_stream_interruption;
mod gemini_generate_content;
mod http_timeouts;
mod openai_chat_completions;
mod openai_responses;
mod request_headers;
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
