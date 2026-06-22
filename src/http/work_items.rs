use super::*;

pub async fn create_work_item(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let objective = request.objective.trim().to_string();
    if objective.is_empty() {
        return Err(bad_request("objective must not be empty"));
    }
    let (runtime, record) = state
        .host
        .enqueue_public_work_item(&agent_id, objective)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "work_item_enqueue_requested",
            json!({
                "work_item_id": record.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn pick_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PickWorkItemRequest>,
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
    let reason = normalize_optional_non_empty(request.reason);
    if request.clear_blocker && reason.is_none() {
        return Err(bad_request(
            "clear_blocker requires a non-empty reason explaining why the blocker is resolved",
        ));
    }
    let picked = runtime
        .pick_work_item_with_reason_and_clear_blocker(work_item_id, reason, request.clear_blocker)
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_pick_requested",
            json!({
                "work_item_id": picked.current_work_item.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    let current_work_item_id = picked.current_work_item.id.clone();
    Ok(Json(PickWorkItemResponse {
        previous_work_item: picked.previous_work_item,
        current_work_item: picked.current_work_item,
        current_work_item_id,
        transition: picked.transition,
    }))
}

pub async fn update_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<UpdateWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let objective = request
        .objective
        .map(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                Err(bad_request("objective must not be empty"))
            } else {
                Ok(trimmed)
            }
        })
        .transpose()?;
    let blocked_by = request
        .blocked_by
        .map(parse_blocked_by_mutation)
        .transpose()?;
    if request.recheck_after == Some(0) {
        return Err(bad_request("recheck_after must be greater than 0"));
    }
    if request.recheck_after.is_some() && blocked_by.as_ref().is_none_or(Option::is_none) {
        return Err(bad_request(
            "recheck_after requires a non-empty blocked_by value",
        ));
    }
    if objective.is_none()
        && request.plan_status.is_none()
        && request.todo_list.is_none()
        && blocked_by.is_none()
    {
        return Err(bad_request(
            "request must include at least one mutation field",
        ));
    }
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let record = runtime
        .update_work_item_fields_with_recheck(
            work_item_id,
            objective,
            request.plan_status,
            None,
            request.todo_list,
            blocked_by,
            request.recheck_after,
        )
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_update_requested",
            json!({
                "work_item_id": record.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn complete_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CompleteWorkItemRequest>,
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
    let record = runtime
        .complete_work_item(work_item_id, Vec::new())
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_complete_requested",
            json!({
                "work_item_id": record.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn work_items(
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
    let mut work_items = runtime
        .latest_work_items_for_agent(&agent_id, query.limit.unwrap_or(50))
        .await
        .map_err(error_response)?;
    sort_state_work_items(&mut work_items);
    Ok(Json(work_items))
}

pub async fn work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(work_item) = runtime
        .latest_work_item(&work_item_id)
        .await
        .map_err(error_response)?
        .filter(|item| item.agent_id == agent_id)
    else {
        return Err(not_found(format!("work item {work_item_id} not found")));
    };
    Ok(Json(work_item))
}
