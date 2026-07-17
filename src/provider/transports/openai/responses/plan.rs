use super::super::continuation::*;
use super::super::*;
use super::continuation::*;

pub(crate) fn build_openai_responses_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    contract: OpenAiResponsesTransportContract,
    tool_schema_contract: ToolSchemaContract,
    reasoning_effort: Option<&str>,
    verbosity: Option<ModelVerbosity>,
) -> Result<Value> {
    let mut tools = request
        .tools
        .iter()
        .map(|tool| {
            if let Some(grammar) = tool.freeform_grammar.as_ref() {
                Ok(json!({
                    "type": "custom",
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "syntax": grammar.syntax,
                        "definition": grammar.definition,
                    }
                }))
            } else {
                Ok(json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                    "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                }))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(tool) = openai_native_web_search_tool(request) {
        tools.push(tool);
    }

    let mut body = json!({
        "model": model,
        "instructions": request.prompt_frame.system_prompt,
        "input": build_openai_input(&request.conversation)?,
        "store": false,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String("auto".to_string());
        body["parallel_tool_calls"] = Value::Bool(false);
    }
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }
    if let Some(response_format) = openai_response_format(request) {
        body["text"]["format"] = response_format;
    }
    if let Some(reasoning_effort) = reasoning_effort {
        body["reasoning"] = json!({ "effort": reasoning_effort });
    }
    match contract {
        OpenAiResponsesTransportContract::StandardJson => {
            body["max_output_tokens"] = Value::from(max_output_tokens);
        }
        OpenAiResponsesTransportContract::CodexStreaming => {
            body["stream"] = Value::Bool(true);
            if let Some(verbosity) = verbosity {
                body["text"] = json!({ "verbosity": verbosity.as_str() });
            }
            if reasoning_effort.is_some() {
                body["include"] = json!(["reasoning.encrypted_content"]);
            } else {
                body["reasoning"] = Value::Null;
                body["include"] = Value::Array(Vec::new());
            }
        }
    }
    Ok(body)
}

fn openai_response_format(request: &ProviderTurnRequest) -> Option<Value> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => Some(json!({
            "type": "json_schema",
            "name": format.name,
            "schema": format.schema,
            "strict": format.strict,
        })),
    }
}

fn openai_native_web_search_tool(request: &ProviderTurnRequest) -> Option<Value> {
    let native = request.native_web_search.as_ref()?;
    match native.kind {
        ProviderNativeWebSearchKind::OpenAi => Some(json!({ "type": native.advertised_tool_type })),
        ProviderNativeWebSearchKind::Xai => Some(json!({ "type": native.advertised_tool_type })),
        _ => None,
    }
}

fn openai_request_controls_diagnostics(body: &Value) -> ProviderOpenAiRequestControlsDiagnostics {
    let reasoning_effort = body
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let verbosity = body
        .get("text")
        .and_then(|text| text.get("verbosity"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let include_reasoning_encrypted_content = body
        .get("include")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        });
    let max_output_tokens_sent = body.get("max_output_tokens").is_some();
    let codex_streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    ProviderOpenAiRequestControlsDiagnostics {
        reasoning_sent: reasoning_effort.is_some(),
        reasoning_effort,
        verbosity,
        include_reasoning_encrypted_content,
        max_output_tokens_sent,
        max_output_tokens_unsupported: codex_streaming,
    }
}

pub(in super::super) fn plan_openai_responses_request(
    mut body: Value,
    request: &ProviderTurnRequest,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    allow_previous_response_id: bool,
    continuation_contract: OpenAiResponsesContinuationContract,
) -> Result<OpenAiRequestPlan> {
    if continuation_contract
        == OpenAiResponsesContinuationContract::StoreResponsesAndOmitInstructionsWithPreviousResponseId
    {
        body["store"] = Value::Bool(true);
    }
    let full_input = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI request did not contain input array",
                "missing input",
            )
        })?;
    let full_input_items = full_input.len();
    let append_match_input = openai_append_match_input_items(&full_input);
    let request_shape = request_shape_without_input(&body, request);
    let scope = continuation_scope(request);
    let request_controls = Some(openai_request_controls_diagnostics(&body));
    let Some(scope_ref) = scope.as_ref() else {
        return Ok(OpenAiRequestPlan {
            body,
            fallback_replay: None,
            scope,
            append_match_input,
            provider_input: full_input,
            replay_loss_reason: None,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "missing_continuation_scope",
                None,
                full_input_items,
                None,
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    };
    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();
    let Some(previous) = previous else {
        return Ok(OpenAiRequestPlan {
            body,
            fallback_replay: None,
            scope,
            append_match_input,
            provider_input: full_input,
            replay_loss_reason: None,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "not_applicable_initial_request",
                None,
                full_input_items,
                None,
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    };

    if previous.request_shape != request_shape {
        let request_shape_hash = request_shape_hash(&request_shape);
        let mut diagnostics = incremental_diagnostics(
            "full_request",
            "request_shape_changed",
            None,
            full_input_items,
            Some(OpenAiContinuationMismatchDiagnostics {
                request_shape_hash: Some(request_shape_hash),
                ..OpenAiContinuationMismatchDiagnostics::default()
            }),
            request_controls,
            native_web_search_diagnostics(request),
            response_format_diagnostics(true, request),
        );
        if previous.replay_loss_reason.is_some() {
            diagnostics
                .incremental_continuation
                .as_mut()
                .expect("incremental diagnostics should include continuation details")
                .server_side_context_may_be_lost = Some(true);
        }
        return Ok(OpenAiRequestPlan {
            body,
            fallback_replay: None,
            scope,
            append_match_input,
            provider_input: full_input,
            replay_loss_reason: previous.replay_loss_reason,
            request_shape,
            diagnostics,
        });
    }

    let expected_prefix = previous.append_match_items.clone();
    let mismatch = openai_continuation_mismatch_diagnostics(
        &expected_prefix,
        &append_match_input,
        &request_shape,
    );
    if expected_prefix.is_empty()
        || append_match_input.len() <= expected_prefix.len()
        || !append_match_input.starts_with(&expected_prefix)
    {
        let mut diagnostics = incremental_diagnostics(
            "full_request",
            "conversation_not_strict_append_only",
            None,
            full_input_items,
            Some(mismatch),
            request_controls,
            native_web_search_diagnostics(request),
            response_format_diagnostics(true, request),
        );
        if previous.replay_loss_reason.is_some() {
            diagnostics
                .incremental_continuation
                .as_mut()
                .expect("incremental diagnostics should include continuation details")
                .server_side_context_may_be_lost = Some(true);
        }
        return Ok(OpenAiRequestPlan {
            body,
            fallback_replay: None,
            scope,
            append_match_input,
            provider_input: full_input,
            replay_loss_reason: previous.replay_loss_reason,
            request_shape,
            diagnostics,
        });
    }

    let incremental_input = full_input[expected_prefix.len()..].to_vec();
    let response_id = allow_previous_response_id
        .then(|| previous.response_id.clone())
        .flatten();
    let has_response_id = response_id.is_some();
    let replay_is_compacted = previous.latest_compaction_index.is_some();
    let mut fallback_replay = None;
    let provider_input = if let Some(response_id) = response_id {
        let mut replay_input = previous.items.clone();
        replay_input.extend(incremental_input.clone());
        if previous.replay_loss_reason.is_none() {
            let mut replay_body = body.clone();
            replay_body["input"] = Value::Array(replay_input.clone());
            replay_body
                .as_object_mut()
                .expect("OpenAI Responses request body should be an object")
                .remove("previous_response_id");
            fallback_replay = Some((replay_body, replay_input));
        }
        body["input"] = Value::Array(incremental_input.clone());
        body["previous_response_id"] = Value::String(response_id);
        if continuation_contract
            == OpenAiResponsesContinuationContract::StoreResponsesAndOmitInstructionsWithPreviousResponseId
        {
            body.as_object_mut()
                .expect("OpenAI Responses request body should be an object")
                .remove("instructions");
        }
        incremental_input.clone()
    } else {
        let mut provider_input = previous.items.clone();
        provider_input.extend(incremental_input.clone());
        body["input"] = Value::Array(provider_input.clone());
        provider_input
    };
    let request_shape_hash = request_shape_hash(&request_shape);
    Ok(OpenAiRequestPlan {
        body,
        fallback_replay,
        scope,
        append_match_input,
        provider_input,
        replay_loss_reason: previous.replay_loss_reason.clone(),
        request_shape,
        diagnostics: ProviderRequestDiagnostics {
            request_lowering_mode: openai_append_match_lowering_mode(
                has_response_id,
                replay_is_compacted,
                continuation_contract,
            ),
            anthropic_cache: None,
            anthropic_context_management: None,
            openai_request_controls: request_controls,
            openai_remote_compaction: None,
            incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
                status: "hit".into(),
                fallback_reason: None,
                server_side_context_may_be_lost: (!has_response_id
                    && previous.replay_loss_reason.is_some())
                .then_some(true),
                incremental_input_items: Some(incremental_input.len()),
                full_input_items: Some(full_input_items),
                expected_prefix_items: Some(expected_prefix.len()),
                first_mismatch_index: None,
                previous_item_type: None,
                current_item_type: None,
                previous_item_id: None,
                current_item_id: None,
                previous_item_hash: None,
                current_item_hash: None,
                request_shape_hash: Some(request_shape_hash),
                first_mismatch_path: None,
                mismatch_kind: None,
            }),
            native_web_search: native_web_search_diagnostics(request),
            response_format: response_format_diagnostics(true, request),
        },
    })
}

fn openai_append_match_lowering_mode(
    has_response_id: bool,
    replay_is_compacted: bool,
    continuation_contract: OpenAiResponsesContinuationContract,
) -> String {
    if has_response_id {
        if continuation_contract
            == OpenAiResponsesContinuationContract::StoreResponsesAndOmitInstructionsWithPreviousResponseId
        {
            "incremental_continuation_omit_instructions".into()
        } else {
            "incremental_continuation".into()
        }
    } else if replay_is_compacted {
        "provider_window_compacted".into()
    } else {
        "provider_window_replay".into()
    }
}

pub(crate) fn build_openai_input(conversation: &[ConversationMessage]) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut tool_call_kinds = HashMap::<String, ModelToolCallKind>::new();
    for message in conversation {
        match message {
            ConversationMessage::UserText(text) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            ConversationMessage::UserBlocks(blocks) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": blocks.iter().map(|block| json!({
                    "type": "input_text",
                    "text": block.text,
                })).collect::<Vec<_>>(),
            })),
            ConversationMessage::UserImage {
                prompt,
                media_type,
                data_base64,
            } => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": prompt },
                    {
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data_base64}"),
                    },
                ],
            })),
            ConversationMessage::AssistantBlocks(blocks) => {
                let mut pending_text = Vec::new();
                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => pending_text.push(text.clone()),
                        ModelBlock::ToolUse {
                            id,
                            name,
                            input,
                            kind,
                        } => {
                            flush_assistant_text(&mut items, &mut pending_text);
                            tool_call_kinds.insert(id.clone(), *kind);
                            match kind {
                                ModelToolCallKind::Function => items.push(json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": canonical_json(
                                        &normalize_openai_function_arguments(Some(input))
                                    ),
                                })),
                                ModelToolCallKind::Custom => items.push(json!({
                                    "type": "custom_tool_call",
                                    "call_id": id,
                                    "name": name,
                                    "input": openai_custom_tool_input(input)?,
                                })),
                            }
                        }
                        ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
                    }
                }
                flush_assistant_text(&mut items, &mut pending_text);
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    let item_type = match tool_call_kinds.get(&result.tool_use_id) {
                        Some(ModelToolCallKind::Custom) => "custom_tool_call_output",
                        Some(ModelToolCallKind::Function) | None => "function_call_output",
                    };
                    items.push(json!({
                        "type": item_type,
                        "call_id": result.tool_use_id,
                        "output": result.content,
                    }));
                }
            }
        }
    }
    Ok(items)
}

fn openai_custom_tool_input(input: &Value) -> Result<String> {
    match input {
        Value::String(value) => Ok(value.clone()),
        Value::Object(map) => map
            .get("patch")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("custom tool call input must contain string field `patch`")
            }),
        _ => anyhow::bail!(
            "custom tool call input must be a string or an object containing string field `patch`"
        ),
    }
}

fn flush_assistant_text(items: &mut Vec<Value>, pending_text: &mut Vec<String>) {
    if pending_text.is_empty() {
        return;
    }
    let content = pending_text
        .drain(..)
        .map(|text| json!({ "type": "output_text", "text": text }))
        .collect::<Vec<_>>();
    items.push(json!({
        "type": "message",
        "role": "assistant",
        "content": content,
    }));
}
