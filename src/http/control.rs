use super::*;

pub async fn runtime_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    let activity = runtime_activity_summary(&state.host)
        .await
        .map_err(error_response)?;
    let last_failure = state
        .host
        .public_agent_activity_snapshots()
        .await
        .map_err(error_response)?
        .into_iter()
        .filter_map(|agent| agent.last_runtime_failure)
        .max_by(|left, right| left.occurred_at.cmp(&right.occurred_at));
    let (startup_surface, runtime_surface) = runtime_surfaces(&state);
    Ok(Json(runtime_service.status_response(
        activity,
        last_failure,
        startup_surface,
        runtime_surface,
    )))
}

pub async fn runtime_readiness(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    let (startup_surface, runtime_surface) = runtime_surfaces(&state);
    Ok(Json(
        runtime_service.readiness_response(startup_surface, runtime_surface),
    ))
}

pub async fn runtime_performance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    Ok(Json(diagnostics::performance_snapshot()))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigReadResponse {
    pub ok: bool,
    pub config_file_path: std::path::PathBuf,
    pub runtime_surface: RuntimeConfigSurface,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigUpdateRequest {
    pub updates: Vec<RuntimeConfigUpdateEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigUpdateEntry {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default)]
    pub unset: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigUpdateResponse {
    pub ok: bool,
    pub changed: bool,
    pub config_file_path: std::path::PathBuf,
    pub results: Vec<RuntimeConfigUpdateResult>,
    pub runtime_surface: RuntimeConfigSurface,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigUpdateResult {
    pub key: String,
    pub effect: RuntimeConfigUpdateEffect,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigUpdateEffect {
    AcceptedRequiresRestart,
    AcceptedReloaded,
    Rejected,
}

pub async fn runtime_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    Ok(Json(RuntimeConfigReadResponse {
        ok: true,
        config_file_path: config.config_file_path.clone(),
        runtime_surface: RuntimeConfigSurface::new(&config),
    }))
}

pub async fn runtime_config_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RuntimeConfigUpdateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    let stored = load_persisted_config_at(&config.config_file_path).map_err(error_response)?;
    let mut candidate = stored.clone();
    let mut results = Vec::new();

    for update in request.updates {
        if !is_runtime_mutable_config_key(&update.key) {
            results.push(RuntimeConfigUpdateResult {
                key: update.key,
                effect: RuntimeConfigUpdateEffect::Rejected,
                reason: "unsupported or startup-only config key".into(),
            });
            continue;
        }

        let result = if update.unset {
            unset_config_key(&mut candidate, &update.key)
        } else {
            match update.value {
                Some(value) => {
                    set_config_key(&mut candidate, &update.key, &config_value_as_raw(value))
                }
                None => Err(anyhow!(
                    "runtime config update for {} requires value or unset=true",
                    update.key
                )),
            }
        };

        match result {
            Ok(()) => {
                results.push(RuntimeConfigUpdateResult {
                    key: update.key,
                    effect: RuntimeConfigUpdateEffect::AcceptedRequiresRestart,
                    reason: "persisted in config.json; the running host keeps its current effective config until restart/reload support is added".into(),
                });
            }
            Err(error) => results.push(RuntimeConfigUpdateResult {
                key: update.key,
                effect: RuntimeConfigUpdateEffect::Rejected,
                reason: error.to_string(),
            }),
        }
    }

    if results
        .iter()
        .any(|result| result.effect == RuntimeConfigUpdateEffect::Rejected)
    {
        reject_accepted_runtime_config_results(
            &mut results,
            "batch rejected; no runtime config updates were persisted",
        );
    } else if let Err(error) = validate_runtime_config_candidate(&config, &candidate) {
        reject_accepted_runtime_config_results(
            &mut results,
            &format!("updated config is invalid: {error}"),
        );
    }

    let changed = results
        .iter()
        .any(|result| result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart);

    if changed {
        save_persisted_config_at(&config.config_file_path, &candidate).map_err(error_response)?;
        // Hot-reload the runtime so the new config takes effect immediately.
        // The current turn (if any) completes with the old provider; the next
        // turn picks up the new config automatically.
        match state.host.reload_all_agents_config().await {
            Ok(()) => {
                // Mark results as reloaded instead of requiring restart.
                for result in &mut results {
                    if result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
                        result.effect = RuntimeConfigUpdateEffect::AcceptedReloaded;
                        result.reason = "applied via hot-reload".into();
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "config saved but hot-reload failed; restart needed");
                for result in &mut results {
                    if result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
                        result.reason =
                            format!("persisted in config.json, but hot-reload failed: {e}");
                    }
                }
            }
        }
    }
    let config = state.host.config();

    Ok(Json(RuntimeConfigUpdateResponse {
        ok: true,
        changed,
        config_file_path: config.config_file_path.clone(),
        results,
        runtime_surface: RuntimeConfigSurface::new(&config),
    }))
}

fn reject_accepted_runtime_config_results(results: &mut [RuntimeConfigUpdateResult], reason: &str) {
    for result in results {
        if result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
            result.effect = RuntimeConfigUpdateEffect::Rejected;
            if result.reason.is_empty() {
                result.reason = reason.into();
            } else {
                result.reason = format!("{reason}: {}", result.reason);
            }
        }
    }
}

fn validate_runtime_config_candidate(
    config: &crate::config::AppConfig,
    candidate: &HolonConfigFile,
) -> Result<()> {
    let credentials = load_credential_store_at(&credential_store_path(&config.home_dir))?;
    crate::web::materialize_web_config(&candidate.web, &credentials)?;
    Ok(())
}

fn is_runtime_mutable_config_key(key: &str) -> bool {
    matches!(
        key,
        "api.cors.enabled"
            | "api.cors.allowed_origins"
            | "api.cors.allowed_methods"
            | "api.cors.allowed_headers"
            | "api.cors.allow_credentials"
            | "api.cors.max_age_seconds"
            | "model.default"
            | "model.fallbacks"
            | "vision.default"
            | "models.catalog"
            | "model.unknown_fallback"
            | "model.unknown_fallback.context_window_tokens"
            | "model.unknown_fallback.effective_context_window_percent"
            | "model.unknown_fallback.prompt_budget_estimated_tokens"
            | "model.unknown_fallback.compaction_trigger_estimated_tokens"
            | "model.unknown_fallback.compaction_keep_recent_estimated_tokens"
            | "model.unknown_fallback.runtime_max_output_tokens"
            | "runtime.max_output_tokens"
            | "runtime.default_tool_output_tokens"
            | "runtime.max_tool_output_tokens"
            | "runtime.disable_provider_fallback"
    ) || key.starts_with("providers.")
        || key.starts_with("web.")
}

fn config_value_as_raw(value: Value) -> String {
    match value {
        Value::String(value) => value,
        other => other.to_string(),
    }
}

#[derive(Debug, Serialize)]
struct CredentialListResponse {
    ok: bool,
    profiles: Vec<CredentialProfileStatus>,
}

pub async fn list_credentials(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    let path = credential_store_path(&config.home_dir);
    let profiles = list_credential_profiles_at(&path).map_err(error_response)?;
    Ok(Json(CredentialListResponse { ok: true, profiles }))
}

#[derive(Debug, Deserialize)]
pub struct SetCredentialRequest {
    kind: String,
    material: String,
}

#[derive(Debug, Serialize)]
struct SetCredentialResponse {
    ok: bool,
    profile: CredentialProfileStatus,
}

pub async fn set_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(profile): Path<String>,
    Json(request): Json<SetCredentialRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    let path = credential_store_path(&config.home_dir);
    let kind = CredentialKind::parse(&request.kind).map_err(error_response)?;
    let profile_status = set_credential_profile_at(&path, &profile, kind, request.material)
        .map_err(error_response)?;
    // Hot-reload so the new credential is available without restart.
    if let Err(e) = state.host.reload_all_agents_config().await {
        tracing::warn!(error = %e, "credential saved but hot-reload failed; restart needed");
    }
    Ok(Json(SetCredentialResponse {
        ok: true,
        profile: profile_status,
    }))
}

#[derive(Debug, Serialize)]
struct DeleteCredentialResponse {
    ok: bool,
    profile: CredentialProfileStatus,
}

pub async fn delete_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(profile): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    let path = credential_store_path(&config.home_dir);
    let profile_status = remove_credential_profile_at(&path, &profile).map_err(error_response)?;
    if let Err(e) = state.host.reload_all_agents_config().await {
        tracing::warn!(error = %e, "credential deleted but hot-reload failed; restart needed");
    }
    Ok(Json(DeleteCredentialResponse {
        ok: true,
        profile: profile_status,
    }))
}

pub(super) fn runtime_surfaces(
    state: &AppState,
) -> (crate::daemon::RuntimeStartupSurface, RuntimeConfigSurface) {
    let config = state.host.config();
    let startup_surface = crate::daemon::RuntimeStartupSurface {
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        workspace_dir: config.workspace_dir.clone(),
        default_agent_id: config.default_agent_id.clone(),
        callback_base_url: config.callback_base_url.clone(),
        control_token_configured: config.control_token.is_some(),
        control_auth_mode: config.control_auth_mode.into(),
    };
    (startup_surface, RuntimeConfigSurface::new(&config))
}

pub async fn runtime_shutdown(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    graceful_runtime_shutdown(&state.host, runtime_service)
        .await
        .map_err(error_response)?;
    Ok(Json(runtime_service.shutdown_response()))
}

pub async fn create_agent(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let agent = state
        .host
        .create_named_agent(&agent_id, request.template.as_deref())
        .await
        .map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "agent_created",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(agent))
}

pub async fn control(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let action = request.action.clone();
    let runtime = state
        .host
        .control_public_agent(&agent_id, action.clone())
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "control_request_admitted",
            json!({
                "target_agent_id": agent_id,
                "action": action,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn abort_current_run(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AbortCurrentRunRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let mode = match request.mode.as_deref().unwrap_or("stop_after_abort") {
        "stop_after_abort" => CurrentRunAbortMode::StopAfterAbort,
        "pause_after_abort" => CurrentRunAbortMode::StopAfterAbort,
        other => {
            return Err(bad_request(format!(
                "unsupported abort mode {other}; expected stop_after_abort or deprecated alias pause_after_abort"
            )))
        }
    };
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class.clone();
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let outcome = runtime
        .abort_current_run(CurrentRunAbortRequest {
            run_id: request.run_id.clone(),
            mode,
        })
        .await
        .map_err(abort_error_response)?;
    Ok(Json(json!({
        "ok": true,
        "aborted": true,
        "agent_id": outcome.agent_id,
        "run_id": outcome.run_id,
        "mode": outcome.mode.as_str(),
        "admission_context": admission_context,
        "provided_trust": provided_trust,
    })))
}

pub async fn attach_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AttachWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let workspace = state
        .host
        .ensure_workspace_entry(std::path::PathBuf::from(&request.path))
        .map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .attach_workspace(&workspace)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_attach_requested",
            json!({
                "target_agent_id": agent_id,
                "workspace_id": workspace.workspace_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "workspace_id": workspace.workspace_id,
        "workspace_anchor": workspace.workspace_anchor,
    })))
}

pub async fn exit_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ExitWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime.exit_workspace().await.map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_exit_requested",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
    })))
}

pub async fn detach_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DetachWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let workspace_id = request.workspace_id.trim().to_string();
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .detach_workspace(&workspace_id)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_detach_requested",
            json!({
                "target_agent_id": agent_id,
                "workspace_id": workspace_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "workspace_id": workspace_id,
    })))
}

pub async fn set_agent_model(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SetAgentModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    if let Some(reasoning_effort) = request.reasoning_effort.as_deref() {
        validate_reasoning_effort(reasoning_effort)?;
    }
    let model = ModelRef::parse(&request.model).map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let model_state = runtime
        .set_model_override(model.clone(), request.reasoning_effort.clone())
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "model": model_state,
    })))
}

pub(super) fn validate_reasoning_effort(value: &str) -> Result<(), (StatusCode, Json<Value>)> {
    match value {
        "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(bad_request(format!(
            "invalid reasoning_effort '{value}'; must be one of low, medium, high, xhigh"
        ))),
    }
}

pub async fn clear_agent_model(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<ClearAgentModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let model_state = runtime
        .clear_model_override()
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "model": model_state,
    })))
}

pub async fn control_prompt(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlPromptRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    enqueue_internal(
        state,
        agent_id,
        EnqueueRequest {
            kind: Some(MessageKind::OperatorPrompt),
            priority: Some(Priority::Interject),
            authority_class: Some(AuthorityClass::OperatorInstruction),
            body: Some(MessageBody::Text { text: request.text }),
            text: None,
            json: None,
            metadata: Some(json!({ "control": true })),
            correlation_id: None,
            causation_id: None,
            origin: Some(IncomingOrigin::Operator {
                actor_id: Some("control".into()),
            }),
        },
        EnqueueIngress::Trusted {
            delivery_surface: MessageDeliverySurface::HttpControlPrompt,
            admission_context,
        },
    )
    .await
}

pub async fn create_operator_transport_binding(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<OperatorTransportBindingRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let target_agent_id = request.target_agent_id.unwrap_or_else(|| agent_id.clone());
    if target_agent_id != agent_id {
        return Err(bad_request(
            "operator transport binding target_agent_id must match route agent_id",
        ));
    }
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let delivery_auth = validate_operator_transport_delivery_auth(request.delivery_auth)?;
    let binding = OperatorTransportBinding {
        binding_id: non_empty_or_generated(request.binding_id, "opbind"),
        transport: require_non_empty(request.transport, "transport")?,
        operator_actor_id: require_non_empty(request.operator_actor_id, "operator_actor_id")?,
        target_agent_id,
        default_route_id: require_non_empty(request.default_route_id, "default_route_id")?,
        delivery_callback_url: require_non_empty(
            request.delivery_callback_url,
            "delivery_callback_url",
        )?,
        delivery_auth,
        capabilities: request.capabilities,
        provider: request.provider.and_then(non_empty_opt),
        provider_identity_ref: request.provider_identity_ref.and_then(non_empty_opt),
        status: OperatorTransportBindingStatus::Active,
        created_at: Utc::now(),
        last_seen_at: None,
        metadata: request.metadata,
    };
    let binding = runtime
        .upsert_operator_transport_binding(binding)
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "binding": binding,
    })))
}

pub async fn operator_ingress(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<OperatorIngressRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let text = require_non_empty(request.text, "text")?;
    let actor_id = require_non_empty(request.actor_id, "actor_id")?;
    let binding_id = require_non_empty(request.binding_id, "binding_id")?;
    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(mut binding) = runtime
        .active_operator_transport_binding(&binding_id)
        .await
        .map_err(error_response)?
    else {
        return Err(forbidden("operator transport binding is not active"));
    };
    if binding.target_agent_id != agent_id {
        return Err(forbidden(
            "operator transport binding does not target this agent",
        ));
    }
    if binding.operator_actor_id != actor_id {
        return Err(forbidden("operator transport actor does not match binding"));
    }
    let expected_provider = binding
        .provider
        .as_deref()
        .unwrap_or(&binding.transport)
        .to_string();
    if let Some(provider) = request.provider.as_deref().and_then(non_empty_str) {
        if provider != expected_provider {
            return Err(forbidden(
                "operator transport provider does not match binding",
            ));
        }
    }

    binding.last_seen_at = Some(Utc::now());
    runtime
        .upsert_operator_transport_binding(binding.clone())
        .await
        .map_err(error_response)?;

    let reply_route_id = request.reply_route_id.and_then(non_empty_opt);
    let metadata = json!({
        "operator_transport": {
            "binding_id": binding.binding_id,
            "transport": binding.transport,
            "reply_route_id": reply_route_id,
            "provider": request.provider.and_then(non_empty_opt).unwrap_or(expected_provider),
            "provider_identity_ref": binding.provider_identity_ref,
            "upstream_provider": request.upstream_provider,
            "provider_message_ref": request.provider_message_ref,
            "metadata": request.metadata,
        }
    });
    let message = InboundRequest {
        agent_id: agent_id.clone(),
        kind: MessageKind::OperatorPrompt,
        priority: Priority::Interject,
        origin: MessageOrigin::Operator {
            actor_id: Some(actor_id),
        },
        authority_class: AuthorityClass::OperatorInstruction,
        body: MessageBody::Text { text },
        delivery_surface: MessageDeliverySurface::RemoteOperatorTransport,
        admission_context: AdmissionContext::OperatorTransportAuthenticated,
        metadata: Some(metadata),
        correlation_id: request.correlation_id,
        causation_id: request.causation_id,
    }
    .into_message();
    let queued = runtime.enqueue(message).await.map_err(error_response)?;
    Ok(Json(EnqueueResponse {
        ok: true,
        agent_id,
        message_id: queued.id,
    }))
}

pub async fn control_debug_prompt(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DebugPromptRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let effective_trust = request
        .authority_class
        .clone()
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let boundary = state
        .host
        .public_agent_boundary_metadata(&agent_id)
        .map_err(agent_access_error)?;
    let dump = state
        .host
        .preview_public_agent_prompt(&agent_id, request.text.clone(), effective_trust.clone())
        .await
        .map_err(agent_access_error)?
        .render_dump();
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "admission_context": admission_context,
        "effective_trust": effective_trust,
        "boundary": boundary,
        "dump": dump,
    })))
}

pub async fn control_wake(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlWakeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    if request.reason.trim().is_empty() {
        return Err(forbidden("wake reason may not be empty"));
    }
    let admission_context = control_admission_context(&state);
    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(|error| match error {
            PublicAgentError::Stopped { agent_id } => stopped_agent_conflict(
                format!(
                    "agent {} is stopped; wake does not override stopped; start first",
                    agent_id
                ),
                agent_id,
            ),
            other => agent_access_error(other),
        })?;
    let reason = request.reason.clone();
    let disposition = runtime
        .submit_wake_hint(WakeHint {
            agent_id: agent_id.clone(),
            reason: reason.clone(),
            description: None,
            source: request.source,
            scope: None,
            waiting_intent_id: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: request.correlation_id,
            causation_id: request.causation_id,
        })
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "wake_requested",
            json!({
                "target_agent_id": agent_id,
                "reason": reason,
                "admission_context": admission_context,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(WakeResponse {
        ok: true,
        agent_id,
        disposition,
    }))
}
