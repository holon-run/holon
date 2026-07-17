use super::super::continuation::*;
use super::super::*;

pub(in super::super) fn request_shape_without_input(
    body: &Value,
    request: &ProviderTurnRequest,
) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("input");
        object.remove("previous_response_id");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

pub(in super::super) fn openai_continuation_mismatch_diagnostics(
    expected_prefix: &[Value],
    full_input: &[Value],
    request_shape: &OpenAiRequestShape,
) -> OpenAiContinuationMismatchDiagnostics {
    let first_mismatch_index = expected_prefix
        .iter()
        .zip(full_input.iter())
        .position(|(previous, current)| previous != current)
        .or_else(|| {
            (expected_prefix.len() != full_input.len())
                .then_some(expected_prefix.len().min(full_input.len()))
        });
    let previous = first_mismatch_index.and_then(|index| expected_prefix.get(index));
    let current = first_mismatch_index.and_then(|index| full_input.get(index));
    let item_path = match (first_mismatch_index, previous, current) {
        (Some(index), Some(previous), Some(current)) => {
            let suffix = first_json_mismatch_path(previous, current).unwrap_or_default();
            Some(format!("/{index}{suffix}"))
        }
        (Some(index), _, _) => Some(format!("/{index}")),
        _ => None,
    };
    OpenAiContinuationMismatchDiagnostics {
        expected_prefix_items: expected_prefix.len(),
        first_mismatch_index,
        previous_item_type: previous.map(openai_item_type),
        current_item_type: current.map(openai_item_type),
        previous_item_id: previous.and_then(openai_item_stable_id),
        current_item_id: current.and_then(openai_item_stable_id),
        previous_item_hash: previous.map(openai_item_hash),
        current_item_hash: current.map(openai_item_hash),
        request_shape_hash: Some(request_shape_hash(request_shape)),
        first_mismatch_path: item_path.clone(),
        mismatch_kind: Some(openai_mismatch_kind(
            previous,
            current,
            item_path.as_deref(),
        )),
    }
}

pub(in super::super) fn first_json_mismatch_path(
    previous: &Value,
    current: &Value,
) -> Option<String> {
    if previous == current {
        return None;
    }
    match (previous, current) {
        (Value::Array(previous), Value::Array(current)) => {
            let shared = previous.len().min(current.len());
            for index in 0..shared {
                if let Some(path) = first_json_mismatch_path(&previous[index], &current[index]) {
                    return Some(format!("/{index}{path}"));
                }
            }
            Some(format!("/{shared}"))
        }
        (Value::Object(previous), Value::Object(current)) => {
            let keys = previous
                .keys()
                .chain(current.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                match (previous.get(key), current.get(key)) {
                    (Some(previous), Some(current)) => {
                        if let Some(path) = first_json_mismatch_path(previous, current) {
                            return Some(format!("/{}{}", json_pointer_escape(key), path));
                        }
                    }
                    _ => return Some(format!("/{}", json_pointer_escape(key))),
                }
            }
            Some(String::new())
        }
        _ => Some(String::new()),
    }
}

pub(in super::super) fn json_pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

pub(in super::super) fn openai_mismatch_kind(
    previous: Option<&Value>,
    current: Option<&Value>,
    path: Option<&str>,
) -> String {
    let Some(previous) = previous else {
        return "length_mismatch".into();
    };
    let Some(current) = current else {
        return "length_mismatch".into();
    };
    let previous_type = openai_item_type(previous);
    let current_type = openai_item_type(current);
    if previous_type != current_type {
        return "semantic_mismatch".into();
    }
    let path = path.unwrap_or_default();
    if path.contains("/id")
        || path.contains("/status")
        || path.contains("/metadata")
        || path.contains("/annotations")
        || path.contains("/logprobs")
    {
        return "provider_metadata_only".into();
    }
    match previous_type.as_str() {
        "message" => {
            if previous.get("role").and_then(Value::as_str) == Some("assistant")
                && path.contains("/content")
                && !path.ends_with("/text")
            {
                "assistant_text_shape".into()
            } else {
                "semantic_mismatch".into()
            }
        }
        "function_call" | "custom_tool_call" => "tool_call_shape".into(),
        "function_call_output" | "custom_tool_call_output" => "tool_result_shape".into(),
        _ => "semantic_mismatch".into(),
    }
}

pub(in super::super) fn openai_item_type(item: &Value) -> String {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| match item {
            Value::Object(_) => "object",
            Value::Array(_) => "array",
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        })
        .to_string()
}

pub(in super::super) fn openai_item_stable_id(item: &Value) -> Option<String> {
    if let Some(id) = ["id", "call_id"]
        .into_iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
    {
        return Some(id.to_string());
    }
    item.get("role").and_then(Value::as_str).map(|role| {
        let item_type = openai_item_type(item);
        format!("{item_type}:{role}")
    })
}

pub(in super::super) fn openai_item_hash(item: &Value) -> String {
    sha256_hex(canonical_json(item).as_bytes())
}

pub(in super::super) fn latest_openai_compaction_index(items: &[Value]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| openai_is_compaction_item(item).then_some(index))
}

pub(in super::super) fn openai_is_compaction_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}

pub(in super::super) fn native_web_search_diagnostics(
    request: &ProviderTurnRequest,
) -> Option<ProviderNativeWebSearchDiagnostics> {
    let native = request.native_web_search.as_ref()?;
    let lowered = matches!(
        native.kind,
        ProviderNativeWebSearchKind::OpenAi | ProviderNativeWebSearchKind::Xai
    );
    Some(ProviderNativeWebSearchDiagnostics {
        kind: native.kind,
        provider_id: native.provider_id.clone(),
        provider_model_ref: native.provider_model_ref.clone(),
        advertised_tool_type: native.advertised_tool_type.clone(),
        backend_kind: native.backend_kind.clone(),
        lowered,
        fallback_reason: (!lowered).then(|| {
            "openai responses transport only supports OpenAI/xAI-native web search".into()
        }),
    })
}

pub(in super::super) fn update_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<ContinuationScopeId>,
    request_shape: OpenAiRequestShape,
    append_match_input: Vec<Value>,
    provider_input: Vec<Value>,
    prior_replay_loss_reason: Option<String>,
    parsed: &ParsedOpenAiResponse,
) {
    let Some(scope) = scope else {
        return;
    };
    let mut state = lock_openai_continuation(continuation);
    let latest_input_tokens = parsed.response.input_tokens;
    let next = match (parsed.response_id.as_ref(), parsed.output_items.is_empty()) {
        (Some(response_id), false) => {
            state.next_generation = state.next_generation.saturating_add(1);
            let response_id = Some(response_id.clone());
            let mut items = provider_input
                .into_iter()
                .map(|item| canonicalize_openai_provider_item(&item))
                .collect::<Vec<_>>();
            items.extend(
                parsed
                    .output_items
                    .iter()
                    .map(canonicalize_openai_provider_item),
            );
            let mut append_match_items = append_match_input;
            append_match_items.extend(openai_append_match_output_items(&parsed.output_items));
            Some(OpenAiProviderWindow {
                response_id,
                request_shape,
                latest_compaction_index: latest_openai_compaction_index(&items),
                items,
                append_match_items,
                replay_loss_reason: prior_replay_loss_reason.or_else(|| {
                    parsed
                        .output_items
                        .iter()
                        .any(openai_is_server_side_search_item)
                        .then(|| "server_side_search_context".into())
                }),
                generation: state.next_generation,
                latest_input_tokens,
            })
        }
        _ => None,
    };
    if let Some(next) = next {
        state.windows.insert(scope, next);
    } else {
        state.windows.remove(&scope);
    }
}

pub(in super::super) fn openai_append_match_output_items(output_items: &[Value]) -> Vec<Value> {
    output_items
        .iter()
        .filter(|item| !openai_is_server_side_search_item(item))
        .filter_map(openai_append_match_output_item)
        .collect()
}

pub(in super::super) fn openai_is_server_side_search_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("web_search_call" | "x_search_call")
    )
}

pub(in super::super) fn openai_append_match_output_item(item: &Value) -> Option<Value> {
    if matches!(item.get("type").and_then(Value::as_str), Some("reasoning")) {
        None
    } else {
        Some(canonicalize_openai_append_match_item(item))
    }
}

pub(in super::super) fn openai_append_match_input_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_append_match_item)
        .collect()
}

// Provider windows are replayed into future OpenAI requests and compact calls.
// Keep their wire shape close to OpenAI's item contract while stripping fields
// that are not accepted on replay.
pub(in super::super) fn canonicalize_openai_provider_item(item: &Value) -> Value {
    let mut item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    if object.get("type").and_then(Value::as_str) == Some("compaction_summary") {
        object.insert("type".into(), Value::String("compaction".into()));
    }
    if matches!(
        object.get("type").and_then(Value::as_str),
        Some("message" | "function_call" | "custom_tool_call" | "reasoning" | "compaction")
    ) {
        object.remove("status");
    }
    if object.get("type").and_then(Value::as_str) == Some("function_call") {
        let arguments = normalize_openai_function_arguments(object.get("arguments"));
        object.insert(
            "arguments".into(),
            Value::String(canonical_json(&arguments)),
        );
    }
    item
}

// Append matching compares provider outputs against Holon-rebuilt input. Use a
// semantic form that preserves item order and conversational meaning while
// ignoring provider-only metadata and nested text decorations.
pub(in super::super) fn canonicalize_openai_append_match_item(item: &Value) -> Value {
    let item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object() else {
        return item;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("message") => canonicalize_openai_append_match_message(object),
        Some("function_call") => canonicalize_openai_append_match_function_call(object),
        Some("custom_tool_call") => canonicalize_openai_append_match_custom_tool_call(object),
        Some("function_call_output" | "custom_tool_call_output") => json!({
            "type": object.get("type").cloned().unwrap_or(Value::Null),
            "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
            "output": object.get("output").cloned().unwrap_or(Value::Null),
        }),
        Some("compaction_summary") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some("compaction") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some(_) | None => canonicalize_openai_provider_item(&item),
    }
}

pub(in super::super) fn canonicalize_openai_append_match_message(
    object: &serde_json::Map<String, Value>,
) -> Value {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| canonicalize_openai_append_match_content_item(role, item))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

pub(in super::super) fn canonicalize_openai_append_match_content_item(
    role: &str,
    item: &Value,
) -> Value {
    let Some(object) = item.as_object() else {
        return item.clone();
    };
    let item_type = object.get("type").and_then(Value::as_str);
    if matches!(
        item_type,
        Some("output_text" | "input_text" | "text" | "message_text")
    ) {
        let normalized_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        return json!({
            "type": normalized_type,
            "text": object.get("text").cloned().unwrap_or(Value::String(String::new())),
        });
    }
    let mut canonical = serde_json::Map::new();
    if let Some(item_type) = object.get("type") {
        canonical.insert("type".into(), item_type.clone());
    }
    for key in ["text", "image_url", "file_id", "filename"] {
        if let Some(value) = object.get(key) {
            canonical.insert(key.into(), value.clone());
        }
    }
    Value::Object(canonical)
}

pub(in super::super) fn canonicalize_openai_append_match_function_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    let arguments = Value::String(canonical_json(&normalize_openai_function_arguments(
        object.get("arguments"),
    )));
    json!({
        "type": "function_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "arguments": arguments,
    })
}

pub(in super::super) fn canonicalize_openai_append_match_custom_tool_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    json!({
        "type": "custom_tool_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "input": object.get("input").cloned().unwrap_or(Value::Null),
    })
}

pub(in super::super) fn normalize_openai_function_arguments(arguments: Option<&Value>) -> Value {
    let parsed = match arguments {
        Some(Value::String(arguments)) if arguments.trim().is_empty() => return json!({}),
        Some(Value::String(arguments)) => {
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.clone()))
        }
        Some(arguments) => arguments.clone(),
        None => return json!({}),
    };
    match parsed {
        Value::Object(_) => parsed,
        raw => json!({ "_raw": raw }),
    }
}

pub(in super::super) fn openai_without_provider_item_id(item: &Value) -> Value {
    let mut item = item.clone();
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(id) = id.as_deref() {
        match object.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") if !object.contains_key("call_id") => {
                object.insert("call_id".into(), Value::String(id.to_string()));
            }
            _ => {}
        }
    }
    object.remove("id");
    item
}

pub(in super::super) fn openai_compaction_encrypted_content_hashes(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(|content| sha256_hex(content.as_bytes()))
        .collect()
}

pub(in super::super) fn openai_compaction_encrypted_content_bytes(items: &[Value]) -> Vec<usize> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(str::len)
        .collect()
}
