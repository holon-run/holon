//! Gemini GenerateContent wire request tests.

use super::support::*;
use super::*;
use crate::config::ProviderId;
use crate::provider::{
    provider_error_code, provider_transport_diagnostics,
    retry::{classify_provider_error, ProviderFailureKind},
};
use axum::{routing::post, Router};
use serde_json::json;

#[tokio::test]
async fn gemini_response_failures_keep_typed_diagnostics_without_body_secrets() {
    for (body, kind, code) in [
        (
            json!({
                "candidates": [{
                    "finishReason": "PROHIBITED_CONTENT",
                    "finishMessage": "secret-finish",
                    "content": {"parts": [{"text": "secret-response"}]}
                }],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }).to_string(),
            ProviderFailureKind::InvalidResponse,
            Some("gemini_content_blocked"),
        ),
        (
            json!({
                "promptFeedback": {"blockReason": "secret-unknown", "blockReasonMessage": "secret-message"},
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }).to_string(),
            ProviderFailureKind::InvalidResponse,
            Some("gemini_content_blocked"),
        ),
        (
            json!({
                "candidates": [{"finishReason": "STOP", "content": {"parts": [{"text": " \n"}]}}],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }).to_string(),
            ProviderFailureKind::EmptyResponse,
            None,
        ),
        (
            r#"{"candidates":"secret-invalid-field"}"#.to_string(),
            ProviderFailureKind::InvalidResponse,
            None,
        ),
        ("secret-not-json".to_string(), ProviderFailureKind::InvalidResponse, None),
    ] {
        let base_url = spawn_test_server(Router::new().fallback(post(move || {
            let body = body.clone();
            async move { body }
        }))).await;
        let mut fixture = test_config("gemini/gemini-3-pro", &[], None, None, false);
        let config = fixture.config.providers.get_mut(&ProviderId::gemini()).unwrap();
        config.base_url = base_url.clone();
        config.credential = Some("secret-api-key".into());
        let provider = GeminiProvider::from_runtime_config(
            config, "gemini-3-pro", 128, &fixture.config.home_dir,
        ).unwrap();
        let error = provider.complete_turn(ProviderTurnRequest::plain("secret-prompt", Vec::new(), Vec::new()))
            .await.expect_err("semantic and JSON failures must be typed");
        assert_eq!(classify_provider_error(&error).kind, kind);
        assert_eq!(provider_error_code(&error), code);
        let diagnostics = provider_transport_diagnostics(&error).expect("response diagnostics");
        assert_eq!(diagnostics.stage, "response_protocol");
        assert_eq!(diagnostics.provider.as_deref(), Some("gemini"));
        assert_eq!(diagnostics.model_ref.as_deref(), Some("gemini@default/gemini-3-pro"));
        assert_eq!(diagnostics.url.as_deref(), Some(format!("{base_url}/models/gemini%2D3%2Dpro:generateContent").as_str()));
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, code.unwrap_or(kind.as_str()));
        assert!(!format!("{error:?} {diagnostics:?} {descriptor:?}").contains("secret-"));
        if kind == ProviderFailureKind::EmptyResponse || code.is_some() {
            let usage = crate::provider::provider_error_token_usage(&error).unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (11, 2));
        }
    }
}
