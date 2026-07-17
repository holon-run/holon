use super::super::continuation::{
    continuation_scope, incremental_diagnostics, lock_openai_continuation, request_shape_hash,
    response_format_diagnostics,
};
use super::super::*;

pub(crate) fn build_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
) -> Result<Value> {
    // Build messages array for Chat Completions API
    let messages =
        build_chat_completion_messages(&request.prompt_frame.system_prompt, &request.conversation)?;

    // Build tools array in OpenAI function calling format
    let tools = if !request.tools.is_empty() {
        Some(
            request
                .tools
                .iter()
                .map(|tool| {
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                            "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };

    // Build Chat Completions request body
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "stream": stream,
    });

    // Add tools if present
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String("auto".to_string());
    }

    // Add prompt cache key if available
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }

    Ok(body)
}

pub(crate) fn plan_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> Result<(Value, OpenAiRequestPlan)> {
    let full_body = build_chat_completion_request(
        model,
        max_output_tokens,
        request,
        tool_schema_contract,
        stream,
    )?;

    let body_messages = full_body
        .get("messages")
        .and_then(|messages| messages.as_array())
        .cloned()
        .unwrap_or_default();

    // Calculate continuation scope
    let scope = continuation_scope(request);
    let full_messages = body_messages;
    let full_message_count = full_messages.len();
    let request_shape = request_shape_for_chat_completion(&full_body, request);

    // Check for continuation opportunity
    let Some(scope_ref) = scope.as_ref() else {
        // No continuation scope - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                replay_loss_reason: None,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "missing_continuation_scope",
                    None,
                    full_message_count,
                    None,
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    };

    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();

    let Some(previous) = previous else {
        // No previous state - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                replay_loss_reason: None,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "not_applicable_initial_request",
                    None,
                    full_message_count,
                    None,
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    };

    // Check if request shape changed
    if previous.request_shape != request_shape {
        // Request changed - send full request
        let request_shape_hash = request_shape_hash(&request_shape);
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                replay_loss_reason: previous.replay_loss_reason,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "request_shape_changed",
                    None,
                    full_message_count,
                    Some(OpenAiContinuationMismatchDiagnostics {
                        request_shape_hash: Some(request_shape_hash),
                        ..OpenAiContinuationMismatchDiagnostics::default()
                    }),
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    }

    // Chat Completions continuation currently cannot safely reconstruct an
    // assistant message from the provider window for prefix matching.
    // `full_messages` contains message objects, but `response_output` is not
    // guaranteed to store a comparable message value, so incremental
    // continuation would be unreliable here. Send the full request instead.
    let request_shape_hash = request_shape_hash(&request_shape);
    return Ok((
        full_body.clone(),
        OpenAiRequestPlan {
            body: full_body,
            fallback_replay: None,
            scope,
            append_match_input: full_messages.clone(),
            provider_input: full_messages,
            replay_loss_reason: previous.replay_loss_reason,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "chat_completions_incremental_continuation_unsupported",
                None,
                full_message_count,
                Some(OpenAiContinuationMismatchDiagnostics {
                    request_shape_hash: Some(request_shape_hash),
                    ..OpenAiContinuationMismatchDiagnostics::default()
                }),
                None,
                None,
                response_format_diagnostics(false, request),
            ),
        },
    ));
}

fn request_shape_for_chat_completion(
    body: &Value,
    request: &ProviderTurnRequest,
) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("messages");
        object.remove("prompt_cache_key");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

pub(crate) fn build_chat_completion_messages(
    system_prompt: &str,
    conversation: &[ConversationMessage],
) -> Result<Vec<Value>> {
    let mut messages = Vec::new();

    // Add system prompt as first message
    if !system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
    }

    // Process conversation messages
    for msg in conversation {
        match msg {
            ConversationMessage::UserText(text) => {
                messages.push(json!({
                    "role": "user",
                    "content": text,
                }));
            }
            ConversationMessage::UserBlocks(blocks) => {
                // Concatenate all block texts
                let content = blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            ConversationMessage::UserImage {
                prompt,
                media_type,
                data_base64,
            } => {
                messages.push(json!({
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{media_type};base64,{data_base64}"),
                            },
                        },
                    ],
                }));
            }
            ConversationMessage::AssistantBlocks(blocks) => {
                // Extract text content and tool calls
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ModelBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input)
                                        .context("failed to serialize tool call arguments")?,
                                }
                            }));
                        }
                        ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
                    }
                }

                // Build assistant message
                let content = text_parts.join("\n\n");
                let mut message = json!({
                    "role": "assistant",
                });

                // Set content field: null for tool-only messages, otherwise string content
                if !text_parts.is_empty() {
                    message["content"] = Value::String(content);
                } else {
                    message["content"] = Value::Null;
                }

                // Add tool_calls if present
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(tool_calls);
                }

                messages.push(message);
            }
            ConversationMessage::UserToolResults(results) => {
                // Each tool result becomes a separate "tool" message
                for result in results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_use_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    Ok(messages)
}
