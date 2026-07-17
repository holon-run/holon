use super::super::continuation::*;
use super::super::*;
use super::continuation::*;

#[derive(Debug, Clone)]
pub(in super::super) struct OpenAiCompactionCandidate {
    pub(in super::super) items: Vec<Value>,
    pub(in super::super) retained_tail: Vec<Value>,
    pub(in super::super) latest_compaction_index: Option<usize>,
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
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    let Some(scope) = scope else {
        return None;
    };
    let window = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    let Some(trigger) =
        openai_compaction_trigger_for_window(&window, request_shape, compaction_policy)
    else {
        return None;
    };
    let candidate = match openai_provider_window_compaction_candidate(&window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
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
        ));
    }
    let compact_body = build_openai_compact_request_body(request_shape, &candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) => {
            if is_non_persisted_compact_item_id_error(&error) {
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
                });
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Some(openai_compact_unsupported_diagnostics(
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
                ));
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
            });
        }
    };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
        });
    }

    let output_items = compacted.len();
    let generation = {
        let mut state = lock_openai_continuation(continuation);
        let current_generation = state.windows.get(scope).map(|current| current.generation);
        if current_generation != Some(window.generation) {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
            });
        }
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        let mut items = openai_compacted_replay_items(&compacted);
        items.extend(candidate.retained_tail.clone());
        let latest_compaction_index = latest_openai_compaction_index(&items);
        state.windows.insert(
            scope.clone(),
            OpenAiProviderWindow {
                response_id: None,
                request_shape: request_shape.clone(),
                items,
                append_match_items: window.append_match_items,
                replay_loss_reason: None,
                latest_compaction_index,
                latest_input_tokens: 0,
                generation,
            },
        );
        generation
    };

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
    })
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
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    if plan.diagnostics.request_lowering_mode != "incremental_continuation" {
        return None;
    }
    let scope = plan.scope.as_ref()?;
    let previous = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    previous.response_id.as_ref()?;

    let mut compactable_items = previous.items.clone();
    compactable_items.extend(plan.provider_input.clone());
    let compactable_window = OpenAiProviderWindow {
        response_id: None,
        request_shape: plan.request_shape.clone(),
        latest_compaction_index: latest_openai_compaction_index(&compactable_items),
        items: compactable_items,
        append_match_items: plan.append_match_input.clone(),
        replay_loss_reason: previous.replay_loss_reason.clone(),
        latest_input_tokens: previous.latest_input_tokens,
        generation: previous.generation,
    };
    let Some(trigger) =
        openai_compaction_trigger_for_request_plan(&previous, plan, compaction_policy)
    else {
        return None;
    };
    let candidate = match openai_provider_window_compaction_candidate(&compactable_window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(compactable_window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: compactable_window.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(&plan.request_shape)),
                continuation_generation: Some(previous.generation),
                error: None,
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(&plan.request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
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
        ));
    }
    let compact_body = build_openai_compact_request_body(&plan.request_shape, &candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) => {
            if is_non_persisted_compact_item_id_error(&error) {
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
                });
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Some(openai_compact_unsupported_diagnostics(
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
                ));
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
            });
        }
    };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
        });
    }

    let current_generation = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).map(|current| current.generation)
    };
    if current_generation != Some(previous.generation) {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
        });
    }

    let output_items = compacted.len();
    let mut provider_input = openai_compacted_replay_items(&compacted);
    provider_input.extend(candidate.retained_tail.clone());
    plan.body["input"] = Value::Array(provider_input.clone());
    if let Some(object) = plan.body.as_object_mut() {
        object.remove("previous_response_id");
    }
    plan.provider_input = provider_input;
    plan.diagnostics.request_lowering_mode = "provider_window_compacted".into();

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
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
    })
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
        && error
            .to_string()
            .contains("Items are not persisted when `store` is set to false")
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
    let mut compactable_items = previous.items.clone();
    compactable_items.extend(plan.provider_input.clone());
    if previous.latest_input_tokens == 0
        && latest_openai_compaction_index(&compactable_items).is_some()
    {
        return None;
    }
    let estimated = estimate_openai_provider_payload_tokens(
        &plan.request_shape,
        openai_items_after_latest_compaction(&compactable_items),
    );
    (estimated >= policy.trigger_input_tokens).then_some(OpenAiCompactionTrigger {
        reason: "estimated_window_pressure",
        estimated_input_tokens: Some(estimated),
        trigger_input_tokens: policy.trigger_input_tokens,
    })
}

fn estimate_openai_provider_payload_tokens(
    request_shape: &OpenAiRequestShape,
    input_items: &[Value],
) -> u64 {
    let shape_tokens = estimate_json_tokens(&request_shape.wire_shape);
    let input_tokens = input_items
        .iter()
        .map(estimate_json_tokens)
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
) -> std::result::Result<OpenAiCompactionCandidate, &'static str> {
    let boundary =
        latest_complete_openai_tool_call_boundary(&window.items).ok_or("no_safe_boundary")?;
    debug_assert!(boundary > 0);

    let compact_items = window.items[..boundary].to_vec();
    if has_unpaired_openai_tool_call(&compact_items) {
        return Err("unpaired_tool_call");
    }

    Ok(OpenAiCompactionCandidate {
        latest_compaction_index: latest_openai_compaction_index(&compact_items),
        items: compact_items,
        retained_tail: window.items[boundary..].to_vec(),
    })
}

fn openai_compacted_replay_items(compacted: &[Value]) -> Vec<Value> {
    latest_openai_compaction_index(compacted)
        .map(|index| compacted[index..].to_vec())
        .unwrap_or_else(|| compacted.to_vec())
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
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
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
    function_calls: &HashSet<String>,
    function_outputs: &HashSet<String>,
    custom_tool_calls: &HashSet<String>,
    custom_tool_outputs: &HashSet<String>,
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
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
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

pub(in super::super) async fn send_openai_compact_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<Value>> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND,
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI compact request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        true,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            "OpenAI compact request failed",
            "response_status",
            Some("openai"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let response_body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI compact response body failed",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            ));
        }
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "OpenAI compact response body read timed out",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                format!("timed out after {:?}", response_body_timeout()),
                request_trace.as_ref(),
            ));
        }
    };
    trace_response_body(request_trace.as_ref(), &response_body);
    let parsed: Value = serde_json::from_str(&response_body)
        .map_err(|error| invalid_response_error("invalid OpenAI compact JSON", error))?;
    let output = parsed
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI compact response did not contain output array",
                "missing output array",
            )
        })?;
    Ok(output
        .into_iter()
        .map(|item| canonicalize_openai_provider_item(&item))
        .collect())
}
