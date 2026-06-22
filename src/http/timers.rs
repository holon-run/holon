use super::*;

pub async fn timers(
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
    Ok(Json(
        runtime
            .recent_timers(query.limit.unwrap_or(50))
            .await
            .map_err(error_response)?,
    ))
}

pub async fn timer(
    Path((agent_id, timer_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(timer) = runtime
        .latest_timer(&timer_id)
        .await
        .map_err(error_response)?
        .filter(|timer| timer.agent_id == agent_id)
    else {
        return Err(not_found(format!("timer {timer_id} not found")));
    };
    Ok(Json(timer))
}

pub async fn create_timer(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateTimerRequest>,
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
    let timer = runtime
        .schedule_timer(request.duration_ms, request.interval_ms, request.summary)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "timer_create_requested",
            json!({
                "timer_id": timer.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(timer))
}

pub async fn cancel_timer(
    Path((agent_id, timer_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CancelTimerRequest>,
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
    let timer = runtime
        .cancel_timer(&timer_id)
        .await
        .map_err(timer_lifecycle_error)?;
    runtime
        .append_audit_event(
            "timer_cancel_requested",
            json!({
                "timer_id": timer.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(timer))
}
