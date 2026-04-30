//! OpenAI Responses API request/response lowering tests.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use super::support::*;
use super::*;
use crate::config::ProviderId;
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};

fn openai_tool_call_response(response_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 3, "output_tokens": 2 },
        "output": [{
            "type": "function_call",
            "call_id": "exec-1",
            "name": "ExecCommand",
            "arguments": "{\"cmd\":\"printf ok\"}"
        }]
    })
}

fn openai_text_response(response_id: &str, text: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 2, "output_tokens": 1 },
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }]
        }]
    })
}

fn provider_large_window_request_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation = (0..8)
        .map(|index| ConversationMessage::UserText(format!("message {index}")))
        .collect();
    request
}

fn provider_large_window_continuation_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_large_window_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
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
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[1]["input"][0]["type"], json!("function_call_output"));
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
                            0 => openai_text_response("resp_1", "ready"),
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
                                { "type": "compaction", "encrypted_content": "opaque-1" },
                                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "recent" }] },
                                { "type": "compaction", "encrypted_content": "opaque-2" }
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
    assert_eq!(remote_compaction.output_items, Some(3));
    assert_eq!(remote_compaction.compaction_items, Some(2));
    assert_eq!(remote_compaction.latest_compaction_index, Some(2));
    assert_eq!(
        remote_compaction.encrypted_content_bytes.as_deref(),
        Some([8usize, 8usize].as_slice())
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
    assert_eq!(replayed_input[2]["type"], json!("compaction"));
    assert_eq!(replayed_input.last().unwrap()["type"], json!("message"));

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    assert_eq!(compact_bodies[0]["input"].as_array().unwrap().len(), 11);
    assert_eq!(compact_bodies[0]["tools"], json!([]));
    assert_eq!(compact_bodies[0]["parallel_tool_calls"], json!(false));
}

#[tokio::test]
async fn openai_responses_skips_remote_compaction_for_unpaired_tool_call() {
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
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let response = provider
        .complete_turn(provider_large_window_request_with_prompt_frame())
        .await
        .unwrap();

    let remote_compaction = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("remote compaction skip diagnostics");
    assert_eq!(remote_compaction.status, "skipped_unpaired_tool_call");
    assert_eq!(remote_compaction.input_items, Some(9));
    assert!(compact_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn openai_responses_remote_compacts_before_request_after_tool_output_pairs_call() {
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
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    let first = provider
        .complete_turn(provider_large_window_request_with_prompt_frame())
        .await
        .unwrap();
    let second = provider
        .complete_turn(provider_large_window_continuation_with_prompt_frame())
        .await
        .unwrap();

    assert_eq!(
        first
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
            .map(|diagnostics| diagnostics.status.as_str()),
        Some("skipped_unpaired_tool_call")
    );
    let remote_compaction = second
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .expect("pre-request remote compaction diagnostics");
    assert_eq!(remote_compaction.status, "compacted");
    assert_eq!(
        remote_compaction.trigger_reason.as_deref(),
        Some("provider_window_item_threshold_before_request")
    );
    assert_eq!(remote_compaction.input_items, Some(10));

    let response_bodies = response_bodies.lock().unwrap();
    assert_eq!(response_bodies.len(), 2);
    assert!(response_bodies[1].get("previous_response_id").is_none());
    let replayed_input = response_bodies[1]["input"].as_array().unwrap();
    assert_eq!(replayed_input[0]["type"], json!("compaction"));
    assert_eq!(
        replayed_input.last().unwrap()["type"],
        json!("function_call_output")
    );

    let compact_bodies = compact_bodies.lock().unwrap();
    assert_eq!(compact_bodies.len(), 1);
    assert_eq!(compact_bodies[0]["input"].as_array().unwrap().len(), 10);
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
async fn openai_responses_does_not_reuse_without_prompt_cache_scope() {
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
    let mut continuation = provider_turn_request();
    continuation.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);

    provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();
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
async fn openai_responses_scopes_incremental_state_by_prompt_cache_identity() {
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
    other_agent.prompt_frame.cache.as_mut().unwrap().agent_id = "other".into();
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
    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
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
