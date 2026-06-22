use super::*;

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
        return Err(not_found(format!("task {task_id} not found")));
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
        return Err(not_found(format!("task {task_id} not found")));
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
        return Err(not_found(format!("task {task_id} not found")));
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
        return Err(not_found(format!("task {task_id} not found")));
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
