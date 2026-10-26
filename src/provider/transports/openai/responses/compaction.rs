use super::super::continuation::*;
use super::super::*;
use super::continuation::*;
use crate::provider::classify_provider_error;
use crate::provider::transports::http::DispatchGuardRejected;

#[cfg(test)]
#[path = "compaction_material_tests.rs"]
mod material_tests;

#[derive(Debug)]
pub(in super::super) struct OpenAiCompactionCandidate<'a> {
    pub(in super::super) items: &'a [Value],
    pub(in super::super) retained_tail: &'a [Value],
    pub(in super::super) latest_compaction_index: Option<usize>,
}

fn admit_tool_call_sets(items: &[Value]) -> Result<()> {
    // Both boundary and pairing scans own only set entries, never call-id text.
    let entries = items
        .iter()
        .filter(|item| item.get("call_id").and_then(Value::as_str).is_some())
        .count();
    crate::diagnostics::resource_io::charge_owned_metadata(
        u64::try_from(entries)?
            .checked_mul(256)
            .ok_or_else(|| anyhow::anyhow!("tool-call set size overflow"))?,
        entries
            .checked_mul(2)
            .ok_or_else(|| anyhow::anyhow!("tool-call set count overflow"))?,
    )?;
    Ok(())
}

fn is_compact_material_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::provider::ProviderTransportError>()
        .is_none()
}

pub(in super::super) async fn maybe_compact_openai_provider_window(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&ContinuationScopeId>,
    request_shape: &OpenAiRequestShape,
    compaction_policy: OpenAiCompactionPolicy,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    provider: &str,
    model_ref: &str,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
) -> Result<Option<ProviderOpenAiRemoteCompactionDiagnostics>> {
    let Some(scope) = scope else {
        return Ok(None);
    };
    if let Some(guard) = dispatch_guard {
        if !guard.allows_auxiliary_dispatch()? {
            return Ok(None);
        }
    }
    let Some(window) = ({
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }) else {
        return Ok(None);
    };
    let Some(trigger) =
        openai_compaction_trigger_for_window(&window, request_shape, compaction_policy)
    else {
        return Ok(None);
    };
    let source_material = window.material.clone();
    let material = &source_material;
    let result = async {
    material.admit_value(request_shape)?;
    let candidate = match material.read(|| {
        admit_tool_call_sets(&window.items)?;
        Ok(openai_provider_window_compaction_candidate(&window))
    })? {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: window.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(request_shape)),
                continuation_generation: Some(window.generation),
                error: None,
            }));
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Ok(Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            trigger.reason,
            input_items,
            candidate.latest_compaction_index,
            trigger.estimated_input_tokens,
            Some(trigger.trigger_input_tokens),
            Some(request_shape_hash),
            Some(window.generation),
            http_status,
            None,
        )));
    }
    material.admit_value(request_shape)?;
    material.read(|| crate::diagnostics::resource_io::admit_cloned_value(&candidate.items))?;
    let compact_body = build_openai_compact_request_body(request_shape, candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
        provider,
        model_ref,
        dispatch_guard,
        material,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) if error.is::<DispatchGuardRejected>() => return Err(error),
        Err(error) => {
            if is_compact_material_error(&error) {
                return Err(material.hold_error(error));
            }
            if is_non_persisted_compact_item_id_error(&error) {
                return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "invalid_non_persisted_item_id".into(),
                    trigger_reason: Some(trigger.reason.into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    estimated_input_tokens: trigger.estimated_input_tokens,
                    trigger_input_tokens: Some(trigger.trigger_input_tokens),
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(window.generation),
                    error: Some(error.to_string()),
                }));
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Ok(Some(openai_compact_unsupported_diagnostics(
                    "unsupported_endpoint",
                    trigger.reason,
                    input_items,
                    candidate.latest_compaction_index,
                    trigger.estimated_input_tokens,
                    Some(trigger.trigger_input_tokens),
                    Some(request_shape_hash),
                    Some(window.generation),
                    http_status,
                    Some(error.to_string()),
                )));
            }
            return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "failed".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: error_status(&error),
                input_items: Some(input_items),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: candidate.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: Some(window.generation),
                error: Some(error.to_string()),
            }));
        }
    };
    if compacted.is_empty() {
        return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        }));
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        }));
    }

    let output_items = compacted.len();
    let generation = {
        let mut state = lock_openai_continuation(continuation);
        let current_generation = state.windows.get(scope).map(|current| current.generation);
        if current_generation != Some(window.generation) {
            return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "stale_generation".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(input_items),
                output_items: Some(output_items),
                compaction_items: Some(compaction_items),
                latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: Some(encrypted_content_hashes),
                encrypted_content_bytes: Some(encrypted_content_bytes),
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: current_generation,
                error: Some(format!(
                    "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                    window.generation
                )),
            }));
        }
        let mut items = material.cloned_slice(openai_compacted_replay_items(&compacted))?;
        items.extend(material.cloned_slice(candidate.retained_tail)?);
        let latest_compaction_index = latest_openai_compaction_index(&items);
        let request_shape = material.cloned(request_shape)?;
        let append_match_items = material.cloned(&window.append_match_items)?;
        admit_openai_window_header(material)?;
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        state.windows.insert(
            scope.clone(),
            Arc::new(OpenAiProviderWindow {
                material: material.clone(),
                response_id: None,
                request_shape,
                items,
                append_match_items,
                replay_loss_reason: None,
                latest_compaction_index,
                latest_input_tokens: 0,
                generation,
            }),
        );
        generation
    };

    Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some(trigger.reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        estimated_input_tokens: trigger.estimated_input_tokens,
        trigger_input_tokens: Some(trigger.trigger_input_tokens),
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(generation),
        error: None,
    }))
    }.await;
    result.map_err(|error| material.hold_error(error))
}

pub(in super::super) async fn maybe_compact_openai_request_plan(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    plan: &mut OpenAiRequestPlan,
    compaction_policy: OpenAiCompactionPolicy,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    provider: &str,
    model_ref: &str,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
) -> Result<Option<ProviderOpenAiRemoteCompactionDiagnostics>> {
    if plan.diagnostics.request_lowering_mode != "incremental_continuation" {
        return Ok(None);
    }
    if let Some(guard) = dispatch_guard {
        if !guard.allows_auxiliary_dispatch()? {
            return Ok(None);
        }
    }
    let Some(scope) = plan.scope.as_ref() else {
        return Ok(None);
    };
    let Some(previous) = ({
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }) else {
        return Ok(None);
    };
    if previous.response_id.is_none() {
        return Ok(None);
    }
    let Some(trigger) =
        openai_compaction_trigger_for_request_plan(&previous, plan, compaction_policy)
    else {
        return Ok(None);
    };
    plan.material
        .read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&scope.as_str())?;
            Ok(())
        })
        .map_err(|error| plan.material.hold_error(error))?;
    let scope = scope.clone();
    let source_material = plan.material.clone();
    let material = &source_material;
    let result = async {
    material.admit_value(&plan.request_shape)?;
    let mut compactable_items = material.cloned(&previous.items)?;
    compactable_items.extend(material.cloned(&plan.provider_input)?);
    let candidate = match material.read(|| {
        admit_tool_call_sets(&compactable_items)?;
        Ok(openai_compaction_candidate(&compactable_items))
    })? {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(compactable_items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: latest_openai_compaction_index(&compactable_items),
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(&plan.request_shape)),
                continuation_generation: Some(previous.generation),
                error: None,
            }));
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(&plan.request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Ok(Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            trigger.reason,
            input_items,
            candidate.latest_compaction_index,
            trigger.estimated_input_tokens,
            Some(trigger.trigger_input_tokens),
            Some(request_shape_hash),
            Some(previous.generation),
            http_status,
            None,
        )));
    }
    material.admit_value(&plan.request_shape)?;
    material.read(|| crate::diagnostics::resource_io::admit_cloned_value(&candidate.items))?;
    let compact_body = build_openai_compact_request_body(&plan.request_shape, candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
        provider,
        model_ref,
        dispatch_guard,
        material,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) if error.is::<DispatchGuardRejected>() => return Err(error),
        Err(error)
            if dispatch_guard.is_some()
                && matches!(
                    classify_provider_error(&error).kind,
                    ProviderFailureKind::Timeout
                        | ProviderFailureKind::Connection
                        | ProviderFailureKind::ServerError
                        | ProviderFailureKind::RateLimited
                ) =>
        {
            return Err(error)
        }
        Err(error) => {
            if is_compact_material_error(&error) {
                return Err(material.hold_error(error));
            }
            if is_non_persisted_compact_item_id_error(&error) {
                return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "invalid_non_persisted_item_id".into(),
                    trigger_reason: Some(trigger.reason.into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    estimated_input_tokens: trigger.estimated_input_tokens,
                    trigger_input_tokens: Some(trigger.trigger_input_tokens),
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(previous.generation),
                    error: Some(error.to_string()),
                }));
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Ok(Some(openai_compact_unsupported_diagnostics(
                    "unsupported_endpoint",
                    trigger.reason,
                    input_items,
                    candidate.latest_compaction_index,
                    trigger.estimated_input_tokens,
                    Some(trigger.trigger_input_tokens),
                    Some(request_shape_hash),
                    Some(previous.generation),
                    http_status,
                    Some(error.to_string()),
                )));
            }
            return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "failed".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: error_status(&error),
                input_items: Some(input_items),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: candidate.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: Some(previous.generation),
                error: Some(error.to_string()),
            }));
        }
    };
    if compacted.is_empty() {
        return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        }));
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        }));
    }

    let current_generation = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(&scope).map(|current| current.generation)
    };
    if current_generation != Some(previous.generation) {
        return Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "stale_generation".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(compaction_items),
            latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(encrypted_content_hashes),
            encrypted_content_bytes: Some(encrypted_content_bytes),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: current_generation,
            error: Some(format!(
                "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                previous.generation
            )),
        }));
    }

    let output_items = compacted.len();
    let mut provider_input = material.cloned_slice(openai_compacted_replay_items(&compacted))?;
    provider_input.extend(material.cloned_slice(candidate.retained_tail)?);
    plan.body["input"] = Value::Array(material.cloned(&provider_input)?);
    if let Some(object) = plan.body.as_object_mut() {
        object.remove("previous_response_id");
    }
    plan.provider_input = provider_input;
    plan.diagnostics.request_lowering_mode = "provider_window_compacted".into();

    Ok(Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some(trigger.reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        estimated_input_tokens: trigger.estimated_input_tokens,
        trigger_input_tokens: Some(trigger.trigger_input_tokens),
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(previous.generation),
        error: None,
    }))
    }.await;
    result.map_err(|error| material.hold_error(error))
}

fn compact_endpoint_unsupported_status(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
) -> Option<u16> {
    let state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .get(compact_url)
        .copied()
}

fn mark_compact_endpoint_unsupported(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
    http_status: u16,
) {
    let mut state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .insert(compact_url.to_string(), http_status);
}

fn unsupported_compact_endpoint_status(error: &anyhow::Error) -> Option<u16> {
    if is_non_persisted_compact_item_id_error(error) {
        return None;
    }
    let status = error_status(error)?;
    match status {
        404 | 405 | 410 | 501 => Some(status),
        _ => None,
    }
}

fn is_non_persisted_compact_item_id_error(error: &anyhow::Error) -> bool {
    error_status(error) == Some(404)
        && crate::provider::provider_error_code(error) == Some("non_persisted_item_id")
}

pub(super) fn error_status(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.status)
}

fn openai_compact_unsupported_diagnostics(
    status: &str,
    trigger_reason: &str,
    input_items: usize,
    latest_compaction_index: Option<usize>,
    estimated_input_tokens: Option<u64>,
    trigger_input_tokens: Option<u64>,
    request_shape_hash: Option<String>,
    continuation_generation: Option<u64>,
    http_status: u16,
    error: Option<String>,
) -> ProviderOpenAiRemoteCompactionDiagnostics {
    ProviderOpenAiRemoteCompactionDiagnostics {
        status: status.into(),
        trigger_reason: Some(trigger_reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: Some(http_status),
        input_items: Some(input_items),
        output_items: None,
        compaction_items: None,
        latest_compaction_index,
        estimated_input_tokens,
        trigger_input_tokens,
        encrypted_content_hashes: None,
        encrypted_content_bytes: None,
        request_shape_hash,
        continuation_generation,
        error,
    }
}

#[derive(Clone, Copy, Debug)]
pub(in super::super) struct OpenAiCompactionTrigger {
    pub(in super::super) reason: &'static str,
    pub(in super::super) estimated_input_tokens: Option<u64>,
    pub(in super::super) trigger_input_tokens: u64,
}

pub(in super::super) fn openai_compaction_trigger_for_window(
    window: &OpenAiProviderWindow,
    request_shape: &OpenAiRequestShape,
    policy: OpenAiCompactionPolicy,
) -> Option<OpenAiCompactionTrigger> {
    if window.latest_input_tokens > 0 {
        return (window.latest_input_tokens >= policy.trigger_input_tokens).then_some(
            OpenAiCompactionTrigger {
                reason: "token_budget_pressure",
                estimated_input_tokens: None,
                trigger_input_tokens: policy.trigger_input_tokens,
            },
        );
    }

    let estimated = estimate_openai_provider_payload_tokens(
        request_shape,
        openai_items_after_latest_compaction(&window.items),
    );
    (estimated >= policy.trigger_input_tokens).then_some(OpenAiCompactionTrigger {
        reason: "estimated_window_pressure",
        estimated_input_tokens: Some(estimated),
        trigger_input_tokens: policy.trigger_input_tokens,
    })
}

pub(in super::super) fn openai_compaction_trigger_for_request_plan(
    previous: &OpenAiProviderWindow,
    plan: &OpenAiRequestPlan,
    policy: OpenAiCompactionPolicy,
) -> Option<OpenAiCompactionTrigger> {
    let latest = latest_openai_compaction_index(&plan.provider_input)
        .map(|index| previous.items.len() + index)
        .or_else(|| latest_openai_compaction_index(&previous.items));
    if previous.latest_input_tokens == 0 && latest.is_some() {
        return None;
    }
    let estimated = previous
        .items
        .iter()
        .chain(&plan.provider_input)
        .skip(latest.map_or(0, |index| index + 1))
        .map(estimate_compaction_json_tokens)
        .fold(
            estimate_compaction_json_tokens(&plan.request_shape.wire_shape),
            usize::saturating_add,
        )
        .saturating_add(1) as u64;
    (estimated >= policy.trigger_input_tokens).then_some(OpenAiCompactionTrigger {
        reason: "estimated_window_pressure",
        estimated_input_tokens: Some(estimated),
        trigger_input_tokens: policy.trigger_input_tokens,
    })
}

fn estimate_compaction_json_tokens(value: &Value) -> usize {
    struct Length(usize);
    impl std::io::Write for Length {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut length = Length(0);
    match serde_json::to_writer(&mut length, value) {
        Ok(()) => length.0.saturating_add(3) / 4,
        Err(_) => 1,
    }
}

fn estimate_openai_provider_payload_tokens(
    request_shape: &OpenAiRequestShape,
    input_items: &[Value],
) -> u64 {
    let shape_tokens = estimate_compaction_json_tokens(&request_shape.wire_shape);
    let input_tokens = input_items
        .iter()
        .map(estimate_compaction_json_tokens)
        .fold(0usize, usize::saturating_add);
    shape_tokens.saturating_add(input_tokens).saturating_add(1) as u64
}

fn openai_items_after_latest_compaction(items: &[Value]) -> &[Value] {
    latest_openai_compaction_index(items)
        .map(|index| &items[index.saturating_add(1)..])
        .unwrap_or(items)
}

pub(in super::super) fn openai_provider_window_compaction_candidate(
    window: &OpenAiProviderWindow,
) -> std::result::Result<OpenAiCompactionCandidate<'_>, &'static str> {
    openai_compaction_candidate(&window.items)
}

fn openai_compaction_candidate(
    items: &[Value],
) -> std::result::Result<OpenAiCompactionCandidate<'_>, &'static str> {
    let boundary = latest_complete_openai_tool_call_boundary(items).ok_or("no_safe_boundary")?;
    debug_assert!(boundary > 0);

    let compact_items = &items[..boundary];
    if has_unpaired_openai_tool_call(&compact_items) {
        return Err("unpaired_tool_call");
    }

    Ok(OpenAiCompactionCandidate {
        latest_compaction_index: latest_openai_compaction_index(&compact_items),
        items: compact_items,
        retained_tail: &items[boundary..],
    })
}

fn openai_compacted_replay_items(compacted: &[Value]) -> &[Value] {
    latest_openai_compaction_index(compacted)
        .map(|index| &compacted[index..])
        .unwrap_or(compacted)
}

fn latest_complete_openai_tool_call_boundary(items: &[Value]) -> Option<usize> {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();
    let mut latest_complete_boundary = None;

    for (index, item) in items.iter().enumerate() {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            if openai_tool_call_sets_are_complete(
                &function_calls,
                &function_outputs,
                &custom_tool_calls,
                &custom_tool_outputs,
            ) {
                latest_complete_boundary = Some(index + 1);
            }
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id);
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id);
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id);
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id);
            }
            _ => {}
        }
        if openai_tool_call_sets_are_complete(
            &function_calls,
            &function_outputs,
            &custom_tool_calls,
            &custom_tool_outputs,
        ) {
            latest_complete_boundary = Some(index + 1);
        }
    }

    latest_complete_boundary
}

fn openai_tool_call_sets_are_complete(
    function_calls: &HashSet<&str>,
    function_outputs: &HashSet<&str>,
    custom_tool_calls: &HashSet<&str>,
    custom_tool_outputs: &HashSet<&str>,
) -> bool {
    function_calls.is_subset(function_outputs) && custom_tool_calls.is_subset(custom_tool_outputs)
}

fn has_unpaired_openai_tool_call(items: &[Value]) -> bool {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();

    for item in items {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id);
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id);
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id);
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id);
            }
            _ => {}
        }
    }

    !openai_tool_call_sets_are_complete(
        &function_calls,
        &function_outputs,
        &custom_tool_calls,
        &custom_tool_outputs,
    )
}

fn build_openai_compact_request_body(request_shape: &OpenAiRequestShape, items: &[Value]) -> Value {
    let compact_items = sanitize_openai_store_false_compact_items(items);
    let mut body = json!({
        "model": request_shape.wire_shape.get("model").cloned().unwrap_or(Value::Null),
        "input": compact_items,
        "instructions": request_shape
            .wire_shape
            .get("instructions")
            .cloned()
            .unwrap_or_else(|| Value::String(request_shape.prompt_frame.system_prompt.clone())),
        "tools": request_shape
            .wire_shape
            .get("tools")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "parallel_tool_calls": request_shape
            .wire_shape
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(Value::Bool(false)),
    });
    if let Some(reasoning) = request_shape.wire_shape.get("reasoning") {
        if !reasoning.is_null() {
            body["reasoning"] = reasoning.clone();
        }
    }
    if let Some(text) = request_shape.wire_shape.get("text") {
        body["text"] = text.clone();
    }
    body
}

fn sanitize_openai_store_false_compact_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect()
}

struct OpenAiCompactedItems {
    items: Vec<Value>,
    _material: crate::provider::ProviderRequestMaterial,
}

impl std::ops::Deref for OpenAiCompactedItems {
    type Target = [Value];
    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

async fn send_openai_compact_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    provider: &str,
    model_ref: &str,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<OpenAiCompactedItems> {
    let result = async {
        let request_trace = trace
            .map(|trace| {
                trace.begin_request_with_material(
                    agent_id,
                    provider,
                    Some(model_ref),
                    url.as_str(),
                    OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND,
                    &headers,
                    &body,
                    material,
                )
            })
            .transpose()?
            .flatten();
        let mut request = client.post(&url).header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = send_openai_request(
            request.body(material.json_bytes(&body)?),
            "OpenAI compact request failed",
            "request_send",
            provider,
            Some(model_ref),
            Some(url.as_str()),
            RequestDeadline::FullRequest,
            request_trace.as_ref(),
            dispatch_guard,
        )
        .await?;
        trace_response_headers(
            request_trace.as_ref(),
            response.status(),
            response.headers(),
        );

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = parse_retry_after(response.headers());
            let body = read_error_response_body(response, request_trace.as_ref(), material).await?;
            return Err(classify_status_error_with_trace(
                "OpenAI compact request failed",
                "response_status",
                Some(provider),
                Some(model_ref),
                Some(url.as_str()),
                status,
                body,
                request_trace.as_ref(),
                retry_after,
            ));
        }
        let response_body =
            match tokio::time::timeout(response_body_timeout(), material.response_text(response))
                .await
            {
                Ok(Ok(text)) => text,
                Ok(Err(error)) => {
                    let error = match error.downcast::<reqwest::Error>() {
                        Ok(error) => error,
                        Err(error) => return Err(error),
                    };
                    return Err(classify_reqwest_transport_error_with_trace(
                        "OpenAI compact response body failed",
                        "response_body",
                        provider,
                        Some(model_ref),
                        Some(url.as_str()),
                        error,
                        request_trace.as_ref(),
                    ));
                }
                Err(_) => {
                    return Err(timeout_transport_error_with_trace(
                        "OpenAI compact response body read timed out",
                        "response_body",
                        provider,
                        Some(model_ref),
                        Some(url.as_str()),
                        format!("timed out after {:?}", response_body_timeout()),
                        request_trace.as_ref(),
                    ))
                }
            };
        trace_response_body(request_trace.as_ref(), &response_body);
        material.read(|| crate::diagnostics::resource_io::admit_json_payload(&response_body))?;
        let parsed: Value = serde_json::from_str(&response_body)
            .map_err(|error| invalid_response_error("invalid OpenAI compact JSON", error))?;
        let output = parsed
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                invalid_response_error(
                    "OpenAI compact response did not contain output array",
                    "missing output array",
                )
            })?;
        material.read(|| crate::diagnostics::resource_io::admit_cloned_value(output))?;
        Ok(OpenAiCompactedItems {
            items: output
                .iter()
                .map(canonicalize_openai_provider_item)
                .collect(),
            _material: material.clone(),
        })
    }
    .await;
    if let Some(guard) = dispatch_guard {
        guard.after_request(result.as_ref().map(|_| None))?;
    }
    result.map_err(|error| material.hold_error(error))
}
