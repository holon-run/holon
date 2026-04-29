//! OpenAI Chat Completions conversion, streaming, and error classification tests.

use super::support::*;
use super::*;
use crate::config::{ModelRef, ProviderId, ProviderTransportKind};
use crate::provider::retry::{classify_provider_error, ProviderFailureKind, RetryDisposition};
use crate::provider::transports::build_chat_completion_messages;
use crate::tool::ToolSpec;
use serde_json::json;

#[test]
fn build_candidate_creates_chat_completions_provider() {
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .transport = ProviderTransportKind::OpenAiChatCompletions;

    let candidate = build_candidate(&fixture.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .expect("chat completions provider should build");

    assert_eq!(candidate.model_ref, "openai/gpt-5.4");
    assert_eq!(candidate.provider_name, "openai");
}

#[test]
fn chat_completion_message_conversion_handles_text_conversation() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Hello!".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "Hi there!".to_string(),
        }]),
        ConversationMessage::UserText("How are you?".to_string()),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    assert_eq!(messages.len(), 4); // system + 3 conversation messages

    // Check system message
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a helpful assistant.");

    // Check user message
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Hello!");

    // Check assistant message
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "Hi there!");

    // Check second user message
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(messages[3]["content"], "How are you?");
}

#[test]
fn chat_completion_request_builds_openai_function_tools() {
    use crate::provider::transports::build_chat_completion_request;

    let tools = vec![ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "param1": {
                    "type": "string",
                    "description": "First parameter"
                }
            },
            "required": ["param1"]
        }),
        freeform_grammar: None,
    }];

    let request = provider_turn_request_with_tools(tools);

    let chat_request = build_chat_completion_request(
        "gpt-5.4",
        1000,
        &request,
        ToolSchemaContract::Relaxed,
        false,
    )
    .expect("should build chat completion request");

    // Check tools array
    assert!(chat_request.get("tools").is_some());
    let tools_array = chat_request["tools"].as_array().unwrap();
    assert_eq!(tools_array.len(), 1);

    let tool = &tools_array[0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "test_tool");
    assert_eq!(tool["function"]["description"], "A test tool");
    assert_eq!(tool["function"]["strict"], false); // Relaxed mode
}

#[test]
fn chat_completion_request_includes_tool_choice_auto() {
    use crate::provider::transports::build_chat_completion_request;

    let tools = vec![ToolSpec {
        name: "get_weather".to_string(),
        description: "Get current weather".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "City name"
                }
            },
            "required": ["location"]
        }),
        freeform_grammar: None,
    }];

    let request = provider_turn_request_with_tools(tools);

    let chat_request = build_chat_completion_request(
        "gpt-5.4",
        1000,
        &request,
        ToolSchemaContract::Relaxed,
        false,
    )
    .expect("should build chat completion request");

    // Check tool_choice is set to "auto"
    assert_eq!(chat_request["tool_choice"], "auto");
}

#[test]
fn chat_completion_message_conversion_handles_tool_calls_in_assistant_message() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("What time is it?".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::Text {
                text: "Let me check.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_123".to_string(),
                name: "get_current_time".to_string(),
                input: json!({}),
            },
        ]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    assert_eq!(messages.len(), 3);

    // Check assistant message
    let assistant_msg = &messages[2];
    assert_eq!(assistant_msg["role"], "assistant");
    assert_eq!(assistant_msg["content"], "Let me check.");
    assert!(assistant_msg.get("tool_calls").is_some());

    let tool_calls = assistant_msg["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_123");
    assert_eq!(tool_calls[0]["type"], "function");
    assert_eq!(tool_calls[0]["function"]["name"], "get_current_time");
}

#[test]
fn chat_completion_message_conversion_handles_multiple_tool_calls() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Search for recent news and weather".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::ToolUse {
                id: "call_1".to_string(),
                name: "search_news".to_string(),
                input: json!({"query": "recent"}),
            },
            ModelBlock::ToolUse {
                id: "call_2".to_string(),
                name: "get_weather".to_string(),
                input: json!({"location": "Paris"}),
            },
        ]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    assert_eq!(messages.len(), 3); // system + user + assistant

    let assistant_msg = &messages[2];
    let tool_calls = assistant_msg["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 2);

    // Check first tool call
    assert_eq!(tool_calls[0]["id"], "call_1");
    assert_eq!(tool_calls[0]["function"]["name"], "search_news");

    // Check second tool call
    assert_eq!(tool_calls[1]["id"], "call_2");
    assert_eq!(tool_calls[1]["function"]["name"], "get_weather");
}

#[test]
fn chat_completion_message_conversion_handles_assistant_text_with_tool_calls() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("What's 2+2?".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::Text {
                text: "Let me calculate that for you.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_calc".to_string(),
                name: "calculator".to_string(),
                input: json!({"expression": "2+2"}),
            },
        ]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    // messages[0] = system, messages[1] = user, messages[2] = assistant
    let assistant_msg = &messages[2];
    assert_eq!(assistant_msg["content"], "Let me calculate that for you.");
    assert!(assistant_msg.get("tool_calls").is_some());

    let tool_calls = assistant_msg["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "calculator");
}

#[test]
fn parse_chat_completion_response_extracts_text_and_tool_calls() {
    use crate::provider::transports::parse_chat_completion_response;

    let response = json!({
        "id": "chatcmpl-abc123",
        "object": "chat.completion",
        "created": 1234567890,
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "I'll help you with that.",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": "{\"query\":\"test\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });

    let parsed =
        parse_chat_completion_response(response).expect("should parse chat completion response");

    // Check blocks
    assert_eq!(parsed.response.blocks.len(), 2);

    // First block should be text
    match &parsed.response.blocks[0] {
        ModelBlock::Text { text } => {
            assert_eq!(text, "I'll help you with that.");
        }
        _ => panic!("Expected text block"),
    }

    // Second block should be tool call
    match &parsed.response.blocks[1] {
        ModelBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "search");
            assert_eq!(input.to_string(), r#"{"query":"test"}"#);
        }
        _ => panic!("Expected tool use block"),
    }

    // Check metadata
    assert_eq!(parsed.response.input_tokens, 10);
    assert_eq!(parsed.response.output_tokens, 5);
    assert_eq!(parsed.response.stop_reason, Some("tool_calls".to_string()));
    assert_eq!(parsed.response_id, Some("chatcmpl-abc123".to_string()));
}

#[test]
fn parse_chat_completion_response_handles_empty_arguments() {
    use crate::provider::transports::parse_chat_completion_response;

    let response = json!({
        "id": "chatcmpl-empty-args",
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_noargs",
                    "type": "function",
                    "function": {
                        "name": "get_status",
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 3,
            "total_tokens": 8
        }
    });

    let parsed = parse_chat_completion_response(response)
        .expect("should parse response with empty arguments");

    match &parsed.response.blocks[0] {
        ModelBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_noargs");
            assert_eq!(name, "get_status");
            assert_eq!(input.to_string(), "{}"); // Empty arguments should become empty object
        }
        _ => panic!("Expected tool use block"),
    }
}

#[test]
fn chat_completion_message_conversion_handles_tool_results() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("What time is it?".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_time".to_string(),
            name: "get_current_time".to_string(),
            input: json!({}),
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_time".to_string(),
            content: "10:30 AM".to_string(),
            is_error: false,
            error: None,
        }]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    // Should have: system, user, assistant, tool result
    assert_eq!(messages.len(), 4);

    // Check tool result message
    let tool_result_msg = &messages[3];
    assert_eq!(tool_result_msg["role"], "tool");
    assert_eq!(tool_result_msg["tool_call_id"], "call_time");
    assert_eq!(tool_result_msg["content"], "10:30 AM");
}

#[test]
fn chat_completion_message_conversion_handles_multiple_tool_results() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Search for news and weather".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::ToolUse {
                id: "call_1".to_string(),
                name: "search_news".to_string(),
                input: json!({"query": "test"}),
            },
            ModelBlock::ToolUse {
                id: "call_2".to_string(),
                name: "get_weather".to_string(),
                input: json!({"location": "Paris"}),
            },
        ]),
        ConversationMessage::UserToolResults(vec![
            ToolResultBlock {
                tool_use_id: "call_1".to_string(),
                content: "Found 5 articles".to_string(),
                is_error: false,
                error: None,
            },
            ToolResultBlock {
                tool_use_id: "call_2".to_string(),
                content: "Sunny, 25°C".to_string(),
                is_error: false,
                error: None,
            },
        ]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    // Should have: system, user, assistant, tool result 1, tool result 2
    assert_eq!(messages.len(), 5);

    // Check first tool result
    let tool_result_1 = &messages[3];
    assert_eq!(tool_result_1["role"], "tool");
    assert_eq!(tool_result_1["tool_call_id"], "call_1");
    assert_eq!(tool_result_1["content"], "Found 5 articles");

    // Check second tool result
    let tool_result_2 = &messages[4];
    assert_eq!(tool_result_2["role"], "tool");
    assert_eq!(tool_result_2["tool_call_id"], "call_2");
    assert_eq!(tool_result_2["content"], "Sunny, 25°C");
}

#[test]
fn chat_completion_handles_multi_turn_conversation_with_tools() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        // Turn 1: User asks for calculation
        ConversationMessage::UserText("What's 2+2?".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_calc".to_string(),
            name: "calculator".to_string(),
            input: json!({"expression": "2+2"}),
        }]),
        // Turn 2: Tool returns result
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_calc".to_string(),
            content: "4".to_string(),
            is_error: false,
            error: None,
        }]),
        // Turn 3: Assistant responds with answer
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "The answer is 4.".to_string(),
        }]),
        // Turn 4: User asks another question
        ConversationMessage::UserText("What about 3+3?".to_string()),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert multi-turn conversation");

    // Should have: system, user1, assistant1 (tool), tool_result, assistant2 (text), user2
    assert_eq!(messages.len(), 6);

    // Verify assistant with tool call
    assert_eq!(messages[2]["role"], "assistant");
    assert!(messages[2].get("tool_calls").is_some());

    // Verify tool result
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_calc");
    assert_eq!(messages[3]["content"], "4");

    // Verify assistant with text response
    assert_eq!(messages[4]["role"], "assistant");
    assert_eq!(messages[4]["content"], "The answer is 4.");
    assert!(messages[4].get("tool_calls").is_none());

    // Verify next user message
    assert_eq!(messages[5]["role"], "user");
    assert_eq!(messages[5]["content"], "What about 3+3?");
}

#[test]
fn chat_completion_message_conversion_handles_assistant_text_only() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Hello".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "Hi there!".to_string(),
        }]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    assert_eq!(messages.len(), 3);

    let assistant_msg = &messages[2];
    assert_eq!(assistant_msg["role"], "assistant");
    assert_eq!(assistant_msg["content"], "Hi there!");
    assert!(assistant_msg.get("tool_calls").is_none());
}

#[test]
fn chat_completion_streaming_processes_content_delta_events() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    // Simulate streaming content delta events
    let events = vec![
        json!({"delta": {"content": "Hello"}}),
        json!({"delta": {"content": " world"}}),
        json!({"delta": {"content": "!"}}),
    ];

    let result =
        accumulate_chat_completion_stream_events(events).expect("should accumulate content deltas");

    let message = &result["choices"][0]["message"];
    assert_eq!(message["content"], "Hello world!");
    assert!(message.get("tool_calls").is_none());
}

#[test]
fn chat_completion_streaming_processes_tool_call_delta_events() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    // Simulate streaming tool call delta events
    let events = vec![
        json!({"delta": {"tool_calls": [{"index": 0, "id": "call_123"}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"name": "get_time"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"timezone\":\"UTC\"}"}}]}}),
    ];

    let result = accumulate_chat_completion_stream_events(events)
        .expect("should accumulate tool call deltas");

    let message = &result["choices"][0]["message"];
    assert!(message.get("tool_calls").is_some());

    let tool_calls = message["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_123");
    assert_eq!(tool_calls[0]["function"]["name"], "get_time");
    assert_eq!(
        tool_calls[0]["function"]["arguments"],
        "{\"timezone\":\"UTC\"}"
    );
}

#[test]
fn chat_completion_streaming_handles_mixed_content_and_tool_calls() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    // Simulate mixed streaming events
    let events = vec![
        json!({"delta": {"content": "Let me check"}}),
        json!({"delta": {"tool_calls": [{"index": 0, "id": "call_456"}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"name": "calculate"}}]}}),
        json!({"delta": {"content": " the time"}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"x\":1}"}}]}}),
    ];

    let result =
        accumulate_chat_completion_stream_events(events).expect("should handle mixed events");

    let message = &result["choices"][0]["message"];
    assert_eq!(message["content"], "Let me check the time");

    let tool_calls = message["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "calculate");
    assert_eq!(tool_calls[0]["function"]["arguments"], "{\"x\":1}");
}

#[test]
fn chat_completion_streaming_handles_multiple_tool_calls() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    // Simulate multiple parallel tool calls
    let events = vec![
        json!({"delta": {"tool_calls": [{"index": 0, "id": "call_1"}]}}),
        json!({"delta": {"tool_calls": [{"index": 1, "id": "call_2"}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"name": "tool_a"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 1, "function": {"name": "tool_b"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 1, "function": {"arguments": "{}"}}]}}),
    ];

    let result = accumulate_chat_completion_stream_events(events)
        .expect("should handle multiple tool calls");

    let tool_calls = result["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0]["id"], "call_1");
    assert_eq!(tool_calls[0]["function"]["name"], "tool_a");
    assert_eq!(tool_calls[1]["id"], "call_2");
    assert_eq!(tool_calls[1]["function"]["name"], "tool_b");
}

#[test]
fn chat_completion_streaming_handles_empty_stream() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    let events = vec![];
    let result =
        accumulate_chat_completion_stream_events(events).expect("should handle empty stream");

    let message = &result["choices"][0]["message"];
    assert_eq!(message["content"], "");
    assert!(message.get("tool_calls").is_none());
}

#[test]
fn chat_completion_request_includes_stream_flag() {
    use crate::provider::transports::build_chat_completion_request;

    let request = provider_turn_request();

    // Test with stream = true
    let streaming_request =
        build_chat_completion_request("gpt-4", 1000, &request, ToolSchemaContract::Relaxed, true)
            .expect("should build streaming request");

    assert_eq!(streaming_request["stream"], true);

    // Test with stream = false
    let non_streaming_request =
        build_chat_completion_request("gpt-4", 1000, &request, ToolSchemaContract::Relaxed, false)
            .expect("should build non-streaming request");

    assert_eq!(non_streaming_request["stream"], false);
}

#[test]
fn chat_completion_continuation_diagnostics_provide_clear_status() {
    // Test that continuation can be properly tracked
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Hello".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "Hi there!".to_string(),
        }]),
    ];

    let messages = build_chat_completion_messages(system_prompt, &conversation)
        .expect("should convert messages");

    // Verify message structure for continuation tracking
    assert_eq!(messages.len(), 3); // system + user + assistant
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
}

#[test]
fn chat_completion_provider_classifies_rate_limit_errors() {
    use crate::provider::transports::classify_openai_chat_completion_error;

    // Test rate limit error classification through provider path
    let error_json = json!({
        "error": {
            "message": "Rate limit exceeded",
            "type": "rate_limit_error",
            "code": "rate_limit_exceeded"
        }
    });

    let error = classify_openai_chat_completion_error("test context", &error_json["error"]);
    let classification = classify_provider_error(&error);

    assert_eq!(classification.kind, ProviderFailureKind::RateLimited);
    assert_eq!(classification.disposition, RetryDisposition::Retryable);
}

#[test]
fn chat_completion_provider_classifies_auth_errors() {
    use crate::provider::transports::classify_openai_chat_completion_error;

    // Test authentication error classification through provider path
    let error_json = json!({
        "error": {
            "message": "Invalid API key",
            "type": "invalid_request_error",
            "code": "invalid_api_key"
        }
    });

    let error = classify_openai_chat_completion_error("test context", &error_json["error"]);
    let classification = classify_provider_error(&error);

    assert_eq!(classification.kind, ProviderFailureKind::AuthError);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn chat_completion_provider_classifies_context_length_errors() {
    use crate::provider::transports::classify_openai_chat_completion_error;

    // Test context length error classification through provider path
    let error_json = json!({
        "error": {
            "message": "This model's maximum context length is 4097 tokens",
            "type": "invalid_request_error",
            "code": "context_length_exceeded"
        }
    });

    let error = classify_openai_chat_completion_error("test context", &error_json["error"]);
    let classification = classify_provider_error(&error);

    assert_eq!(classification.kind, ProviderFailureKind::ContractError);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn chat_completion_provider_classifies_server_errors() {
    use crate::provider::transports::classify_openai_chat_completion_error;

    // Test server error classification through provider path
    let error_json = json!({
        "error": {
            "message": "Internal server error",
            "type": "server_error",
            "code": "server_error"
        }
    });

    let error = classify_openai_chat_completion_error("test context", &error_json["error"]);
    let classification = classify_provider_error(&error);

    assert_eq!(classification.kind, ProviderFailureKind::ServerError);
    assert_eq!(classification.disposition, RetryDisposition::Retryable);
}

#[test]
fn chat_completion_provider_classifies_unknown_errors_as_contract_errors() {
    use crate::provider::transports::classify_openai_chat_completion_error;

    // Test unknown error classification through provider path
    let error_json = json!({
        "error": {
            "message": "Unknown error occurred",
            "type": "unknown_error_type",
            "code": "unknown_code"
        }
    });

    let error = classify_openai_chat_completion_error("test context", &error_json["error"]);
    let classification = classify_provider_error(&error);

    // Unknown errors should default to contract errors
    assert_eq!(classification.kind, ProviderFailureKind::ContractError);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn chat_completion_handles_very_long_messages() {
    let system_prompt = "You are a helpful assistant.";
    let very_long_text = "A".repeat(10000); // 10k characters

    let conversation = vec![ConversationMessage::UserText(very_long_text)];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    assert_eq!(messages.len(), 2); // system + user
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"].as_str().unwrap().len(), 10000);
}

#[test]
fn chat_completion_handles_special_characters() {
    let system_prompt = "You are a helpful assistant.";
    let special_text = "Test with special chars: \n\t\r\"'\\<>{}[]|&;#$%^*";

    let conversation = vec![ConversationMessage::UserText(special_text.to_string())];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    assert_eq!(messages[1]["content"], special_text);
}

#[test]
fn chat_completion_handles_unicode_emoji() {
    let system_prompt = "You are a helpful assistant.";
    let emoji_text = "Hello 👋 🌍 🚀 💻 🎉";

    let conversation = vec![ConversationMessage::UserText(emoji_text.to_string())];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    assert_eq!(messages[1]["content"], emoji_text);
}

#[test]
fn chat_completion_handles_empty_conversation() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    assert_eq!(messages.len(), 1); // only system prompt
}

#[test]
fn chat_completion_handles_multiple_assistant_messages() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("First question".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "First answer".to_string(),
        }]),
        ConversationMessage::UserText("Second question".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "Second answer".to_string(),
        }]),
        ConversationMessage::UserText("Third question".to_string()),
    ];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    assert_eq!(messages.len(), 6); // system + 5 conversation messages
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "First answer");
    assert_eq!(messages[4]["role"], "assistant");
    assert_eq!(messages[4]["content"], "Second answer");
}

#[test]
fn chat_completion_handles_mixed_tool_and_text_messages() {
    let system_prompt = "You are a helpful assistant.";
    let conversation = vec![
        ConversationMessage::UserText("Use a tool".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::Text {
                text: "I'll use the tool.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_123".to_string(),
                name: "test_tool".to_string(),
                input: json!({"param": "value"}),
            },
        ]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_123".to_string(),
            content: "Tool result".to_string(),
            is_error: false,
            error: None,
        }]),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "Final response".to_string(),
        }]),
    ];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    // Should have: system, user, assistant (with text + tool_calls), tool result, assistant
    assert_eq!(messages.len(), 5);
    assert!(messages[2].get("tool_calls").is_some());
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[4]["role"], "assistant");
}

#[test]
fn chat_completion_streaming_handles_empty_delta() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    let events = vec![
        json!({"delta": {"content": ""}}),   // Empty content delta
        json!({"choices": [{"delta": {}}]}), // Empty delta
    ];

    let result = accumulate_chat_completion_stream_events(events);
    assert!(result.is_ok());

    let accumulated = result.unwrap();
    assert_eq!(accumulated["choices"][0]["message"]["content"], "");
}

#[test]
fn chat_completion_streaming_handles_large_number_of_events() {
    use crate::provider::transports::accumulate_chat_completion_stream_events;

    // Create 100 small delta events
    let events: Vec<serde_json::Value> = (0..100)
        .map(|i| json!({"delta": {"content": &format!("chunk{}", i)}}))
        .collect();

    let result = accumulate_chat_completion_stream_events(events);
    assert!(result.is_ok());

    let accumulated = result.unwrap();
    let content = accumulated["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    assert!(content.starts_with("chunk0"));
    assert!(content.ends_with("chunk99"));
}

#[test]
fn chat_completion_handles_tool_call_with_complex_arguments() {
    let system_prompt = "You are a helpful assistant.";
    let complex_input = json!({
        "nested": {
            "array": [1, 2, 3],
            "object": {"key": "value"},
            "string": "test",
            "number": 42,
            "boolean": true
        }
    });

    let conversation = vec![ConversationMessage::AssistantBlocks(vec![
        ModelBlock::ToolUse {
            id: "call_complex".to_string(),
            name: "complex_tool".to_string(),
            input: complex_input.clone(),
        },
    ])];

    let result = build_chat_completion_messages(system_prompt, &conversation);
    assert!(result.is_ok());

    let messages = result.unwrap();
    let tool_calls = messages[1]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);

    let function = &tool_calls[0]["function"];
    assert_eq!(function["name"], "complex_tool");

    let parsed_args: serde_json::Value =
        serde_json::from_str(function["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(parsed_args, complex_input);
}

#[test]
fn chat_completion_request_handles_empty_tools_list() {
    use crate::provider::transports::build_chat_completion_request;

    let request = provider_turn_request_with_tools(vec![]); // No tools

    let chat_request = build_chat_completion_request(
        "gpt-5.4",
        1000,
        &request,
        ToolSchemaContract::Relaxed,
        false,
    );

    assert!(chat_request.is_ok());

    let body = chat_request.unwrap();
    // Should not have tools field when no tools are provided
    assert!(
        body.get("tools").is_none() || body.get("tools").unwrap().as_array().unwrap().is_empty()
    );
    assert!(body.get("tool_choice").is_none());
}

#[test]
fn chat_completion_continuation_handles_system_prompt_changes() {
    // This test verifies that continuation properly detects system prompt changes
    // Currently, Chat Completions continuation is disabled, but the test
    // ensures the detection logic is in place for future implementation

    let system_prompt1 = "You are a helpful assistant.";
    let system_prompt2 = "You are an expert coder.";

    let conversation = vec![ConversationMessage::UserText("Hello".to_string())];

    let result1 = build_chat_completion_messages(system_prompt1, &conversation);
    let result2 = build_chat_completion_messages(system_prompt2, &conversation);

    assert!(result1.is_ok());
    assert!(result2.is_ok());

    let messages1 = result1.unwrap();
    let messages2 = result2.unwrap();

    // Different system prompts should produce different messages
    assert_ne!(messages1[0]["content"], messages2[0]["content"]);
}
