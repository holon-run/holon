use super::super::*;
use crate::openai_chat::{decode_response, ChatResponse, ChatResponseError};
use crate::provider::{set_provider_transport_usage_evidence, AttemptTokenUsage};
use holon_model_client::ContentPart;

pub fn parse_chat_completion_response(response: Value) -> Result<ParsedOpenAiResponse> {
    // `reasoning_tokens` lives in the raw usage block; the decoded `Usage`
    // struct does not carry it. Capture it before `decode_response` consumes
    // the payload.
    let reasoning_tokens = response
        .get("usage")
        .and_then(|usage| usage.get("completion_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    let ChatResponse {
        completion: parsed,
        raw_message,
        served_service_tier,
    } = decode_response(response).map_err(|error| match error {
        ChatResponseError::MissingMessage => invalid_response_error(
            "OpenAI Chat Completions choice did not contain message",
            "missing message",
        ),
        ChatResponseError::InvalidResponse(error) => invalid_response_error(
            "OpenAI Chat Completions response could not be parsed",
            error.to_string(),
        ),
    })?;
    let response_id = parsed.id.clone();
    let mut blocks = Vec::new();

    for content in parsed.message.content {
        if let ContentPart::Text { text } = content {
            if !text.is_empty() {
                blocks.push(ModelBlock::Text { text });
            }
        }
    }

    for tool_call in parsed.tool_calls {
        blocks.push(ModelBlock::ToolUse {
            id: tool_call.id,
            name: tool_call.name,
            input: tool_call.arguments,
            kind: ModelToolCallKind::Function,
            provider_data: None,
        });
    }

    let usage = parsed.usage.as_ref();
    let input_tokens = usage.map(|usage| usage.input_tokens).unwrap_or(0);
    let output_tokens = usage.map(|usage| usage.output_tokens).unwrap_or(0);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage.cached_input_tokens.unwrap_or(0),
        creation_input_tokens: 0,
    });
    // OpenAI Chat Completions `input_tokens` includes cache hits, so normalize
    // as inclusive input. `output_tokens` already includes reasoning tokens.
    let normalized_token_usage = usage.map(|usage| {
        AttemptTokenUsage::from_inclusive_input(
            usage.input_tokens,
            usage.cached_input_tokens.unwrap_or(0),
            0,
            usage.output_tokens,
            reasoning_tokens,
        )
    });

    // #4295: a completed turn with no usable text or tool calls must classify
    // as a retryable empty provider response instead of settling an empty
    // turn. Length truncation keeps its existing passthrough behavior.
    if parsed.finish_reason.as_deref() != Some("length")
        && !openai_blocks_have_countable_content(&blocks)
    {
        return Err(set_provider_transport_usage_evidence(
            empty_response_error(
                "empty OpenAI Chat Completions response",
                "response contained no usable text or tool calls",
                crate::types::TokenUsage::new(input_tokens, output_tokens),
            ),
            cache_usage,
            normalized_token_usage,
        ));
    }

    Ok(ParsedOpenAiResponse {
        served_service_tier,
        response: ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: parsed.finish_reason,
            input_tokens,
            output_tokens,
            cache_usage,
            normalized_token_usage,
            provider_message_id: response_id.clone(),
            provider_request_id: None,
            request_diagnostics: None,
        },
        response_id,
        output_items: vec![raw_message],
        transport_timeline: None,
    })
}

#[cfg(any(test, feature = "runtime-tests"))]
#[allow(dead_code)]

pub(super) fn process_chat_completion_sse_event(
    data_lines: &mut Vec<String>,
) -> Result<Option<ChatCompletionSseEvent>> {
    if data_lines.is_empty() {
        return Ok(None);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed == "[DONE]" {
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    let event: Value = serde_json::from_str(trimmed).map_err(|error| {
        invalid_response_error("invalid Chat Completions streaming JSON", error)
    })?;

    // Check for errors
    if event.get("error").is_some() {
        return Err(invalid_response_error(
            "Chat Completions streaming contained error event",
            "error_in_stream",
        ));
    }

    // Process delta content from choices[0].delta (OpenAI Chat Completions streaming format)
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if let Some(delta) = first_choice.get("delta") {
                // Process content delta
                if let Some(content) = delta.get("content") {
                    if let Some(text) = content.as_str() {
                        return Ok(Some(ChatCompletionSseEvent::ContentDelta(text.to_string())));
                    }
                }

                // Process tool_calls delta - return the entire array for accumulation
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    return Ok(Some(ChatCompletionSseEvent::ToolCallDelta(json!({
                        "tool_calls": tool_calls
                    }))));
                }
            }
        }
    }

    // Check for finish_reason at both top-level and choices[0] level
    if event.get("finish_reason").is_some() {
        // Stream ending event
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    // Also check for finish_reason in choices[0]
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if first_choice.get("finish_reason").is_some() {
                // Stream ending event
                return Ok(Some(ChatCompletionSseEvent::Done));
            }
        }
    }

    Ok(None)
}

#[cfg(any(test, feature = "runtime-tests"))]
pub fn accumulate_chat_completion_stream_events(events: Vec<Value>) -> Result<Value> {
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = None;

    for event in events {
        // Handle both formats: direct delta and nested choices[0].delta
        let delta = if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            choices.first().and_then(|c| c.get("delta"))
        } else {
            event.get("delta")
        };

        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str) {
            content.push_str(text);
        }

        if let Some(tool_calls_delta) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tool_call_delta in tool_calls_delta {
                let index = tool_call_delta["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(json!({}));
                }

                let tool_call = &mut tool_calls[index];

                if let Some(id) = tool_call_delta.get("id").and_then(Value::as_str) {
                    tool_call["id"] = Value::String(id.to_string());
                }
                if let Some(name) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    tool_call["function"]["name"] = Value::String(name.to_string());
                }
                if let Some(arguments) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                {
                    let current_args = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");

                    let new_args = if let Some(additional) = arguments.as_str() {
                        // If we have existing content, concatenate; otherwise use new content
                        if current_args.is_empty() {
                            additional.to_string()
                        } else {
                            format!("{}{}", current_args, additional)
                        }
                    } else {
                        current_args.to_string()
                    };

                    // Only set if we have arguments
                    if !new_args.is_empty() {
                        tool_call["function"]["arguments"] = Value::String(new_args);
                    }
                }
            }
        }

        // Handle finish_reason in both formats: direct and nested in choices[0]
        if let Some(reason) = event.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(reason.to_string());
        } else if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            if let Some(first_choice) = choices.first() {
                if let Some(reason) = first_choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = Some(reason.to_string());
                }
            }
        }
    }

    // Build accumulated response
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(json!({
        "id": "chatcmpl-stream",
        "choices": [{
            "message": message,
            "finish_reason": finish_reason.unwrap_or("stop".to_string())
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    }))
}

#[cfg(any(test, feature = "runtime-tests"))]
#[allow(dead_code)]
pub(super) enum ChatCompletionSseEvent {
    ContentDelta(String),
    ToolCallDelta(Value),
    Done,
}
