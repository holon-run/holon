//! Anthropic Messages API request/cache/context-management tests.

use std::sync::{Arc, Mutex};

use super::support::*;
use super::*;
use crate::config::{AnthropicCacheStrategy, ProviderId};
use axum::{http::HeaderMap, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};

#[tokio::test]
async fn anthropic_request_lowers_prompt_frame_blocks_to_cache_control() {
    let captured_body = Arc::new(Mutex::new(None::<serde_json::Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 4, "output_tokens": 2 }
                }))
            }
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
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .base_url = base_url;
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    let response = provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();

    // Assert that request_diagnostics.anthropic_cache is populated
    assert!(
        response.request_diagnostics.is_some(),
        "request_diagnostics should be populated"
    );
    let diagnostics = response.request_diagnostics.as_ref().unwrap();
    assert!(
        diagnostics.anthropic_cache.is_some(),
        "anthropic_cache diagnostics should be populated"
    );
    let cache_diagnostics = diagnostics.anthropic_cache.as_ref().unwrap();
    assert_eq!(cache_diagnostics.system_block_count, 1);
    assert!(!cache_diagnostics.cache_breakpoints.is_empty());

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    assert_eq!(body["system"][0]["text"], json!("stable system"));
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(
        body["messages"][0]["content"][0]["text"],
        json!("agent context")
    );
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

#[tokio::test]
async fn anthropic_continuation_request_retains_cache_control_prompt_anchors() {
    let captured_body = Arc::new(Mutex::new(None::<serde_json::Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 4,
                        "output_tokens": 2,
                        "cache_read_input_tokens": 20
                    }
                }))
            }
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
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .base_url = base_url;
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    let response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    // Assert that request_diagnostics.anthropic_cache is populated for continuations
    assert!(
        response.request_diagnostics.is_some(),
        "request_diagnostics should be populated for continuations"
    );
    let diagnostics = response.request_diagnostics.as_ref().unwrap();
    assert!(
        diagnostics.anthropic_cache.is_some(),
        "anthropic_cache diagnostics should be populated for continuations"
    );
    let cache_diagnostics = diagnostics.anthropic_cache.as_ref().unwrap();
    assert_eq!(cache_diagnostics.system_block_count, 1); // Structured system request
    assert_eq!(cache_diagnostics.tools_count, 0); // No tools in continuation request

    assert_eq!(
        response
            .cache_usage
            .as_ref()
            .map(|usage| usage.read_input_tokens),
        Some(20)
    );
    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], json!("tool_use"));
    assert_eq!(
        body["messages"][2]["content"][0]["type"],
        json!("tool_result")
    );
    assert_eq!(
        body["messages"][2]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(cache_diagnostics
        .cache_breakpoints
        .iter()
        .any(|breakpoint| {
            breakpoint.location == "messages[2].content[0]"
                && breakpoint.stability == "conversation_tail"
        }));
}

#[tokio::test]
async fn anthropic_claude_cli_like_strategy_moves_context_to_system_prefix() {
    let captured_body = Arc::new(Mutex::new(None::<serde_json::Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 4, "output_tokens": 2 }
                }))
            }
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
    let anthropic = fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap();
    anthropic.base_url = base_url;
    anthropic.context_management.cache_strategy = AnthropicCacheStrategy::ClaudeCliLike;
    anthropic.context_management.betas = vec![
        "claude-code-20250219".into(),
        "prompt-caching-scope-2026-01-05".into(),
    ];
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    let mut request = provider_turn_request_with_prompt_frame();
    request
        .prompt_frame
        .cache
        .as_mut()
        .unwrap()
        .prompt_cache_key = "cache\"key\\with\nnewline".into();
    request
        .conversation
        .push(ConversationMessage::UserText("implement it".into()));
    let response = provider.complete_turn(request).await.unwrap();

    let diagnostics = response.request_diagnostics.as_ref().unwrap();
    assert_eq!(
        diagnostics.request_lowering_mode,
        "claude_cli_like_prompt_cache"
    );
    let cache_diagnostics = diagnostics.anthropic_cache.as_ref().unwrap();
    assert_eq!(cache_diagnostics.cache_strategy, "claude_cli_like");
    assert_eq!(cache_diagnostics.system_cache_control_count, 2);
    assert_eq!(cache_diagnostics.message_cache_control_count, 1);
    assert_eq!(cache_diagnostics.conversation_message_count, 1);

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    assert_eq!(
        body["betas"],
        json!(["claude-code-20250219", "prompt-caching-scope-2026-01-05"])
    );
    assert_eq!(body["temperature"], json!(1.0));
    let user_id: serde_json::Value = serde_json::from_str(
        body["metadata"]["user_id"]
            .as_str()
            .expect("metadata user_id should be a string"),
    )
    .expect("metadata user_id should contain valid escaped JSON");
    let session_id = user_id["session_id"]
        .as_str()
        .expect("metadata session_id should be a string");
    assert_ne!(session_id, "cache\"key\\with\nnewline");
    assert!(session_id.len() >= 6 && session_id.len() <= 64);
    assert!(session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    assert!(session_id.starts_with("cache-key-with-newline-"));
    assert_eq!(
        body["system"][0]["text"],
        json!("x-anthropic-billing-header: holon")
    );
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["system"][1]["text"], json!("rendered system"));
    assert_eq!(
        body["system"][1]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(body["system"][2]["text"], json!("agent context"));
    assert_eq!(
        body["system"][2]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["messages"][0]["content"][0]["text"],
        json!("implement it")
    );
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

#[tokio::test]
async fn anthropic_request_emits_context_management_when_enabled() {
    let captured_body = Arc::new(Mutex::new(None::<serde_json::Value>));
    let captured_beta = Arc::new(Mutex::new(None::<String>));
    let captured_body_for_server = captured_body.clone();
    let captured_beta_for_server = captured_beta.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(
            move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let captured_body = captured_body_for_server.clone();
                let captured_beta = captured_beta_for_server.clone();
                async move {
                    *captured_body.lock().unwrap() = Some(body);
                    *captured_beta.lock().unwrap() = headers
                        .get("anthropic-beta")
                        .and_then(|value| value.to_str().ok())
                        .map(ToString::to_string);
                    Json(json!({
                        "content": [{ "type": "text", "text": "ok" }],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 4, "output_tokens": 2 }
                    }))
                }
            },
        ),
    ))
    .await;
    let mut fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("anthropic-token"),
        false,
    );
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .base_url = base_url;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .context_management
        .enabled = true;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .context_management
        .trigger_input_tokens = 12_000;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .context_management
        .keep_recent_tool_uses = 4;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .context_management
        .clear_at_least_input_tokens = Some(2_000);
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    assert_eq!(
        captured_beta.lock().unwrap().as_deref(),
        Some("context-management-2025-06-27")
    );
    assert_eq!(
        body["context_management"]["edits"][0],
        json!({
            "type": "clear_tool_uses_20250919",
            "trigger": { "type": "input_tokens", "value": 12000 },
            "keep": { "type": "tool_uses", "value": 4 },
            "exclude_tools": ["ApplyPatch", "NotifyOperator"],
            "clear_at_least": { "type": "input_tokens", "value": 2000 }
        })
    );
    assert!(provider
        .prompt_capabilities()
        .contains(&ProviderPromptCapability::ContextManagement));
}

#[tokio::test]
async fn anthropic_claude_cli_like_strategy_uses_valid_default_session_id() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 4, "output_tokens": 2 }
                }))
            }
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
    let anthropic = fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap();
    anthropic.base_url = base_url;
    anthropic.context_management.cache_strategy = AnthropicCacheStrategy::ClaudeCliLike;
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    let user_id: Value = serde_json::from_str(
        body["metadata"]["user_id"]
            .as_str()
            .expect("metadata user_id should be a string"),
    )
    .expect("metadata user_id should contain valid escaped JSON");
    assert_eq!(user_id["session_id"], json!("holon-default"));
}

#[tokio::test]
async fn anthropic_claude_cli_like_strategy_keeps_non_empty_initial_messages() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 4, "output_tokens": 2 }
                }))
            }
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
    let anthropic = fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap();
    anthropic.base_url = base_url;
    anthropic.context_management.cache_strategy = AnthropicCacheStrategy::ClaudeCliLike;
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    let messages = body["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], json!("user"));
    assert_eq!(
        messages[0]["content"][0]["text"],
        json!("Continue using the context above.")
    );
    assert_ne!(messages[0]["content"][0]["text"], body["system"][2]["text"]);
}

#[tokio::test]
async fn anthropic_claude_cli_like_strategy_does_not_cache_mark_tool_results() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 4, "output_tokens": 2 }
                }))
            }
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
    let anthropic = fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap();
    anthropic.base_url = base_url;
    anthropic.context_management.cache_strategy = AnthropicCacheStrategy::ClaudeCliLike;
    let provider = AnthropicProvider::from_config(&fixture.config).unwrap();

    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::Text {
                text: "I'll inspect the issue first.".into(),
            },
            ModelBlock::ToolUse {
                id: "exec-1".into(),
                name: "ExecCommand".into(),
                input: json!({ "cmd": "gh issue view 565" }),
            },
        ]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "Process exited with code 0\n\nstdout:\n{}".into(),
            is_error: false,
            error: None,
        }]),
    ]);

    let response = provider.complete_turn(request).await.unwrap();

    let cache_diagnostics = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.anthropic_cache.as_ref())
        .expect("anthropic cache diagnostics should be present");
    assert_eq!(cache_diagnostics.conversation_message_count, 3);
    assert_eq!(cache_diagnostics.message_cache_control_count, 1);

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("server should capture request body");
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(body["messages"][0]["role"], json!("user"));
    assert_eq!(
        body["messages"][0]["content"][0]["text"],
        json!("Continue using the context above.")
    );
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(body["messages"][1]["content"][1]
        .get("cache_control")
        .is_none());
    assert!(body["messages"][2]["content"][0]
        .get("cache_control")
        .is_none());
    assert_eq!(body["messages"][1]["content"][1]["type"], json!("tool_use"));
    assert_eq!(
        body["messages"][2]["content"][0]["type"],
        json!("tool_result")
    );
}
