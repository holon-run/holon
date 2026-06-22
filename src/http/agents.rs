use super::*;

pub async fn root(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    if accepts_html(&headers) {
        if let Some(response) = web_asset_response(&state, "index.html", false).await {
            return Ok(response);
        }
    }
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "default_agent": state.host.config().default_agent_id,
    }))
    .into_response())
}

pub async fn handshake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = state.host.config();
    Ok(Json(json!({
        "ok": true,
        "protocol": {
            "name": "holon-control",
            "version": 1,
        },
        "auth": {
            "mode": if state.require_control_token { "bearer" } else { "local" },
            "required": state.require_control_token,
        },
        "capabilities": [
            "agents.list",
            "agents.state",
            "agents.events",
            "agents.control",
            "tui.remote"
        ],
        "runtime": {
            "default_agent": config.default_agent_id,
            "workspace_dir": config.workspace_dir,
            "home_dir": config.home_dir,
            "listen": config.http_addr,
            "advertise_url": state.advertise_url,
        }
    })))
}

pub async fn list_agent_entries(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let agents = state
        .host
        .list_agent_entries()
        .await
        .map_err(error_response)?;
    crate::diagnostics::record_projection_agents_list(started_at.elapsed());
    traced_json("/agents/list", started_at, agents)
}

pub async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state.host.default_runtime().await.map_err(error_response)?;
    let available_models = runtime.available_models().await.map_err(error_response)?;
    let model_availability = runtime.model_availability().await.map_err(error_response)?;
    let model_discovery_cache = runtime
        .model_discovery_status()
        .await
        .map_err(error_response)?;
    traced_json(
        "/models",
        started_at,
        json!({
            "available_models": available_models,
            "model_availability": model_availability,
            "model_discovery_cache": model_discovery_cache,
        }),
    )
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let query = request.query.trim().to_string();
    if query.is_empty() {
        return Err(bad_request("query must not be empty"));
    }
    let limit = request
        .limit
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .clamp(1, SEARCH_MAX_LIMIT);
    let runtime = state.host.default_runtime().await.map_err(error_response)?;
    let agent_ids = normalize_search_agent_ids(request.agent_ids)?;
    let search_result = if agent_ids.is_empty() {
        runtime
            .search_memory(&query, limit, request.include_all_workspaces)
            .await
            .map_err(error_response)?
    } else {
        for agent_id in &agent_ids {
            state
                .host
                .get_public_agent(agent_id)
                .await
                .map_err(agent_access_error)?;
        }
        runtime
            .search_memory_for_agents(&query, limit, request.include_all_workspaces, &agent_ids)
            .await
            .map_err(error_response)?
    };
    traced_json(
        "/search",
        started_at,
        SearchResponse {
            query,
            limit,
            results: search_result.results,
            index_status: search_result.index_status,
        },
    )
}

fn normalize_search_agent_ids(
    agent_ids: Option<Vec<String>>,
) -> Result<Vec<String>, (StatusCode, Json<Value>)> {
    let Some(agent_ids) = agent_ids else {
        return Ok(Vec::new());
    };
    let mut normalized = Vec::new();
    for agent_id in agent_ids {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return Err(bad_request("agent_ids must not contain empty agent ids"));
        }
        if !normalized.iter().any(|existing| existing == agent_id) {
            normalized.push(agent_id.to_string());
        }
    }
    Ok(normalized)
}

pub async fn status_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    status(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn status(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_agent_for_local_status(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    traced_json("/agents/{agent_id}/status", started_at, agent)
}

pub async fn briefs_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    briefs(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
        Query(query),
    )
    .await
}

pub async fn briefs(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let briefs = runtime
        .recent_briefs(query.limit.unwrap_or(20))
        .await
        .map_err(error_response)?;
    Ok(Json(briefs))
}

pub async fn brief(
    Path((agent_id, brief_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(brief) = runtime
        .brief_by_id(&brief_id)
        .await
        .map_err(error_response)?
        .filter(|brief| brief.agent_id == agent_id)
    else {
        return Err(not_found(format!("brief {brief_id} not found")));
    };
    Ok(Json(brief))
}

pub async fn transcript_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    transcript(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
        Query(query),
    )
    .await
}

pub async fn transcript(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let transcript = runtime
        .recent_transcript(query.limit.unwrap_or(50))
        .await
        .map_err(error_response)?;
    Ok(Json(transcript))
}

pub async fn transcript_entry(
    Path((agent_id, entry_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(entry) = runtime
        .transcript_entry_by_id(&entry_id)
        .await
        .map_err(error_response)?
        .filter(|entry| entry.agent_id == agent_id)
    else {
        return Err(not_found(format!("transcript entry {entry_id} not found")));
    };
    Ok(Json(entry))
}

pub async fn transcript_batch_get(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<BatchGetTranscriptEntriesRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let mut entries = Vec::new();
    let mut missing_entry_ids = Vec::new();
    for entry_id in request.entry_ids {
        match runtime
            .transcript_entry_by_id(&entry_id)
            .await
            .map_err(error_response)?
        {
            Some(entry) if entry.agent_id == agent_id => entries.push(entry),
            _ => missing_entry_ids.push(entry_id),
        }
    }
    Ok(Json(BatchGetTranscriptEntriesResponse {
        entries,
        missing_entry_ids,
    }))
}

pub async fn worktree_summary_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    worktree_summary(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn worktree_summary(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let summary = runtime
        .summarize_worktree_tasks()
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "agent_id": agent_id,
        "summary": summary,
    })))
}

pub async fn list_skills(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let skills = crate::skills::list_installed_skills(&agent_home).map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skills": skills,
    })))
}

pub async fn install_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::InstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let skill_name =
        crate::skills::install_skill_with_user_home(&agent_home, Some(&user_home), &request.kind)
            .map_err(skill_install_error_response)?;
    runtime
        .append_audit_event(
            "skill_installed",
            json!({
                "target_agent_id": agent_id,
                "skill_name": skill_name,
                "kind": request.kind,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": skill_name,
    })))
}

pub async fn uninstall_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::UninstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    crate::skills::uninstall_skill(&agent_home, &request.name).map_err(error_response)?;
    runtime
        .append_audit_event(
            "skill_uninstalled",
            json!({
                "target_agent_id": agent_id,
                "skill_name": request.name,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": request.name,
    })))
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
