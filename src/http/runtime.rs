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

fn runtime_surfaces(
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
