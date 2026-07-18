use super::*;
use crate::runtime_error::RuntimeError;

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolExecutionArtifactContent {
    pub artifact_index: usize,
    pub size: u64,
    pub content: String,
}

pub async fn tasks(
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
            .active_tasks(query.limit.unwrap_or(50))
            .await
            .map_err(error_response)?,
    ))
}

pub async fn task_status(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(task_not_found_response(&task_id));
    }
    let snapshot = runtime
        .managed_tasks()
        .task_status_snapshot(&task_id)
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(snapshot))
}

pub async fn task_output(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TaskOutputQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(task_not_found_response(&task_id));
    }
    let output = runtime
        .managed_tasks()
        .task_output(
            &task_id,
            query.block.unwrap_or(false),
            query.timeout_ms.unwrap_or(TASK_OUTPUT_DEFAULT_TIMEOUT_MS),
        )
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(output))
}

pub async fn tool_execution(
    Path((agent_id, tool_execution_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(record) = runtime
        .storage()
        .read_tool_execution_by_id(&tool_execution_id)
        .map_err(error_response)?
        .filter(|record| record.agent_id == agent_id)
    else {
        return Err(not_found(format!(
            "tool execution {tool_execution_id} not found"
        )));
    };
    Ok(Json(record))
}

pub async fn tool_execution_artifact(
    Path((agent_id, tool_execution_id, artifact_index)): Path<(String, String, usize)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(record) = runtime
        .storage()
        .read_tool_execution_by_id(&tool_execution_id)
        .map_err(error_response)?
        .filter(|record| record.agent_id == agent_id)
    else {
        return Err(not_found(format!(
            "tool execution {tool_execution_id} not found"
        )));
    };
    let result = record
        .output
        .get("result")
        .or_else(|| {
            record
                .output
                .get("envelope")
                .and_then(|value| value.get("result"))
        })
        .unwrap_or(&record.output);
    let artifact_path = result
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|artifacts| artifacts.get(artifact_index))
        .and_then(|artifact| artifact.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| not_found(format!("artifact {artifact_index} not found")))?;

    let data_dir = std::fs::canonicalize(runtime.storage().data_dir())
        .map_err(|error| error_response(error.into()))?;
    let requested_path = PathBuf::from(artifact_path);
    let requested_path = if requested_path.is_absolute() {
        requested_path
    } else {
        data_dir.join(requested_path)
    };
    let artifact_path = std::fs::canonicalize(&requested_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            not_found(format!("artifact {artifact_index} not found"))
        } else {
            error_response(error.into())
        }
    })?;
    if !artifact_path.starts_with(&data_dir) {
        return Err(forbidden("artifact path escapes runtime data directory"));
    }
    let metadata = tokio::fs::metadata(&artifact_path)
        .await
        .map_err(|error| error_response(error.into()))?;
    if !metadata.is_file() {
        return Err(bad_request("artifact path is not a file"));
    }
    let content = tokio::fs::read_to_string(&artifact_path)
        .await
        .map_err(|error| error_response(error.into()))?;
    Ok(Json(ToolExecutionArtifactContent {
        artifact_index,
        size: metadata.len(),
        content,
    }))
}

pub async fn task_input(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TaskInputRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(task_not_found_response(&task_id));
    }
    let authority_class = request
        .authority_class
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let result = runtime
        .managed_tasks()
        .task_input_with_trust(&task_id, &request.text, &authority_class)
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(result))
}

pub async fn task_stop(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TaskStopRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(task_not_found_response(&task_id));
    }
    let authority_class = request
        .authority_class
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let task = runtime
        .managed_tasks()
        .stop_task(&task_id, &authority_class)
        .await
        .map_err(task_lifecycle_error)?;
    let force_stop_requested = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("force_stop_requested"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snapshot = TaskStatusSnapshot::from_task_record(&task);
    let result = TaskStopResult {
        summary_text: Some(match task.status {
            TaskStatus::Cancelling => format!("stop requested for task {}", task.id),
            TaskStatus::Cancelled => format!("cancelled task {}", task.id),
            _ => format!("updated task {}", task.id),
        }),
        task: snapshot,
        stop_requested: true,
        force_stop_requested,
    };
    Ok(Json(result))
}

pub async fn create_command_task(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateCommandTaskRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let effective_trust = provided_trust
        .clone()
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let task = runtime
        .schedule_command_task(
            request.summary,
            crate::types::CommandTaskSpec {
                cmd: request.cmd,
                workdir: request.workdir,
                shell: request.shell,
                login: request.login.unwrap_or(true),
                tty: request.tty.unwrap_or(false),
                yield_time_ms: request.yield_time_ms.unwrap_or(10_000),
                max_output_tokens: request.max_output_tokens,
                accepts_input: request.accepts_input.unwrap_or(false),
                terminal_reentry: false,
            },
            effective_trust.clone(),
        )
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "task_create_requested",
            json!({
                "task_id": task.id,
                "kind": task.kind,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "effective_trust": effective_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(task))
}

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
    let work_items = runtime
        .storage()
        .work_queue_read_model()
        .map_err(error_response)?
        .items
        .into_iter()
        .take(query.limit.unwrap_or(50))
        .collect::<Vec<_>>();
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
        .storage()
        .work_queue_read_model()
        .map_err(error_response)?
        .items
        .into_iter()
        .find(|item| item.id == work_item_id && item.agent_id == agent_id)
    else {
        return Err(error_response(
            RuntimeError::not_found(
                "work_item_not_found",
                format!("work item {work_item_id} not found"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into(),
        ));
    };
    Ok(Json(work_item))
}

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
        return Err(error_response(
            RuntimeError::not_found("timer_not_found", format!("timer {timer_id} not found"))
                .with_safe_context("timer_id", timer_id)
                .into(),
        ));
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

fn timer_lifecycle_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    error_response(err)
}

fn task_not_found_response(task_id: &str) -> (StatusCode, Json<Value>) {
    error_response(
        RuntimeError::not_found("task_not_found", format!("task {task_id} not found"))
            .with_safe_context("task_id", task_id)
            .into(),
    )
}
