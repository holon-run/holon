use super::support::*;
use super::*;
use axum::{routing::post, Json, Router};
use serde_json::json;

#[tokio::test]
async fn anthropic_direct_runtime_cache_policy_keeps_unknown_source() {
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(|| async {
            Json(json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 4, "output_tokens": 2}
            }))
        }),
    ))
    .await;
    let mut fixture = ProviderFixture::anthropic("anthropic-token");
    for enabled in [false, true] {
        let runtime = &mut fixture.config;
        runtime.base_url = base_url.clone();
        runtime.context_management.cache_capabilities.cache_control = enabled;
        let provider = AnthropicProvider::from_runtime_config(
            runtime,
            "claude-sonnet-4-6",
            1024,
            fixture.home.path(),
            true,
            TEST_CLIENT_VERSION,
        )
        .unwrap();
        let response = provider
            .complete_turn(provider_turn_request_with_prompt_frame())
            .await
            .unwrap();
        let diagnostics = response
            .request_diagnostics
            .unwrap()
            .anthropic_cache
            .unwrap();
        assert_eq!(diagnostics.explicit_cache_control_enabled, Some(enabled));
        assert_eq!(diagnostics.cache_control_source, None);
        assert_eq!(
            diagnostics.model_route.as_deref(),
            Some("anthropic@default/claude-sonnet-4-6")
        );
        let value = serde_json::to_value(diagnostics).unwrap();
        assert!(value.get("cache_control_source").is_none());
    }
}

#[test]
fn anthropic_prompt_cache_diagnostics_reads_legacy_records_with_unknown_policy() {
    let legacy = json!({
        "tools_count": 0, "tools_hash": "tools", "system_hash": "system",
        "system_block_count": 1, "estimated_system_tokens": 4,
        "context_hash_by_stability": {}, "conversation_message_count": 1,
        "conversation_content_block_count": 1, "cache_breakpoints": [],
        "tokens_before_last_breakpoint": 0, "tokens_after_last_breakpoint": 4,
        "automatic_cache_control_requested": false
    });
    let diagnostics: AnthropicPromptCacheDiagnostics = serde_json::from_value(legacy).unwrap();
    assert_eq!(diagnostics.model_route, None);
    assert_eq!(diagnostics.explicit_cache_control_enabled, None);
    assert_eq!(diagnostics.cache_control_source, None);
    assert_eq!(diagnostics.estimated_system_tokens, 4);
}
