use super::*;

#[derive(Debug, Default)]
pub(super) struct OpenAiContinuationState {
    pub(super) windows: HashMap<ContinuationScopeId, OpenAiProviderWindow>,
    pub(super) unsupported_compact_endpoints: HashMap<String, u16>,
    pub(super) next_generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct OpenAiProviderWindow {
    pub(super) response_id: Option<String>,
    pub(super) request_shape: OpenAiRequestShape,
    pub(super) items: Vec<Value>,
    pub(super) append_match_items: Vec<Value>,
    pub(super) replay_loss_reason: Option<String>,
    pub(super) latest_compaction_index: Option<usize>,
    pub(super) latest_input_tokens: u64,
    pub(super) generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenAiRequestShape {
    pub(super) wire_shape: Value,
    pub(super) prompt_frame: ProviderPromptFrame,
}

#[derive(Debug)]
pub(super) struct OpenAiRequestPlan {
    pub(super) body: Value,
    pub(super) fallback_replay: Option<(Value, Vec<Value>)>,
    pub(super) scope: Option<ContinuationScopeId>,
    pub(super) append_match_input: Vec<Value>,
    pub(super) provider_input: Vec<Value>,
    pub(super) replay_loss_reason: Option<String>,
    pub(super) request_shape: OpenAiRequestShape,
    pub(super) diagnostics: ProviderRequestDiagnostics,
}

#[derive(Debug, Clone, Default)]
pub(super) struct OpenAiContinuationMismatchDiagnostics {
    pub(super) expected_prefix_items: usize,
    pub(super) first_mismatch_index: Option<usize>,
    pub(super) previous_item_type: Option<String>,
    pub(super) current_item_type: Option<String>,
    pub(super) previous_item_id: Option<String>,
    pub(super) current_item_id: Option<String>,
    pub(super) previous_item_hash: Option<String>,
    pub(super) current_item_hash: Option<String>,
    pub(super) request_shape_hash: Option<String>,
    pub(super) first_mismatch_path: Option<String>,
    pub(super) mismatch_kind: Option<String>,
}

pub(super) fn continuation_scope(request: &ProviderTurnRequest) -> Option<ContinuationScopeId> {
    request.continuation_scope_id.clone()
}

pub(super) fn request_shape_hash(request_shape: &OpenAiRequestShape) -> String {
    let value = json!({
        "wire_shape": request_shape.wire_shape,
        "prompt_frame": request_shape.prompt_frame,
    });
    sha256_hex(canonical_json(&value).as_bytes())
}

pub(super) fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(values) => {
            let items = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let ordered = map
                .iter()
                .map(|(key, value)| (key, value))
                .collect::<BTreeMap<_, _>>();
            let items = ordered
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", items.join(","))
        }
    }
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

pub(super) fn incremental_diagnostics(
    request_lowering_mode: &str,
    fallback_reason: &str,
    incremental_input_items: Option<usize>,
    full_input_items: usize,
    mismatch: Option<OpenAiContinuationMismatchDiagnostics>,
    openai_request_controls: Option<ProviderOpenAiRequestControlsDiagnostics>,
    native_web_search: Option<ProviderNativeWebSearchDiagnostics>,
    response_format: Option<ProviderResponseFormatDiagnostics>,
) -> ProviderRequestDiagnostics {
    let mismatch = mismatch.unwrap_or_default();
    ProviderRequestDiagnostics {
        request_lowering_mode: request_lowering_mode.into(),
        anthropic_cache: None,
        anthropic_context_management: None,
        openai_request_controls,
        openai_remote_compaction: None,
        incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
            status: "fallback_full_request".into(),
            fallback_reason: Some(fallback_reason.into()),
            server_side_context_may_be_lost: None,
            incremental_input_items,
            full_input_items: Some(full_input_items),
            expected_prefix_items: Some(mismatch.expected_prefix_items),
            first_mismatch_index: mismatch.first_mismatch_index,
            previous_item_type: mismatch.previous_item_type,
            current_item_type: mismatch.current_item_type,
            previous_item_id: mismatch.previous_item_id,
            current_item_id: mismatch.current_item_id,
            previous_item_hash: mismatch.previous_item_hash,
            current_item_hash: mismatch.current_item_hash,
            request_shape_hash: mismatch.request_shape_hash,
            first_mismatch_path: mismatch.first_mismatch_path,
            mismatch_kind: mismatch.mismatch_kind,
        }),
        native_web_search,
        response_format,
    }
}

pub(super) fn response_format_diagnostics(
    lowered: bool,
    request: &ProviderTurnRequest,
) -> Option<ProviderResponseFormatDiagnostics> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => {
            Some(ProviderResponseFormatDiagnostics {
                requested: true,
                lowered,
                format_type: "json_schema".into(),
                schema_name: Some(format.name.clone()),
                fallback_reason: (!lowered)
                    .then(|| "transport does not support JSON Schema response format".into()),
            })
        }
    }
}

pub(super) fn invalidate_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&ContinuationScopeId>,
) {
    let Some(scope) = scope else {
        return;
    };
    lock_openai_continuation(continuation).windows.remove(scope);
}

pub(super) fn lock_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> MutexGuard<'_, OpenAiContinuationState> {
    match continuation.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
