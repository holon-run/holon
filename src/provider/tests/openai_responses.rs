//! OpenAI Responses API request/response lowering tests.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use super::support::*;
use super::*;
use crate::config::{ModelRef, ProviderEndpointId, ProviderId, ProviderTransportKind};
use crate::model_catalog::ModelRuntimeOverride;
use crate::provider::{
    transports::OpenAiCompactionPolicy, ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
};
use axum::{http::StatusCode, routing::post, Json, Router};
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

fn deepseek_reasoning_and_tool_call_sse_response(response_id: &str) -> String {
    let response = json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 12, "output_tokens": 4 },
        "output": [{
            "type": "reasoning",
            "content": [{
                "type": "reasoning_text",
                "text": "inspect the workspace first"
            }]
        }, {
            "type": "custom_tool_call",
            "call_id": "patch-1",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** Add File: note.txt\n+ok\n*** End Patch\n"
        }]
    });
    format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": response,
        })
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
async fn tokenhub_responses_replays_full_history_without_remote_compaction() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_server = attempts.clone();
    let compact_attempts = Arc::new(AtomicUsize::new(0));
    let compact_for_server = compact_attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = captured_for_server.clone();
                    let attempts = attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            Json(openai_tool_call_response("resp_1"))
                        } else {
                            Json(openai_text_response("resp_2", "done"))
                        }
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move || {
                    let attempts = compact_for_server.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                }),
            ),
    )
    .await;
    let fixture = test_config("openai/gpt-5.4", &[], Some("test-key"), None, false);
    let mut config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    config.id = ProviderId::parse("tencent-tokenhub-responses").unwrap();
    config.route_provider = ProviderId::parse("tencent-tokenhub").unwrap();
    config.route_endpoint = ProviderEndpointId::parse("responses").unwrap();
    config.base_url = base_url;
    config.builtin_web_search = None;
    let provider = OpenAiProvider::from_runtime_config_with_compaction_policy_and_context_window(
        &config,
        "glm-5.2",
        1024,
        None,
        fixture.config.home_dir.as_path(),
        OpenAiCompactionPolicy {
            trigger_input_tokens: 1,
        },
    )
    .unwrap();
    let mut request = provider_turn_request_with_prompt_frame();
    request.tools = vec![tool_spec_named("ExecCommand")];
    provider.complete_turn(request).await.unwrap();
    let mut followup = provider_continuation_request_with_prompt_frame();
    followup.tools = vec![tool_spec_named("ExecCommand")];
    provider.complete_turn(followup).await.unwrap();

    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    for body in bodies.iter() {
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["store"], json!(false));
        assert_ne!(body["stream"], json!(true));
        let tools = body["tools"].as_array().expect("declared function tool");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], json!("function"));
        assert_eq!(tools[0]["name"], json!("ExecCommand"));
    }
    let replay = bodies[1]["input"].as_array().unwrap();
    assert!(replay.len() > bodies[0]["input"].as_array().unwrap().len());
    assert!(replay.iter().any(|item| item["type"] == "function_call"));
    assert!(replay
        .iter()
        .any(|item| item["type"] == "function_call_output"));
    assert_eq!(compact_attempts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deepseek_responses_streams_and_replays_full_history_across_provider_restart() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let response_attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts_for_server = response_attempts.clone();
    let compact_attempts = Arc::new(AtomicUsize::new(0));
    let compact_attempts_for_server = compact_attempts.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let captured = captured_for_server.clone();
                    let attempts = response_attempts_for_server.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let response = if attempt == 0 {
                            deepseek_reasoning_and_tool_call_sse_response("resp_1")
                        } else {
                            openai_text_sse_response("resp_2", "done")
                        };
                        ([("content-type", "text/event-stream")], response)
                    }
                }),
            )
            .route(
                "/responses/compact",
                post(move |Json(_body): Json<Value>| {
                    let attempts = compact_attempts_for_server.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                }),
            ),
    )
    .await;
    let fixture = test_config(
        "deepseek/deepseek-v4-pro",
        &[],
        Some("deepseek-key"),
        None,
        false,
    );
    let mut provider_config = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    provider_config.id = deepseek.clone();
    provider_config.route_provider = deepseek;
    provider_config.route_endpoint = ProviderEndpointId::parse("responses").unwrap();
    provider_config.transport = ProviderTransportKind::OpenAiResponses;
    provider_config.base_url = base_url;
    provider_config.builtin_web_search = None;
    provider_config.reasoning_effort = Some("high".into());

    let first_provider =
        OpenAiProvider::from_runtime_config_with_compaction_policy_and_context_window(
            &provider_config,
            "deepseek-v4-pro",
            384_000,
            None,
            fixture.config.home_dir.as_path(),
            OpenAiCompactionPolicy {
                trigger_input_tokens: 1,
            },
        )
        .unwrap();
    let mut first_request = provider_turn_request_with_prompt_frame();
    first_request.tools = vec![
        crate::tool::tools::apply_patch_tool::definition_for_surface(
            crate::tool::apply_patch::ApplyPatchSurface::CodexDslFreeform,
        )
        .unwrap()
        .spec,
    ];
    first_request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::DeepSeek,
        provider_id: "deepseek".into(),
        provider_model_ref: "deepseek@responses/deepseek-v4-pro".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "deepseek_web_search".into(),
        max_results: None,
    });
    let first = first_provider.complete_turn(first_request).await.unwrap();
    assert!(matches!(
        &first.blocks[0],
        ModelBlock::ReasoningText { text } if text == "inspect the workspace first"
    ));
    assert!(matches!(
        &first.blocks[1],
        ModelBlock::ToolUse { id, name, kind, .. }
            if id == "patch-1"
                && name == "ApplyPatch"
                && kind == &crate::provider::ModelToolCallKind::Custom
    ));
    let diagnostics = first
        .request_diagnostics
        .as_ref()
        .expect("DeepSeek request diagnostics");
    assert_eq!(diagnostics.provider_id.as_deref(), Some("deepseek"));
    assert_eq!(
        diagnostics.provider_model_ref.as_deref(),
        Some("deepseek@responses/deepseek-v4-pro")
    );
    assert_eq!(
        diagnostics.provider_transport.as_deref(),
        Some("openai_responses")
    );
    assert_eq!(diagnostics.endpoint_dialect.as_deref(), Some("responses"));
    assert_eq!(diagnostics.streaming, Some(true));
    assert_eq!(diagnostics.reasoning_effort.as_deref(), Some("high"));
    let stable_prefix = diagnostics
        .stable_prefix
        .as_ref()
        .expect("stable prefix diagnostics");
    assert_eq!(stable_prefix.schema_version, 1);
    assert_eq!(stable_prefix.algorithm, "sha256");
    assert_eq!(stable_prefix.dynamic_tail_items, 1);
    assert!(stable_prefix
        .components
        .iter()
        .any(|component| component.name == "tools"));
    assert_eq!(
        diagnostics
            .native_web_search
            .as_ref()
            .map(|search| search.lowered),
        Some(true)
    );

    let persisted_blocks = serde_json::to_vec(&first.blocks).unwrap();
    drop(first_provider);
    let restored_blocks: Vec<ModelBlock> = serde_json::from_slice(&persisted_blocks).unwrap();

    let mut followup = provider_turn_request_with_prompt_frame();
    followup.tools = vec![
        crate::tool::tools::apply_patch_tool::definition_for_surface(
            crate::tool::apply_patch::ApplyPatchSurface::CodexDslFreeform,
        )
        .unwrap()
        .spec,
    ];
    followup.conversation.extend([
        ConversationMessage::AssistantBlocks(restored_blocks),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "patch-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);
    let restarted_provider =
        OpenAiProvider::from_runtime_config_with_compaction_policy_and_context_window(
            &provider_config,
            "deepseek-v4-pro",
            384_000,
            None,
            fixture.config.home_dir.as_path(),
            OpenAiCompactionPolicy {
                trigger_input_tokens: 1,
            },
        )
        .unwrap();
    let second = restarted_provider.complete_turn(followup).await.unwrap();
    assert!(matches!(
        &second.blocks[0],
        ModelBlock::Text { text } if text == "done"
    ));

    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    for body in bodies.iter() {
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("prompt_cache_key").is_none());
    }
    assert!(bodies[0]["tools"]
        .as_array()
        .expect("DeepSeek tools")
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search" })));
    assert!(bodies.iter().all(|body| {
        body["tools"]
            .as_array()
            .expect("DeepSeek tools")
            .iter()
            .any(|tool| tool["type"] == "custom" && tool["name"] == "apply_patch")
    }));
    let replayed = bodies[1]["input"].as_array().unwrap();
    assert!(replayed.iter().any(|item| {
        item["type"] == "reasoning"
            && item["content"][0]["type"] == "reasoning_text"
            && item["content"][0]["text"] == "inspect the workspace first"
    }));
    assert!(replayed.iter().any(|item| {
        item["type"] == "custom_tool_call"
            && item["call_id"] == "patch-1"
            && item["name"] == "apply_patch"
    }));
    assert!(replayed.iter().any(|item| {
        item["type"] == "custom_tool_call_output"
            && item["call_id"] == "patch-1"
            && item["output"] == "ok"
    }));
    assert_eq!(compact_attempts.load(Ordering::SeqCst), 0);
}

fn provider_large_window_continuation_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_large_window_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
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
