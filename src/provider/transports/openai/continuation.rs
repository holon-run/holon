use super::*;

#[derive(Debug, Default)]
pub(super) struct OpenAiContinuationState {
    pub(super) windows: HashMap<ContinuationScopeId, Arc<OpenAiProviderWindow>>,
    pub(super) unsupported_compact_endpoints: HashMap<String, u16>,
    pub(super) next_generation: u64,
}

#[derive(Debug)]
pub(super) struct OpenAiProviderWindow {
    pub(super) material: crate::provider::ProviderRequestMaterial,
    pub(super) response_id: Option<String>,
    pub(super) request_shape: OpenAiRequestShape,
    pub(super) items: Vec<Value>,
    pub(super) append_match_items: Vec<Value>,
    pub(super) replay_loss_reason: Option<String>,
    pub(super) latest_compaction_index: Option<usize>,
    pub(super) latest_input_tokens: u64,
    pub(super) generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct OpenAiRequestShape {
    pub(super) wire_shape: Value,
    pub(super) prompt_frame: ProviderPromptFrame,
}

#[derive(Debug)]
pub(super) struct OpenAiRequestPlan {
    pub(super) material: crate::provider::ProviderRequestMaterial,
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

pub(super) fn admit_openai_window_header(
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<()> {
    material.read(|| {
        crate::diagnostics::resource_io::charge_owned_metadata(
            (std::mem::size_of::<OpenAiProviderWindow>() + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        Ok(())
    })
}

pub(super) fn admit_openai_request_shape(
    body: &Value,
    request: &ProviderTurnRequest,
    excluded: &[&str],
) -> Result<()> {
    request.material.read(|| {
        use crate::diagnostics::resource_io::admit_cloned_value;
        admit_cloned_value(&request.prompt_frame)?;
        if let Some(object) = body.as_object() {
            for (key, value) in object {
                if !excluded.contains(&key.as_str()) {
                    admit_cloned_value(&(key, value))?;
                }
            }
        } else {
            admit_cloned_value(body)?;
        }
        Ok(())
    })
}

pub(super) fn request_shape_hash(request_shape: &OpenAiRequestShape) -> String {
    let value = json!({
        "wire_shape": request_shape.wire_shape,
        "prompt_frame": request_shape.prompt_frame,
    });
    canonical_json_hash(&value)
}

pub(super) fn canonical_json(value: &Value) -> String {
    // Map is key-ordered: the build does not enable serde_json/preserve_order.
    // Avoid retaining a second canonical String for every nested subtree.
    serde_json::to_string(value).expect("JSON Value is serializable")
}

struct JsonHashWriter(Sha256);
impl std::io::Write for JsonHashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn canonical_json_hash(value: &impl serde::Serialize) -> String {
    let mut writer = JsonHashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("JSON hash source is serializable");
    writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
        provider_id: None,
        provider_model_ref: None,
        provider_transport: None,
        endpoint_dialect: None,
        streaming: None,
        reasoning_effort: None,
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
        stable_prefix: None,
        transport_timeline: None,
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

pub(super) fn invalidate_openai_continuation_after_failure(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&ContinuationScopeId>,
    error: &anyhow::Error,
) {
    // A rejected local send has not changed the provider's continuation window.
    if !crate::provider::is_dispatch_guard_rejected(error) {
        invalidate_openai_continuation(continuation, scope);
    }
}

pub(super) fn lock_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> MutexGuard<'_, OpenAiContinuationState> {
    match continuation.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
