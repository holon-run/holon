use super::*;

pub async fn enqueue_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let agent_id = state.host.config().default_agent_id.clone();
    enqueue_internal(state, agent_id, request, EnqueueIngress::Public).await
}

pub async fn enqueue(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    enqueue_internal(state, agent_id, request, EnqueueIngress::Public).await
}
