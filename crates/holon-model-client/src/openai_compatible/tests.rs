use super::*;
use crate::{ContentPart, ContinuationState, ImageUrl, ToolDefinition};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn test_server(response: &'static [u8], delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection");
        let mut request = [0_u8; 8192];
        let _ = stream.read(&mut request);
        thread::sleep(delay);
        stream.write_all(response).expect("response");
    });
    format!("http://{address}/v1")
}

#[test]
fn lowers_tools_extensions_and_multimodal_content() {
    let mut request = CompletionRequest::new(
        "gpt-test",
        vec![
            Message::system("You are concise."),
            Message {
                role: Role::User,
                content: vec![
                    ContentPart::text("Describe this."),
                    ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            url: "https://example.test/image.png".to_string(),
                            detail: Some("low".to_string()),
                        },
                    },
                ],
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
    );
    request.max_output_tokens = Some(128);
    request.tools.push(ToolDefinition {
        name: "lookup".to_string(),
        description: Some("Look something up.".to_string()),
        parameters: json!({"type": "object", "properties": {}}),
        strict: true,
    });
    request.extensions.reasoning_effort = Some("low".to_string());
    request
        .extensions
        .extra
        .insert("temperature".to_string(), json!(0));

    let body = OpenAiCompatibleClient::build_request_body(&request).unwrap();
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(body["tools"][0]["function"]["name"], "lookup");
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["temperature"], 0);
}

#[test]
fn parses_tool_calls_usage_without_unsupported_continuation_state() {
    let value = json!({
        "id": "chatcmpl-test",
        "model": "gpt-test",
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "arguments": "{\"query\":\"rust\"}"
                    }
                }]
            }
        }],
        "usage": {
            "prompt_tokens": 7,
            "completion_tokens": 3,
            "total_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 2}
        }
    });

    let response = parse_response(value).unwrap();
    assert_eq!(response.id.as_deref(), Some("chatcmpl-test"));
    assert!(response.continuation.is_none());
    assert_eq!(response.tool_calls[0].arguments["query"], "rust");
    assert_eq!(response.usage.unwrap().cached_input_tokens, Some(2));
}

#[test]
fn parses_empty_tool_call_arguments_as_an_empty_object() {
    let response = parse_response(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-noargs",
                    "type": "function",
                    "function": {
                        "name": "get_status",
                        "arguments": ""
                    }
                }]
            }
        }]
    }))
    .unwrap();

    assert_eq!(response.tool_calls[0].arguments, json!({}));
}

#[test]
fn parses_missing_tool_call_arguments_as_an_empty_object() {
    let response = parse_response(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-missing-args",
                    "type": "function",
                    "function": {
                        "name": "get_status"
                    }
                }]
            }
        }]
    }))
    .unwrap();

    assert_eq!(response.tool_calls[0].arguments, json!({}));
}

#[test]
fn parses_null_tool_call_arguments_as_an_empty_object() {
    let response = parse_response(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-null-args",
                    "type": "function",
                    "function": {
                        "name": "get_status",
                        "arguments": null
                    }
                }]
            }
        }]
    }))
    .unwrap();

    assert_eq!(response.tool_calls[0].arguments, json!({}));
}

#[tokio::test]
async fn contract_maps_success_and_http_errors_without_live_provider_access() {
    let endpoint = test_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 127\r\nx-request-id: req-server\r\nConnection: close\r\n\r\n{\"id\":\"chatcmpl-test\",\"model\":\"gpt-test\",\"choices\":[{\"finish_reason\":\"stop\",\"message\":{\"role\":\"assistant\",\"content\":\"hello\"}}]}",
            Duration::ZERO,
        );
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: endpoint,
        api_key: Some("test-token".into()),
        ..Default::default()
    })
    .expect("client");
    let mut context = CallContext {
        request_id: Some("req-client".into()),
        ..Default::default()
    };
    context
        .headers
        .insert("x-test-header".into(), "test-value".into());
    let response = client
        .complete(
            &context,
            CompletionRequest::new("gpt-test", vec![Message::user("hello")]),
        )
        .await
        .expect("completion");
    assert_eq!(response.id.as_deref(), Some("chatcmpl-test"));
    assert_eq!(response.message.content, vec![ContentPart::text("hello")]);

    let endpoint = test_server(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 7\r\nx-request-id: req-error\r\nConnection: close\r\n\r\nfailure",
            Duration::ZERO,
        );
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: endpoint,
        ..Default::default()
    })
    .expect("client");
    let error = client
        .complete(
            &CallContext::default(),
            CompletionRequest::new("gpt-test", vec![Message::user("hello")]),
        )
        .await
        .expect_err("http error");
    assert!(matches!(
        error,
        ClientError::Http {
            status: 503,
            request_id: Some(request_id),
            ..
        } if request_id == "req-error"
    ));
}

#[tokio::test]
async fn contract_classifies_decode_and_timeout_errors() {
    let endpoint = test_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json",
        Duration::ZERO,
    );
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: endpoint,
        ..Default::default()
    })
    .expect("client");
    let error = client
        .complete(
            &CallContext::default(),
            CompletionRequest::new("gpt-test", vec![Message::user("hello")]),
        )
        .await
        .expect_err("decode error");
    assert!(matches!(error, ClientError::Decode(_)));

    let endpoint = test_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        Duration::from_millis(200),
    );
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: endpoint,
        ..Default::default()
    })
    .expect("client");
    let error = client
        .complete(
            &CallContext {
                timeout: Some(Duration::from_millis(20)),
                ..Default::default()
            },
            CompletionRequest::new("gpt-test", vec![Message::user("hello")]),
        )
        .await
        .expect_err("timeout");
    assert!(matches!(error, ClientError::Transport(_)));
}

#[test]
fn rejects_empty_content_without_finish_reason_or_tool_calls() {
    let error = parse_response(json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": ""
            }
        }]
    }))
    .unwrap_err();

    assert!(
        matches!(error, ClientError::Decode(message) if message.contains("no supported content"))
    );
}

#[test]
fn rejects_continuation_until_request_replay_is_supported() {
    let mut request = CompletionRequest::new("gpt-test", vec![Message::user("continue")]);
    request.continuation = Some(ContinuationState::response_id("chatcmpl-test"));

    let error = OpenAiCompatibleClient::build_request_body(&request).unwrap_err();
    assert!(
        matches!(error, ClientError::Unsupported(message) if message.contains("request replay"))
    );
}

#[test]
fn preserves_assistant_tool_calls_for_tool_result_follow_up() {
    let response = parse_response(json!({
        "id": "chatcmpl-tool-roundtrip",
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-roundtrip",
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "arguments": "{\"query\":\"rust\"}"
                    }
                }]
            }
        }]
    }))
    .unwrap();

    let request = CompletionRequest::new(
        "gpt-test",
        vec![
            response.message,
            Message::tool("call-roundtrip", "{\"result\":\"ok\"}"),
        ],
    );
    let body = OpenAiCompatibleClient::build_request_body(&request).unwrap();

    assert_eq!(body["messages"][0]["role"], "assistant");
    assert_eq!(body["messages"][0]["content"], Value::Null);
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"query\":\"rust\"}"
    );
    assert_eq!(body["messages"][1]["role"], "tool");
    assert_eq!(body["messages"][1]["tool_call_id"], "call-roundtrip");
}

#[test]
fn rejects_empty_requests() {
    let request = CompletionRequest::new("", Vec::new());
    let error = OpenAiCompatibleClient::build_request_body(&request).unwrap_err();
    assert!(matches!(error, ClientError::InvalidRequest(_)));
}

#[test]
fn debug_redacts_api_keys_and_header_values() {
    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer do-not-log-this-header".to_string(),
    );
    let config = OpenAiCompatibleConfig {
        api_key: Some("do-not-log-this-key".to_string()),
        headers,
        ..Default::default()
    };

    let config_debug = format!("{config:?}");
    assert!(config_debug.contains("Authorization"));
    assert!(!config_debug.contains("do-not-log-this-key"));
    assert!(!config_debug.contains("do-not-log-this-header"));

    let client = OpenAiCompatibleClient::new(config).unwrap();
    let client_debug = format!("{client:?}");
    assert!(client_debug.to_ascii_lowercase().contains("authorization"));
    assert!(!client_debug.contains("do-not-log-this-key"));
    assert!(!client_debug.contains("do-not-log-this-header"));
}
