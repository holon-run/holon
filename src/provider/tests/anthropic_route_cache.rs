//! Route-resolved cache policy must reach the wire without changing shared endpoints.

use super::support::*;
use super::*;
use crate::config::{
    persisted_config_path, save_persisted_config_at, AnthropicCacheStrategy, AppConfig,
    HolonConfigFile, ModelRouteRef, ProviderId,
};
use crate::model_catalog::AnthropicCacheControlSource;
use crate::prompt::PromptStability;
use crate::provider::AnthropicPromptCacheDiagnostics;
use axum::{http::HeaderMap, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

async fn assert_registry_cache_policy_isolation(strategy: AnthropicCacheStrategy) {
    let _env = crate::test_env::lock_env();
    let captured = Arc::new(Mutex::new(Vec::<(HeaderMap, Value)>::new()));
    let captured_for_server = captured.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            async move {
                captured.lock().unwrap().push((headers, body));
                Json(json!({
                    "content": [{"type": "text", "text": "ok"}],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 4, "output_tokens": 2,
                        "cache_read_input_tokens": 7, "cache_creation_input_tokens": 3
                    }
                }))
            }
        }),
    ))
    .await;
    let home = tempfile::tempdir().unwrap();
    let stored: HolonConfigFile = serde_json::from_value(json!({
        "model": {"default": "cache-proxy@default/model-a"},
        "providers": {
            "cache-proxy": {
                "transport": "anthropic_messages",
                "base_url": base_url,
                "auth": {"source": "none", "kind": "none"},
                "cache_capabilities": {"cache_control": true}
            }
        },
        "models": {
            "routes": {
                "cache-proxy@default/model-a": {
                    "cache_capabilities": {"cache_control": false}
                }
            }
        }
    }))
    .unwrap();
    save_persisted_config_at(&persisted_config_path(home.path()), &stored).unwrap();
    let mut config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    let provider_id = ProviderId::parse("cache-proxy").unwrap();
    let runtime = config.providers.get_mut(&provider_id).unwrap();
    runtime.context_management.cache_strategy = strategy;
    runtime.context_management.enabled = true;
    runtime.context_management.betas = vec![
        "claude-code-20250219".into(),
        "prompt-caching-scope-2026-01-05".into(),
        "unrelated-test-beta".into(),
    ];
    let original_runtime = runtime.clone();

    for (model, enabled, source) in [
        ("model-a", false, AnthropicCacheControlSource::RouteOverride),
        (
            "model-b",
            true,
            AnthropicCacheControlSource::ProviderOverride,
        ),
        ("model-a", false, AnthropicCacheControlSource::RouteOverride),
    ] {
        let route = format!("cache-proxy@default/{model}");
        config.default_model = ModelRouteRef::parse(&route).unwrap();
        let provider = build_provider_from_config(&config).unwrap();
        assert_eq!(provider.configured_model_refs(), vec![route.clone()]);
        let mut request = provider_continuation_request_with_prompt_frame();
        // The registry adds a cacheable runtime system block. Leave room for
        // both the turn-scoped prefix and rolling history in the four-marker budget.
        request.prompt_frame.context_blocks[0].cache_breakpoint = false;
        request
            .prompt_frame
            .context_blocks
            .push(PromptContentBlock {
                text: "current turn context".into(),
                stability: PromptStability::TurnScoped,
                cache_breakpoint: false,
            });
        request.conversation[0] =
            ConversationMessage::UserBlocks(request.prompt_frame.context_blocks.clone());
        let response = provider.complete_turn(request).await.unwrap();
        let diagnostics = response
            .request_diagnostics
            .as_ref()
            .unwrap()
            .anthropic_cache
            .as_ref()
            .unwrap();
        assert_eq!(diagnostics.model_route.as_deref(), Some(route.as_str()));
        assert_eq!(diagnostics.model, model);
        assert_eq!(diagnostics.explicit_cache_control_enabled, Some(enabled));
        assert_eq!(diagnostics.cache_control_source, Some(source));
        let usage = response.cache_usage.as_ref().unwrap();
        assert_eq!(usage.read_input_tokens, 7);
        assert_eq!(usage.creation_input_tokens, 3);

        let (headers, body) = captured.lock().unwrap().last().unwrap().clone();
        assert_eq!(body["model"], model);
        assert_eq!(headers["anthropic-beta"], "context-management-2025-06-27");
        assert!(body.get("context_management").is_some());
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "current turn context"
        );
        let serialized = serde_json::to_string(&body).unwrap();
        if enabled {
            assert!(diagnostics.system_cache_control_count > 0);
            assert!(
                diagnostics.message_cache_control_count >= 2,
                "expected turn-scoped and rolling breakpoints: {body}"
            );
            assert!(diagnostics.rolling_marker_at_tail);
            assert_eq!(
                body["messages"][0]["content"][0]["cache_control"],
                json!({"type": "ephemeral"})
            );
            assert_eq!(
                body["betas"],
                json!([
                    "claude-code-20250219",
                    "prompt-caching-scope-2026-01-05",
                    "unrelated-test-beta"
                ])
            );
        } else {
            assert!(!serialized.contains("cache_control"));
            assert!(!serialized.contains("x-anthropic-billing-header"));
            assert!(body.get("metadata").is_none());
            assert!(body.get("temperature").is_none());
            assert!(diagnostics.cache_breakpoints.is_empty());
            assert!(!diagnostics.rolling_marker_at_tail);
            assert_eq!(body["betas"], json!(["unrelated-test-beta"]));
        }
        if enabled && strategy == AnthropicCacheStrategy::ClaudeCodePromptCache {
            assert!(serialized.contains("x-anthropic-billing-header"));
            assert!(body["metadata"]["user_id"].is_string());
            assert_eq!(body["temperature"], 1.0);
        }
        assert_eq!(
            serde_json::to_value(&diagnostics.betas).unwrap(),
            body["betas"]
        );
        assert_eq!(config.providers[&provider_id], original_runtime);
    }
    assert_eq!(captured.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn anthropic_registry_route_cache_override_isolated_across_rebuilds_messages_native() {
    assert_registry_cache_policy_isolation(AnthropicCacheStrategy::MessagesNative).await;
}

#[tokio::test]
async fn anthropic_registry_route_cache_override_isolated_across_rebuilds_claude_code() {
    assert_registry_cache_policy_isolation(AnthropicCacheStrategy::ClaudeCodePromptCache).await;
}

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
    let mut fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("anthropic-token"),
        false,
    );
    for enabled in [false, true] {
        let runtime = fixture
            .config
            .providers
            .get_mut(&ProviderId::anthropic())
            .unwrap();
        runtime.base_url = base_url.clone();
        runtime.context_management.cache_capabilities.cache_control = enabled;
        let provider = AnthropicProvider::from_runtime_config(
            runtime,
            "claude-sonnet-4-6",
            1024,
            fixture._home_dir.path(),
            true,
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
