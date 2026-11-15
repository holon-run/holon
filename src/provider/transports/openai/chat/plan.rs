#[cfg(test)]
use holon_model_client::chat_completions::RequestBody;
use holon_model_client::{chat_completions, Role};

use super::super::continuation::{
    continuation_scope, incremental_diagnostics, lock_openai_continuation, request_shape_hash,
    response_format_diagnostics,
};
use super::super::*;

#[cfg(test)]
pub(crate) fn build_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
    reasoning_effort: Option<&str>,
    max_output_tokens_parameter: &str,
) -> Result<Value> {
    let (messages, tools) = build_chat_completion_parts(request, tool_schema_contract)?;
    let tool_choice = (!tools.is_empty()).then_some("auto");
    Ok(Value::Object(
        RequestBody {
            model,
            messages,
            output_limit: Some((max_output_tokens_parameter, max_output_tokens)),
            stream,
            tools,
            tool_choice,
            prompt_cache_key: request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.prompt_cache_key.as_str()),
            reasoning_effort,
        }
        .encode(),
    ))
}

pub(crate) fn build_chat_completion_parts(
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let messages =
        build_chat_completion_messages(&request.prompt_frame.system_prompt, &request.conversation)?;
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            Ok(chat_completions::function_tool(
                &tool.name,
                Some(&tool.description),
                &emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                matches!(tool_schema_contract, ToolSchemaContract::Strict),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((messages, tools))
}

pub(crate) fn plan_chat_completion_request(
    full_body: Value,
    request: &ProviderTurnRequest,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> Result<(Value, OpenAiRequestPlan)> {
    let body_messages = full_body
        .get("messages")
        .and_then(|messages| messages.as_array())
        .map(|items| request.material.cloned(items))
        .transpose()?
        .unwrap_or_default();

    // Calculate continuation scope
    let scope = continuation_scope(request);
    let full_messages = body_messages;
    let full_message_count = full_messages.len();
    super::super::continuation::admit_openai_request_shape(
        &full_body,
        request,
        &["messages", "prompt_cache_key"],
    )?;
    let request_shape = request_shape_for_chat_completion(&full_body, request);
    request.material.admit_value(&request_shape)?;

    // Check for continuation opportunity
    let Some(scope_ref) = scope.as_ref() else {
        // No continuation scope - send full request
        return Ok((
            request.material.cloned(&full_body)?,
            OpenAiRequestPlan {
                material: request.material.clone(),
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: request.material.cloned(&full_messages)?,
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
            request.material.cloned(&full_body)?,
            OpenAiRequestPlan {
                material: request.material.clone(),
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: request.material.cloned(&full_messages)?,
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
            request.material.cloned(&full_body)?,
            OpenAiRequestPlan {
                material: request.material.clone(),
                body: full_body,
                fallback_replay: None,
                scope,
                append_match_input: request.material.cloned(&full_messages)?,
                provider_input: full_messages,
                replay_loss_reason: request.material.cloned(&previous.replay_loss_reason)?,
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
        request.material.cloned(&full_body)?,
        OpenAiRequestPlan {
            material: request.material.clone(),
            body: full_body,
            fallback_replay: None,
            scope,
            append_match_input: request.material.cloned(&full_messages)?,
            provider_input: full_messages,
            replay_loss_reason: request.material.cloned(&previous.replay_loss_reason)?,
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
    let wire_shape = body.as_object().map_or_else(
        || body.clone(),
        |object| {
            Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| !matches!(key.as_str(), "messages" | "prompt_cache_key"))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        },
    );
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

pub(crate) fn build_chat_completion_messages(
    system_prompt: &str,
    conversation: &[ConversationMessage],
) -> Result<Vec<Value>> {
    use chat_completions::message;

    let mut messages = Vec::new();
    if !system_prompt.is_empty() {
        messages.push(message(
            Role::System,
            Value::from(system_prompt),
            Vec::new(),
            None,
        ));
    }

    for msg in conversation {
        match msg {
            ConversationMessage::UserText(text) => {
                messages.push(message(
                    Role::User,
                    Value::from(text.as_str()),
                    Vec::new(),
                    None,
                ));
            }
            ConversationMessage::UserBlocks(blocks) => {
                let content = blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.push(message(
                    Role::User,
                    Value::String(content),
                    Vec::new(),
                    None,
                ));
            }
            ConversationMessage::UserImage {
                prompt,
                media_type,
                data_base64,
                ..
            } => {
                let content = json!([
                    { "type": "text", "text": prompt },
                    { "type": "image_url", "image_url": { "url": data_base64.data_url(media_type)? } },
                ]);
                messages.push(message(Role::User, content, Vec::new(), None));
            }
            ConversationMessage::AssistantBlocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => text_parts.push(text.as_str()),
                        ModelBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            tool_calls.push(
                                chat_completions::function_call(id, name, input)
                                    .context("failed to serialize tool call arguments")?,
                            );
                        }
                        ModelBlock::Thinking { .. }
                        | ModelBlock::ReasoningText { .. }
                        | ModelBlock::RedactedThinking { .. }
                        | ModelBlock::Citations { .. }
                        | ModelBlock::ProviderToolUse { .. }
                        | ModelBlock::ProviderToolResult { .. } => {}
                    }
                }
                let content = if text_parts.is_empty() {
                    Value::Null
                } else {
                    Value::String(text_parts.join("\n\n"))
                };
                messages.push(message(Role::Assistant, content, tool_calls, None));
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    messages.push(message(
                        Role::Tool,
                        Value::from(result.content.as_str()),
                        Vec::new(),
                        Some(&result.tool_use_id),
                    ));
                }
            }
        }
    }

    Ok(messages)
}
