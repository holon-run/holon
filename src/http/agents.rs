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
