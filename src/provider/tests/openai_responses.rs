//! OpenAI Responses API request/response lowering tests.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use super::support::*;
use super::*;
use crate::config::{ModelRef, ProviderId};
use crate::model_catalog::ModelRuntimeOverride;
use crate::provider::transports::set_stream_idle_timeout_override_for_tests;
use crate::provider::{
    ContinuationScopeId, ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
};
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};

fn openai_tool_call_response(response_id: &str) -> Value {
    openai_tool_call_response_for(response_id, "exec-1")
}

fn openai_tool_call_response_for(response_id: &str, call_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 256, "output_tokens": 2 },
        "output": [{
            "type": "function_call",
            "id": "fc_non_persisted",
            "status": "completed",
            "call_id": call_id,
            "name": "ExecCommand",
            "arguments": "{\n  \"cmd\": \"printf ok\"\n}"
        }]
    })
}

fn openai_non_object_tool_call_response(response_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 256, "output_tokens": 2 },
        "output": [{
            "type": "function_call",
            "id": "fc_non_persisted",
            "status": "completed",
            "call_id": "invented-1",
            "name": "x_semantic_search",
            "arguments": "[\"holon\"]"
        }]
    })
}

fn xai_search_and_non_object_tool_call_response(response_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 256, "output_tokens": 2 },
        "output": [{
            "type": "x_search_call",
            "id": "search_1",
            "status": "completed"
        }, {
            "type": "function_call",
            "id": "fc_non_persisted",
            "status": "completed",
            "call_id": "invented-1",
            "name": "x_semantic_search",
            "arguments": "[\"holon\"]"
        }]
    })
}

fn openai_text_response(response_id: &str, text: &str) -> Value {
    openai_text_response_with_input_tokens(response_id, text, 2)
}

fn openai_text_response_with_input_tokens(
    response_id: &str,
    text: &str,
    input_tokens: u64,
) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": input_tokens, "output_tokens": 1 },
        "output": [{
            "type": "message",
            "id": "msg_non_persisted",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }]
        }]
    })
}

fn openai_text_response_with_provider_metadata(response_id: &str, text: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 256, "output_tokens": 1 },
        "output": [{
            "type": "message",
            "id": "msg_non_persisted",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [{ "type": "file_citation", "index": 0 }],
                "metadata": { "provider_only": true },
                "status": "completed"
            }]
        }]
    })
}

fn set_remote_compaction_trigger(config: &mut crate::config::AppConfig, model_ref: &str) {
    config.validated_model_overrides.insert(
        ModelRef::parse(model_ref).unwrap(),
        ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(256),
            compaction_trigger_estimated_tokens: Some(128),
            compaction_keep_recent_estimated_tokens: Some(32),
            ..ModelRuntimeOverride::default()
        },
    );
}

fn openai_text_sse_response(response_id: &str, text: &str) -> String {
    openai_text_sse_response_with_input_tokens(response_id, text, 2)
}

fn openai_text_sse_response_with_input_tokens(
    response_id: &str,
    text: &str,
    input_tokens: u64,
) -> String {
    format!(
        concat!(
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{}\",\"status\":\"completed\",\"usage\":{{\"input_tokens\":{},\"output_tokens\":1}},\"output\":[{{\"type\":\"message\",\"id\":\"msg_non_persisted\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{}\"}}]}}]}}}}\n\n"
        ),
        response_id, input_tokens, text
    )
}

fn openai_reasoning_and_tool_call_sse_response(response_id: &str) -> String {
    format!(
        concat!(
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{}\",\"status\":\"completed\",\"usage\":{{\"input_tokens\":256,\"output_tokens\":4}},\"output\":[",
            "{{\"type\":\"reasoning\",\"id\":\"rs_non_persisted\",\"encrypted_content\":\"opaque-reasoning\"}},",
            "{{\"type\":\"function_call\",\"id\":\"fc_non_persisted\",\"call_id\":\"exec-1\",\"name\":\"ExecCommand\",\"arguments\":\"{{\\\"cmd\\\":\\\"printf ok\\\"}}\"}}",
            "]}}}}\n\n"
        ),
        response_id
    )
}

fn provider_large_window_request_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation = (0..8)
        .map(|index| ConversationMessage::UserText(format!("message {index}")))
        .collect();
    request
}

#[tokio::test]
async fn openai_from_config_uses_resolved_max_output_override_on_the_wire() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = response_bodies_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(openai_text_response("resp_1", "done"))
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    fixture.config.validated_model_overrides.insert(
        ModelRef::parse("openai/gpt-5.4").unwrap(),
        ModelRuntimeOverride {
            runtime_max_output_tokens: Some(1_234),
            ..ModelRuntimeOverride::default()
        },
    );
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();

    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies[0]["max_output_tokens"], json!(1_234));
}

#[tokio::test]
async fn openai_responses_lowers_user_image_to_input_image() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = response_bodies_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(openai_text_response("resp_1", "a red square"))
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(ProviderTurnRequest::plain(
            "Describe the image.",
            vec![ConversationMessage::UserImage {
                prompt: "What is visible?".to_string(),
                media_type: "image/png".to_string(),
                data_base64: "iVBORw0KGgo=".to_string(),
            }],
            Vec::new(),
        ))
        .await
        .unwrap();

    let response_bodies = response_bodies.lock().unwrap();
    let input = response_bodies[0]["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], json!("message"));
    assert_eq!(input[0]["role"], json!("user"));
    assert_eq!(
        input[0]["content"],
        json!([
            { "type": "input_text", "text": "What is visible?" },
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,iVBORw0KGgo="
            }
        ])
    );
}

fn provider_nearly_large_window_request_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation = (0..7)
        .map(|index| ConversationMessage::UserText(format!("message {index}")))
        .collect();
    request
}

fn provider_nearly_large_window_continuation_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_nearly_large_window_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);
    request
}

fn provider_large_window_continuation_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_large_window_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);
    request
}

fn provider_large_window_paired_request_with_prompt_frame() -> ProviderTurnRequest {
    provider_large_window_continuation_with_prompt_frame()
}

fn provider_large_window_paired_followup_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_large_window_paired_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "ready".into(),
        }]),
        ConversationMessage::UserText("continue".into()),
    ]);
    request
}

fn provider_text_followup_with_prompt_frame(previous_text: &str) -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: previous_text.into(),
        }]),
        ConversationMessage::UserText("continue".into()),
    ]);
    request
}

#[test]
fn openai_codex_provider_declares_builtin_web_search_capability() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let capability = provider
        .builtin_web_search()
        .expect("openai codex provider should declare builtin search");
    assert_eq!(capability.kind, ProviderNativeWebSearchKind::OpenAi);
    assert_eq!(capability.provider_id, "openai-codex");
    assert_eq!(capability.provider_model_ref, "openai-codex/gpt-5.4");
    assert_eq!(capability.advertised_tool_type, "web_search");
    assert_eq!(capability.backend_kind, "openai_codex_web_search");
}

#[tokio::test]
async fn openai_responses_uses_incremental_continuation_for_strict_append() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert_eq!(bodies[1]["instructions"], json!("rendered system"));
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[1]["input"][0]["type"], json!("function_call_output"));
}

#[tokio::test]
async fn openai_responses_normalizes_non_object_tool_arguments_before_continuation() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_non_object_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    assert_eq!(first.blocks.len(), 1);
    match &first.blocks[0] {
        ModelBlock::ToolUse {
            id, name, input, ..
        } => {
            assert_eq!(id, "invented-1");
            assert_eq!(name, "x_semantic_search");
            assert_eq!(input, &json!({ "_raw": ["holon"] }));
        }
        block => panic!("expected tool use, got {block:?}"),
    }

    let mut continuation = provider_turn_request_with_prompt_frame();
    continuation.conversation.extend([
        ConversationMessage::AssistantBlocks(first.blocks),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "invented-1".into(),
            content: "Failed: x_semantic_search not exposed for round".into(),
            is_error: true,
            error: None,
        }]),
    ]);
    let response = provider.complete_turn(continuation).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    let incremental = bodies[1]["input"].as_array().unwrap();
    assert_eq!(incremental.len(), 1);
    assert_eq!(incremental[0]["type"], json!("function_call_output"));
}

#[tokio::test]
async fn openai_responses_replays_lossless_window_after_continuation_rejection() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                match attempts.fetch_add(1, Ordering::SeqCst) {
                    0 => Json(openai_tool_call_response("resp_1")).into_response(),
                    1 => (StatusCode::BAD_REQUEST, "continuation rejected").into_response(),
                    _ => Json(openai_text_response("resp_2", "done")).into_response(),
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    let diagnostics = response.request_diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics.request_lowering_mode, "provider_window_replay");
    let continuation = diagnostics.incremental_continuation.as_ref().unwrap();
    assert_eq!(continuation.status, "fallback_provider_window_replay");
    assert_eq!(
        continuation.fallback_reason.as_deref(),
        Some("previous_response_id_rejected")
    );
    assert_eq!(continuation.server_side_context_may_be_lost, None);
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert!(bodies[2].get("previous_response_id").is_none());
    assert_eq!(bodies[2]["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn xai_native_search_preserves_continuation_after_rejected_internal_tool_call() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Json(xai_search_and_non_object_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("xai-key"), None, false);
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    provider_config.id = ProviderId::parse("xai").unwrap();
    provider_config.route_provider = ProviderId::parse("xai").unwrap();
    provider_config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &provider_config,
        "grok-4.5",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    let mut first_request = provider_turn_request_with_prompt_frame();
    first_request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::Xai,
        provider_id: "xai".into(),
        provider_model_ref: "xai/grok-4.5".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "xai_web_search_x_search".into(),
        max_results: Some(5),
    });
    let first = provider.complete_turn(first_request.clone()).await.unwrap();
    assert_eq!(first.blocks.len(), 1);

    let mut continuation = first_request;
    continuation.conversation.extend([
        ConversationMessage::AssistantBlocks(first.blocks),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "invented-1".into(),
            content: "Failed: x_semantic_search not exposed for round".into(),
            is_error: true,
            error: None,
        }]),
    ]);
    let response = provider.complete_turn(continuation).await.unwrap();

    let diagnostics = response
        .request_diagnostics
        .as_ref()
        .expect("request diagnostics");
    assert_eq!(
        diagnostics.request_lowering_mode,
        "incremental_continuation_omit_instructions"
    );
    assert_eq!(
        diagnostics
            .incremental_continuation
            .as_ref()
            .and_then(|continuation| continuation.server_side_context_may_be_lost),
        None
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    let incremental = bodies[1]["input"].as_array().unwrap();
    assert_eq!(incremental.len(), 1);
    assert_eq!(incremental[0]["type"], json!("function_call_output"));
}

#[tokio::test]
async fn xai_native_search_refuses_lossy_replay_after_continuation_rejection() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Json(xai_search_and_non_object_tool_call_response("resp_1")).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "continuation rejected").into_response()
                }
            }
        }),
    ))
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("xai-key"), None, false);
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    provider_config.id = ProviderId::parse("xai").unwrap();
    provider_config.route_provider = ProviderId::parse("xai").unwrap();
    provider_config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &provider_config,
        "grok-4.5",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    let mut first_request = provider_turn_request_with_prompt_frame();
    first_request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::Xai,
        provider_id: "xai".into(),
        provider_model_ref: "xai/grok-4.5".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "xai_web_search_x_search".into(),
        max_results: Some(5),
    });
    let first = provider.complete_turn(first_request.clone()).await.unwrap();
    let mut continuation = first_request;
    continuation.conversation.extend([
        ConversationMessage::AssistantBlocks(first.blocks),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "invented-1".into(),
            content: "Failed: x_semantic_search not exposed for round".into(),
            is_error: true,
            error: None,
        }]),
    ]);

    let error = provider.complete_turn(continuation).await.unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("refused provider-window replay"));
    assert!(message.contains("server_side_search_context"));
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
}

#[tokio::test]
async fn xai_native_search_fallback_warns_when_server_context_may_be_lost() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Json(xai_search_and_non_object_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("xai-key"), None, false);
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    provider_config.id = ProviderId::parse("xai").unwrap();
    provider_config.route_provider = ProviderId::parse("xai").unwrap();
    provider_config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &provider_config,
        "grok-4.5",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    let mut request = provider_turn_request_with_prompt_frame();
    request.prompt_frame.cache.as_mut().unwrap().agent_id =
        "xai-native-search-fallback-warning".into();
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::Xai,
        provider_id: "xai".into(),
        provider_model_ref: "xai/grok-4.5".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "xai_web_search_x_search".into(),
        max_results: Some(5),
    });
    let first = provider.complete_turn(request.clone()).await.unwrap();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(first.blocks),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "invented-1".into(),
            content: "Failed: x_semantic_search not exposed for round".into(),
            is_error: true,
            error: None,
        }]),
    ]);
    request
        .native_web_search
        .as_mut()
        .unwrap()
        .advertised_tool_type = "web_search_preview".into();
    let response = provider.complete_turn(request).await.unwrap();

    let continuation = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
        .expect("incremental continuation diagnostics");
    assert_eq!(
        continuation.fallback_reason.as_deref(),
        Some("request_shape_changed")
    );
    assert_eq!(continuation.server_side_context_may_be_lost, Some(true));
    let bodies = captured_bodies.lock().unwrap();
    assert!(bodies[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn xai_responses_omits_instructions_only_for_incremental_continuation() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("xai-key"), None, false);
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    provider_config.id = ProviderId::parse("xai").unwrap();
    provider_config.route_provider = ProviderId::parse("xai").unwrap();
    provider_config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &provider_config,
        "grok-4.5",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation_omit_instructions")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["instructions"], json!("rendered system"));
    assert_eq!(bodies[0]["store"], json!(true));
    assert!(bodies[0].get("tools").is_none());
    assert!(bodies[0].get("tool_choice").is_none());
    assert!(bodies[0].get("parallel_tool_calls").is_none());
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert_eq!(bodies[1]["store"], json!(true));
    assert!(bodies[1].get("instructions").is_none());
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[1]["input"][0]["type"], json!("function_call_output"));
}

#[tokio::test]
async fn xai_responses_prompt_change_keeps_instructions_and_uses_full_request() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("xai-key"), None, false);
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    provider_config.id = ProviderId::parse("xai").unwrap();
    provider_config.route_provider = ProviderId::parse("xai").unwrap();
    provider_config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &provider_config,
        "grok-4.5",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let mut changed = provider_continuation_request_with_prompt_frame();
    changed.prompt_frame.system_prompt = "changed rendered system".into();
    let response = provider.complete_turn(changed).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("request_shape_changed")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
    assert_eq!(bodies[1]["instructions"], json!("changed rendered system"));
    assert_eq!(bodies[1]["store"], json!(true));
    assert!(bodies[1]["input"].as_array().unwrap().len() > 1);
}

#[tokio::test]
async fn openai_responses_uses_incremental_continuation_when_text_output_has_provider_metadata() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_text_response_with_provider_metadata(
                        "resp_1", "ready",
                    ))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_text_followup_with_prompt_frame("ready"))
        .await
        .unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation")
    );
    let diagnostics = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
        .expect("incremental diagnostics");
    assert_eq!(diagnostics.status, "hit");
    assert_eq!(diagnostics.fallback_reason, None);
    assert_eq!(diagnostics.incremental_input_items, Some(1));
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[1]["input"][0]["role"], json!("user"));
}

#[tokio::test]
async fn openai_codex_append_match_replays_provider_window_without_previous_response_id() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/codex/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let response = if attempt == 0 {
                    openai_text_sse_response("resp_1", "ready")
                } else {
                    openai_text_sse_response("resp_2", "done")
                };
                ([("content-type", "text/event-stream")], response)
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_text_followup_with_prompt_frame("ready"))
        .await
        .unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("provider_window_replay")
    );
    let diagnostics = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
        .expect("incremental diagnostics");
    assert_eq!(diagnostics.status, "hit");
    assert_eq!(diagnostics.fallback_reason, None);
    assert_eq!(diagnostics.incremental_input_items, Some(1));
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
    let input = bodies[1]["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[1]["role"], json!("assistant"));
    assert_eq!(input[2]["role"], json!("user"));
}

#[tokio::test]
async fn openai_responses_remote_compacts_provider_window_and_replays_compaction_items() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = match attempt {
                            0 => openai_text_response_with_provider_metadata("resp_1", "ready"),
                            _ => openai_text_response("resp_2", "done"),
                        };
                        Json(response)
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "recent" }] },
                                { "type": "compaction_summary", "encrypted_content": "opaque-2", "status": "completed" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_large_window_paired_followup_with_prompt_frame())
        .await
        .unwrap();

    let remote_compaction = first
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(remote_compaction.input_items, Some(11));
    assert_eq!(remote_compaction.output_items, Some(2));
    assert_eq!(remote_compaction.compaction_items, Some(1));
    assert_eq!(remote_compaction.latest_compaction_index, Some(1));
    assert_eq!(
        remote_compaction.encrypted_content_bytes.as_deref(),
        Some([8usize].as_slice())
    );

    assert_eq!(
        second
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("provider_window_compacted")
    );
    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    assert!(response_bodies[1].get("previous_response_id").is_none());
    let replayed_input = response_bodies[1]["input"].as_array().unwrap();
    assert_eq!(replayed_input[0]["type"], json!("compaction"));
    assert_eq!(replayed_input.last().unwrap()["type"], json!("message"));
    assert!(!response_bodies[1].to_string().contains("recent"));

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    assert_eq!(compact_bodies[0]["input"].as_array().unwrap().len(), 11);
    assert_eq!(compact_bodies[0]["tools"], json!([]));
    assert_eq!(compact_bodies[0]["parallel_tool_calls"], json!(false));
}

#[tokio::test]
async fn openai_codex_remote_compact_uses_codex_backend_route_for_legacy_base_url() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/codex/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        (
                            [("content-type", "text/event-stream")],
                            openai_text_sse_response_with_input_tokens("resp_1", "ready", 256),
                        )
                    }
                }),
            )
            .route(
                "/codex/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai-codex/gpt-5.4");
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let response = provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(response_bodies.lock().unwrap().len(), 1);
    assert_eq!(compact_bodies.lock().unwrap().len(), 1);
    let remote_compaction = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(
        remote_compaction.endpoint_kind.as_deref(),
        Some("responses_compact")
    );
}

#[tokio::test]
async fn openai_codex_remote_compact_does_not_double_codex_path_for_new_base_url() {
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/codex/responses",
                post(|Json(_body): Json<Value>| async {
                    (
                        [("content-type", "text/event-stream")],
                        openai_text_sse_response_with_input_tokens("resp_1", "ready", 256),
                    )
                }),
            )
            .route(
                "/codex/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = format!("{base_url}/codex");
    set_remote_compaction_trigger(&mut fixture.config, "openai-codex/gpt-5.4");
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(compact_bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn openai_codex_remote_compact_sanitizes_ids_and_retains_unpaired_tail() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/codex/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = if attempt == 0 {
                            openai_reasoning_and_tool_call_sse_response("resp_1")
                        } else {
                            openai_text_sse_response("resp_2", "done")
                        };
                        ([("content-type", "text/event-stream")], response)
                    }
                }),
            )
            .route(
                "/codex/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque-compact" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai-codex/gpt-5.4");
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_request_with_prompt_frame())
        .await
        .unwrap();
    provider
        .complete_turn(provider_large_window_continuation_with_prompt_frame())
        .await
        .unwrap();

    let remote_compaction = first
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(remote_compaction.input_items, Some(9));

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    let compact_input = compact_bodies[0]["input"].as_array().unwrap();
    assert_eq!(compact_input.len(), 9);
    assert!(compact_bodies[0].to_string().contains("opaque-reasoning"));
    assert!(!compact_bodies[0].to_string().contains("rs_non_persisted"));
    assert!(!compact_bodies[0].to_string().contains("fc_non_persisted"));
    assert!(!compact_bodies[0].to_string().contains("exec-1"));

    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    let second_input = response_bodies[1]["input"].as_array().unwrap();
    assert_eq!(second_input[0]["type"], json!("compaction"));
    assert_eq!(second_input[1]["type"], json!("function_call"));
    assert_eq!(second_input[2]["type"], json!("function_call_output"));
    assert_eq!(second_input[1]["call_id"], json!("exec-1"));
    assert_eq!(second_input[2]["call_id"], json!("exec-1"));
}

#[tokio::test]
async fn openai_responses_caches_unsupported_remote_compact_endpoint() {
    let compact_attempts = Arc::new(AtomicUsize::new(0));
    let compact_attempts_for_server = compact_attempts.clone();
    let response_attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts_for_server = response_attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(_body): Json<Value>| {
                    let attempts = response_attempts_for_server.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            Json(openai_text_response_with_input_tokens(
                                "resp_1", "ready", 256,
                            ))
                        } else {
                            Json(openai_text_response("resp_2", "done"))
                        }
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(_body): Json<Value>| {
                    let attempts = compact_attempts_for_server.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({ "detail": "Not Found" })),
                        )
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_large_window_paired_followup_with_prompt_frame())
        .await
        .unwrap();

    let first_compaction = first
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("first remote compaction diagnostics");
    assert_eq!(first_compaction.status, "unsupported_endpoint");
    assert_eq!(first_compaction.http_status, Some(404));

    let second_compaction = second
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("second remote compaction diagnostics");
    assert_eq!(second_compaction.status, "skipped_unsupported_endpoint");
    assert_eq!(second_compaction.http_status, Some(404));
    assert_eq!(compact_attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn openai_responses_reports_non_persisted_compact_item_ids_without_endpoint_cache() {
    let compact_attempts = Arc::new(AtomicUsize::new(0));
    let compact_attempts_for_server = compact_attempts.clone();
    let response_attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts_for_server = response_attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(_body): Json<Value>| {
                    let attempts = response_attempts_for_server.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            Json(openai_text_response_with_input_tokens(
                                "resp_1", "ready", 256,
                            ))
                        } else {
                            Json(openai_text_response("resp_2", "done"))
                        }
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(_body): Json<Value>| {
                    let attempts = compact_attempts_for_server.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({
                                "error": {
                                    "message": "Item with id 'rs_missing' not found. Items are not persisted when `store` is set to false. Try again with `store` set to true, or remove this item from your input.",
                                    "type": "invalid_request_error",
                                    "param": "input",
                                    "code": null
                                }
                            })),
                        )
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_large_window_paired_followup_with_prompt_frame())
        .await
        .unwrap();

    for response in [first, second] {
        let compaction = response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
            .expect("remote compaction diagnostics");
        assert_eq!(compaction.status, "invalid_non_persisted_item_id");
        assert_eq!(compaction.http_status, Some(404));
    }
    assert_eq!(compact_attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn openai_codex_streaming_incomplete_max_output_tokens_returns_recoverable_response() {
    let base_url = spawn_test_server(Router::new().route(
        "/codex/responses",
        post(|Json(_body): Json<Value>| async {
            (
                [("content-type", "text/event-stream")],
                concat!(
                    "event: response.output_item.done\n",
                    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"partial report\"}]}}\n\n",
                    "event: response.incomplete\n",
                    "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_1\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\n"
                ),
            )
        }),
    ))
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let response = provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();

    assert_eq!(response.stop_reason.as_deref(), Some("max_output_tokens"));
    assert_eq!(response.input_tokens, 5);
    assert_eq!(response.output_tokens, 3);
    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "partial report"
    ));
}

#[tokio::test]
async fn openai_codex_retries_streaming_body_read_interruptions() {
    let interrupted = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n20\r\nevent: response.created\n".to_vec();
    let body = openai_text_sse_response("resp_ok", "retry ok");
    let complete = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes();
    let base_url = spawn_raw_http_server_bytes_sequence(vec![interrupted, complete]).await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    let provider = build_provider_from_config(&fixture.config).unwrap();

    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .expect("stream body interruption should retry and recover");

    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "retry ok"
    ));
    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("connection")
    );
    assert_eq!(
        timeline.attempts[0].disposition.as_deref(),
        Some("retryable")
    );
    assert_eq!(
        timeline.attempts[0]
            .transport_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.stage.as_str()),
        Some("streaming_response_body")
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
}

#[tokio::test]
async fn openai_codex_retries_streaming_idle_timeout_interruptions() {
    // First response sends headers immediately, then stalls before the first SSE chunk.
    let delayed_headers =
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
    let delayed_chunk = b"20\r\nevent: response.created\n".to_vec();
    let body = openai_text_sse_response("resp_ok", "retry ok");
    let complete = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes();
    let base_url = spawn_raw_http_server_scripted(vec![
        vec![(0, delayed_headers), (400, delayed_chunk)],
        vec![(0, complete)],
    ])
    .await;
    let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;
    set_stream_idle_timeout_override_for_tests(Some(100));
    let provider = build_provider_from_config(&fixture.config).unwrap();

    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .expect("idle timeout interruption should retry and recover");

    set_stream_idle_timeout_override_for_tests(None);

    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "retry ok"
    ));
    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("timeout")
    );
    assert_eq!(
        timeline.attempts[0].disposition.as_deref(),
        Some("retryable")
    );
    assert_eq!(
        timeline.attempts[0]
            .transport_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.stage.as_str()),
        Some("streaming_response_body")
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
}

#[tokio::test]
async fn openai_responses_compacts_safe_prefix_before_unpaired_tool_call() {
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(|Json(_body): Json<Value>| async {
                    Json(openai_tool_call_response("resp_1"))
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let response = provider
        .complete_turn(provider_large_window_request_with_prompt_frame())
        .await
        .unwrap();

    let remote_compaction = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(remote_compaction.input_items, Some(8));
    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    assert_eq!(compact_bodies[0]["input"].as_array().unwrap().len(), 8);
    assert!(compact_bodies[0].to_string().contains("message 7"));
    assert!(!compact_bodies[0].to_string().contains("exec-1"));
}

#[tokio::test]
async fn openai_responses_compacts_safe_prefix_without_item_count_gate() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = match attempt {
                            0 => openai_tool_call_response("resp_1"),
                            _ => openai_text_response("resp_2", "done"),
                        };
                        Json(response)
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque" }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let response = provider
        .complete_turn(provider_nearly_large_window_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_nearly_large_window_continuation_with_prompt_frame())
        .await
        .unwrap();

    let remote_compaction = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(remote_compaction.input_items, Some(7));

    assert!(
        second
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
            .is_none(),
        "the second request should replay the compacted prefix and retained tool tail without another compact pass"
    );
    assert_eq!(
        remote_compaction.trigger_reason.as_deref(),
        Some("token_budget_pressure")
    );
    assert_eq!(remote_compaction.trigger_input_tokens, Some(128));

    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    assert!(response_bodies[1].get("previous_response_id").is_none());
    let second_input = response_bodies[1]["input"].as_array().unwrap();
    assert_eq!(second_input.len(), 3);
    assert_eq!(second_input[0]["type"], json!("compaction"));

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    let compact_input = compact_bodies[0]["input"].as_array().unwrap();
    assert_eq!(compact_input.len(), 7);
    assert_eq!(compact_input.last().unwrap()["type"], json!("message"));
}

#[tokio::test]
async fn openai_responses_replays_compacted_prefix_with_retained_tool_tail() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let compact_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compact_bodies_for_server = compact_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = match attempt {
                            0 => openai_tool_call_response("resp_1"),
                            _ => openai_text_response("resp_2", "done"),
                        };
                        Json(response)
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(body): Json<Value>| {
                    let captured = compact_bodies_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        Json(json!({
                            "output": [
                                { "type": "compaction", "encrypted_content": "opaque" },
                                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "recent" }] }
                            ]
                        }))
                    }
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    set_remote_compaction_trigger(&mut fixture.config, "openai/gpt-5.4");
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_large_window_continuation_with_prompt_frame())
        .await
        .unwrap();

    let first_compaction = first
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("post-response remote compaction diagnostics");
    assert_eq!(first_compaction.status, "compacted");
    assert_eq!(
        first_compaction.trigger_reason.as_deref(),
        Some("token_budget_pressure")
    );
    assert_eq!(first_compaction.input_items, Some(8));

    assert!(
        second
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
            .is_none(),
        "the second request should replay the compacted prefix and retained tool tail without another compact pass"
    );

    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    assert!(response_bodies[1].get("previous_response_id").is_none());
    let replayed_input = response_bodies[1]["input"].as_array().unwrap();
    assert_eq!(replayed_input[0]["type"], json!("compaction"));
    assert!(!response_bodies[1].to_string().contains("message 0"));
    let tool_pair_index = replayed_input
        .windows(2)
        .position(|items| {
            items[0]["type"] == json!("function_call")
                && items[1]["type"] == json!("function_call_output")
                && items[0]["call_id"] == json!("exec-1")
                && items[1]["call_id"] == json!("exec-1")
        })
        .expect("retained tool call should be followed by its tool output");
    assert!(tool_pair_index > 0);

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    assert_eq!(compact_bodies[0]["input"].as_array().unwrap().len(), 8);
    assert!(!compact_bodies[0].to_string().contains("exec-1"));
}

#[tokio::test]
async fn openai_responses_local_shape_change_does_not_rewrite_compacted_provider_window() {
    let response_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let response_bodies_for_server = response_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = response_bodies_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = match attempt {
                            0 => openai_text_response("resp_1", "ready"),
                            _ => openai_text_response("resp_2", "done"),
                        };
                        Json(response)
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(|Json(_body): Json<Value>| async {
                    Json(json!({
                        "output": [
                            { "type": "compaction", "encrypted_content": "opaque" }
                        ]
                    }))
                }),
            ),
    )
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_large_window_paired_request_with_prompt_frame())
        .await
        .unwrap();
    let mut changed = provider_large_window_paired_followup_with_prompt_frame();
    changed
        .prompt_frame
        .cache
        .as_mut()
        .expect("cache identity")
        .compression_epoch += 1;
    let response = provider.complete_turn(changed).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("request_shape_changed")
    );
    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    assert!(response_bodies[1].get("previous_response_id").is_none());
    assert_eq!(response_bodies[1]["input"][0]["type"], json!("message"));
    assert_ne!(response_bodies[1]["input"][0]["type"], json!("compaction"));
}

#[tokio::test]
async fn openai_responses_does_not_reuse_without_continuation_scope() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();
    let mut initial = provider_turn_request_with_prompt_frame();
    initial.continuation_scope_id = None;
    let mut continuation = provider_continuation_request_with_prompt_frame();
    continuation.continuation_scope_id = None;
    continuation.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);

    provider.complete_turn(initial).await.unwrap();
    let response = provider.complete_turn(continuation).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("missing_continuation_scope")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
    assert!(bodies[1]["input"].as_array().unwrap().len() > 1);
}

#[tokio::test]
async fn openai_responses_scopes_incremental_state_by_continuation_scope_id() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let mut other_agent = provider_continuation_request_with_prompt_frame();
    other_agent.continuation_scope_id = ContinuationScopeId::new("other");
    let other_response = provider.complete_turn(other_agent).await.unwrap();
    let same_agent_response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(
        other_response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("not_applicable_initial_request")
    );
    assert_eq!(
        same_agent_response
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[1].get("previous_response_id").is_none());
    assert_eq!(bodies[2]["previous_response_id"], json!("resp_1"));
}

#[tokio::test]
async fn openai_responses_cache_key_change_preserves_lineage_after_shape_fallback() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                match attempts.fetch_add(1, Ordering::SeqCst) {
                    0 => Json(openai_tool_call_response_for("resp_1", "exec-1")),
                    1 => Json(openai_tool_call_response_for("resp_2", "exec-2")),
                    _ => Json(openai_text_response("resp_3", "done")),
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let mut changed = provider_continuation_request_with_prompt_frame();
    let changed_cache = changed.prompt_frame.cache.as_mut().unwrap();
    changed_cache.prompt_cache_key = "cache-key-v2".into();
    let fallback = provider.complete_turn(changed.clone()).await.unwrap();

    assert_eq!(
        fallback
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("request_shape_changed")
    );

    changed.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-2".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-2".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);
    let resumed = provider.complete_turn(changed).await.unwrap();

    assert_eq!(
        resumed
            .request_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.request_lowering_mode.as_str()),
        Some("incremental_continuation")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["prompt_cache_key"], json!("cache-key"));
    assert_eq!(bodies[1]["prompt_cache_key"], json!("cache-key-v2"));
    assert!(bodies[1].get("previous_response_id").is_none());
    assert_eq!(bodies[2]["prompt_cache_key"], json!("cache-key-v2"));
    assert_eq!(bodies[2]["previous_response_id"], json!("resp_2"));
}

#[tokio::test]
async fn openai_responses_falls_back_when_prompt_shape_changes() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let mut changed = provider_continuation_request_with_prompt_frame();
    changed.prompt_frame.system_prompt = "changed rendered system".into();
    let response = provider.complete_turn(changed).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("request_shape_changed")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
    assert!(bodies[1]["input"].as_array().unwrap().len() > 1);
}

#[tokio::test]
async fn openai_responses_falls_back_when_conversation_is_not_append_only() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("conversation_not_strict_append_only")
    );
    let diagnostics = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
        .expect("incremental continuation diagnostics");
    assert_eq!(diagnostics.expected_prefix_items, Some(2));
    assert_eq!(diagnostics.first_mismatch_index, Some(1));
    assert_eq!(
        diagnostics.previous_item_type.as_deref(),
        Some("function_call")
    );
    assert_eq!(diagnostics.current_item_type, None);
    assert_eq!(diagnostics.previous_item_id.as_deref(), Some("exec-1"));
    assert_eq!(diagnostics.current_item_id, None);
    assert!(diagnostics.previous_item_hash.is_some());
    assert!(diagnostics.current_item_hash.is_none());
    assert!(diagnostics.request_shape_hash.is_some());
    assert_eq!(diagnostics.first_mismatch_path.as_deref(), Some("/1"));
    assert_eq!(
        diagnostics.mismatch_kind.as_deref(),
        Some("length_mismatch")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn openai_responses_reports_semantic_mismatch_path_for_changed_assistant_text() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_text_response("resp_1", "ready"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_text_followup_with_prompt_frame("changed"))
        .await
        .unwrap();

    let diagnostics = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
        .expect("incremental continuation diagnostics");
    assert_eq!(
        diagnostics.fallback_reason.as_deref(),
        Some("conversation_not_strict_append_only")
    );
    assert_eq!(diagnostics.first_mismatch_index, Some(1));
    assert_eq!(
        diagnostics.first_mismatch_path.as_deref(),
        Some("/1/content/0/text")
    );
    assert_eq!(
        diagnostics.mismatch_kind.as_deref(),
        Some("semantic_mismatch")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert!(bodies[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn openai_responses_falls_back_when_tool_schema_changes() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1"))
                } else {
                    Json(openai_text_response("resp_2", "done"))
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();
    let mut first = provider_turn_request_with_prompt_frame();
    first.tools = vec![tool_spec_named("ExecCommand")];
    let mut second = provider_continuation_request_with_prompt_frame();
    second.tools = vec![tool_spec_named("ExecCommand"), sleep_tool_spec()];

    provider.complete_turn(first).await.unwrap();
    let response = provider.complete_turn(second).await.unwrap();

    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("request_shape_changed")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn openai_responses_invalidates_incremental_state_after_error_before_retry() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let attempts = attempts_for_server.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let has_previous = body.get("previous_response_id").is_some();
                captured.lock().unwrap().push(body);
                if attempt == 0 {
                    Json(openai_tool_call_response("resp_1")).into_response()
                } else if has_previous {
                    (StatusCode::INTERNAL_SERVER_ERROR, "temporary failure").into_response()
                } else {
                    Json(openai_text_response("resp_2", "retry full")).into_response()
                }
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = build_provider_from_config(&fixture.config).unwrap();

    provider
        .complete_turn(provider_turn_request_with_prompt_frame())
        .await
        .unwrap();
    let response = provider
        .complete_turn(provider_continuation_request_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(
        response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.incremental_continuation.as_ref())
            .and_then(|diagnostics| diagnostics.fallback_reason.as_deref()),
        Some("not_applicable_initial_request")
    );
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert!(bodies[2].get("previous_response_id").is_none());
}
