use super::super::*;

pub(crate) fn parse_chat_completion_response(response: Value) -> Result<ParsedOpenAiResponse> {
    // Extract response ID
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    // Extract choices array
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI Chat Completions response did not contain choices array",
                "missing choices",
            )
        })?;

    let first_choice = choices.first().ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choices array was empty",
            "empty choices",
        )
    })?;

    // Extract message from first choice
    let message = first_choice.get("message").ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choice did not contain message",
            "missing message",
        )
    })?;

    // Parse message content
    let mut blocks = Vec::new();

    // Extract text content
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            blocks.push(ModelBlock::Text {
                text: content.to_string(),
            });
        }
    }

    // Extract tool calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let id = tool_call.get("id").and_then(Value::as_str).ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain id",
                    "missing tool_call_id",
                )
            })?;

            let function = tool_call.get("function").ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain function",
                    "missing function",
                )
            })?;

            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_response_error(
                        "OpenAI Chat Completions function did not contain name",
                        "missing function_name",
                    )
                })?;

            let arguments_str = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");

            let arguments = if arguments_str.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments_str).map_err(|error| {
                    invalid_response_error("invalid tool call arguments JSON", error)
                })?
            };

            blocks.push(ModelBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: arguments,
                kind: ModelToolCallKind::Function,
            });
        }
    }

    // Allow valid minimal assistant messages that contain neither text nor tool calls.
    // OpenAI Chat Completions can return empty/null content together with a finish_reason.
    // In such cases, we return an empty blocks vector rather than an error.
    if blocks.is_empty() {
        // Check if we have a valid finish_reason before accepting empty blocks
        let finish_reason = first_choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);

        if finish_reason.is_some() {
            // Accept empty response when we have a finish_reason
        } else {
            return Err(invalid_response_error(
                "OpenAI Chat Completions response contained no supported content",
                "empty content",
            ));
        }
    }

    // Extract usage
    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        creation_input_tokens: 0,
    });

    // Extract finish reason
    let stop_reason = first_choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Build output items for continuation tracking
    // Store the complete message object for proper continuation support
    let output_items = vec![message.clone()];

    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason,
            input_tokens: usage
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            provider_message_id: response_id.clone(),
            provider_request_id: None,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn accumulate_chat_completion_stream_events(events: Vec<Value>) -> Result<Value> {
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

#[cfg(test)]
#[allow(dead_code)]
pub(super) enum ChatCompletionSseEvent {
    ContentDelta(String),
    ToolCallDelta(Value),
    Done,
}
