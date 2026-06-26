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

#[derive(Debug, Clone, Copy)]
pub(crate) enum EnqueueIngress {
    Public,
    Trusted {
        delivery_surface: MessageDeliverySurface,
        admission_context: AdmissionContext,
    },
}

pub(crate) fn public_admission_context() -> AdmissionContext {
    AdmissionContext::PublicUnauthenticated
}

pub(crate) fn control_admission_context(state: &AppState) -> AdmissionContext {
    if state.require_control_token {
        AdmissionContext::ControlAuthenticated
    } else {
        AdmissionContext::LocalProcess
    }
}

pub(crate) async fn current_boundary_metadata(
    runtime: &crate::runtime::RuntimeHandle,
) -> Result<Value> {
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    Ok(HostLocalBoundary::from_snapshot(&execution.snapshot()).audit_metadata())
}

pub(crate) async fn enqueue_internal(
    state: Arc<AppState>,
    agent_id: String,
    request: EnqueueRequest,
    ingress: EnqueueIngress,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let kind = request.kind.unwrap_or(MessageKind::WebhookEvent);
    if matches!(kind, MessageKind::SystemTick | MessageKind::CallbackEvent) {
        return Err(forbidden(
            "runtime-owned message kinds may not be enqueued externally",
        ));
    }
    let priority = request.priority.unwrap_or(Priority::Normal);
    if matches!(ingress, EnqueueIngress::Public) && priority == Priority::Interject {
        return Err(forbidden("public enqueue may not use interject priority"));
    }
    let origin = match ingress {
        EnqueueIngress::Public => match request.origin {
            Some(IncomingOrigin::Channel {
                channel_id,
                sender_id,
            }) => MessageOrigin::Channel {
                channel_id,
                sender_id,
            },
            Some(IncomingOrigin::Webhook { source, event_type }) => {
                MessageOrigin::Webhook { source, event_type }
            }
            Some(_) => {
                return Err(forbidden(
                    "public enqueue only accepts channel or webhook origins",
                ));
            }
            None => MessageOrigin::Webhook {
                source: "http".into(),
                event_type: None,
            },
        },
        EnqueueIngress::Trusted { .. } => {
            request
                .origin
                .map(into_origin)
                .unwrap_or(MessageOrigin::Webhook {
                    source: "http".into(),
                    event_type: None,
                })
        }
    };
    let authority_class = match ingress {
        EnqueueIngress::Public => {
            if request.authority_class.is_some() {
                return Err(forbidden("public enqueue may not override authority_class"));
            }
            default_authority_for_origin(&origin)
        }
        EnqueueIngress::Trusted { .. } => request
            .authority_class
            .unwrap_or_else(|| default_authority_for_origin(&origin)),
    };
    let (delivery_surface, admission_context) = match ingress {
        EnqueueIngress::Public => (
            MessageDeliverySurface::HttpPublicEnqueue,
            public_admission_context(),
        ),
        EnqueueIngress::Trusted {
            delivery_surface,
            admission_context,
        } => (delivery_surface, admission_context),
    };
    let kind_decision = validate_message_kind_for_origin(&kind, &origin);
    if !kind_decision.allowed {
        return Err(forbidden(kind_decision.reason));
    }

    let body = request
        .body
        .unwrap_or_else(|| match (request.text, request.json) {
            (Some(text), _) => MessageBody::Text { text },
            (_, Some(value)) => MessageBody::Json { value },
            _ => MessageBody::Text {
                text: String::new(),
            },
        });

    let message = InboundRequest {
        agent_id: agent_id.clone(),
        kind,
        priority,
        origin,
        authority_class,
        body,
        delivery_surface,
        admission_context,
        metadata: request.metadata,
        correlation_id: request.correlation_id,
        causation_id: request.causation_id,
    }
    .into_message();

    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let queued = runtime.enqueue(message).await.map_err(error_response)?;

    Ok(Json(EnqueueResponse {
        ok: true,
        agent_id,
        message_id: queued.id,
    }))
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

pub async fn state_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    agent_state(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn agent_state(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let mut agent = runtime.agent_summary().await.map_err(error_response)?;
    let tasks_started = std::time::Instant::now();
    let tasks = runtime
        .active_tasks(STATE_BOOTSTRAP_TASK_LIMIT)
        .await
        .map_err(error_response)?
        .into_iter()
        .map(slim_state_task_record)
        .collect();
    crate::diagnostics::record_projection_state_tasks(tasks_started.elapsed());
    let timers_started = std::time::Instant::now();
    let timers = runtime.recent_timers(50).await.map_err(error_response)?;
    crate::diagnostics::record_projection_state_timers(timers_started.elapsed());
    let work_items_started = std::time::Instant::now();
    let mut work_items = runtime
        .latest_work_items_for_agent(&agent_id, STATE_BOOTSTRAP_WORK_ITEM_LIMIT)
        .await
        .map_err(error_response)?
        .into_iter()
        .map(slim_state_work_item_record)
        .collect::<Vec<_>>();
    crate::diagnostics::record_projection_state_work_items(work_items_started.elapsed());
    sort_state_work_items(&mut work_items);
    let waiting_started = std::time::Instant::now();
    let waiting_intents = runtime
        .latest_waiting_intents()
        .await
        .map_err(error_response)?
        .into_iter()
        .map(slim_state_waiting_intent_record)
        .collect();
    crate::diagnostics::record_projection_state_waiting_intents(waiting_started.elapsed());
    let triggers_started = std::time::Instant::now();
    let external_triggers = runtime
        .latest_external_triggers()
        .await
        .map_err(error_response)?
        .into_iter()
        .map(ExternalTriggerStateSnapshot::from)
        .collect();
    crate::diagnostics::record_projection_state_external_triggers(triggers_started.elapsed());
    let workspace = state_workspace_snapshot(&agent, &state);
    slim_state_agent_summary(&mut agent);
    let session = StateSessionSnapshot {
        current_run_id: agent.agent.current_run_id.clone(),
        pending_count: agent.agent.pending,
        last_turn: agent
            .agent
            .last_turn_terminal
            .clone()
            .map(slim_state_turn_terminal_record),
    };
    traced_json(
        "/agents/{agent_id}/state",
        started_at,
        AgentStateSnapshot {
            agent,
            session,
            tasks,
            timers,
            work_items,
            waiting_intents,
            external_triggers,
            workspace,
        },
    )
}

pub(crate) fn sort_state_work_items(work_items: &mut [WorkItemRecord]) {
    work_items.sort_by(|left, right| {
        state_work_item_rank(left)
            .cmp(&state_work_item_rank(right))
            .then_with(|| {
                if left.state == WorkItemState::Open && right.state == WorkItemState::Open {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.updated_at.cmp(&right.updated_at))
                } else {
                    right
                        .updated_at
                        .cmp(&left.updated_at)
                        .then_with(|| right.created_at.cmp(&left.created_at))
                }
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn slim_state_task_record(mut task: TaskRecord) -> TaskRecord {
    let _ = task.detail.take();
    let _ = task.recovery.take();
    task
}

fn slim_state_work_item_record(mut record: WorkItemRecord) -> WorkItemRecord {
    record.objective =
        truncate_state_bootstrap_string(&record.objective, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    record.plan_artifact = None;
    record.todo_list.clear();
    record.work_refs.clear();
    record.blocked_by = record
        .blocked_by
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    record.result_summary = record
        .result_summary
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    record
}

fn slim_state_waiting_intent_record(mut record: WaitingIntentRecord) -> WaitingIntentRecord {
    record.description =
        truncate_state_bootstrap_string(&record.description, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    record.source =
        truncate_state_bootstrap_string(&record.source, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    record.resource = record
        .resource
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    record.condition = record
        .condition
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    record
}

fn slim_state_agent_summary(agent: &mut AgentSummary) {
    agent.loaded_agents_md = Default::default();
    agent.skills = Default::default();
    agent.active_waiting_intents.clear();
    agent.active_wait_conditions.clear();
    agent.active_external_triggers.clear();
    agent.recent_operator_notifications.clear();
    agent.agent.context_summary = agent
        .agent
        .context_summary
        .take()
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    agent.agent.tool_latency.clear();
    agent.agent.working_memory.active_episode_builder = None;
    agent.agent.active_skills.clear();
    agent.agent.last_continuation = None;
    agent.agent.last_turn_terminal = agent
        .agent
        .last_turn_terminal
        .take()
        .map(slim_state_turn_terminal_record);
    if let Some(failure) = agent.agent.last_runtime_failure.as_mut() {
        failure.summary =
            truncate_state_bootstrap_string(&failure.summary, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        failure.detail_hint = failure
            .detail_hint
            .take()
            .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    }
}

fn slim_state_turn_terminal_record(mut record: TurnTerminalRecord) -> TurnTerminalRecord {
    record.reason = record
        .reason
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    record.last_assistant_message = record
        .last_assistant_message
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_LAST_TURN_TEXT_LIMIT));
    record.checkpoint = record.checkpoint.map(|mut checkpoint| {
        checkpoint.text =
            truncate_state_bootstrap_string(&checkpoint.text, STATE_BOOTSTRAP_LAST_TURN_TEXT_LIMIT);
        checkpoint
    });
    record
}

#[cfg(test)]
fn slim_state_transcript_entry(
    mut entry: crate::types::TranscriptEntry,
) -> crate::types::TranscriptEntry {
    entry.data = slim_state_json_value(entry.data, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT);
    entry
}

#[cfg(test)]
fn slim_state_json_value(value: Value, string_limit: usize) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_state_bootstrap_string(&text, string_limit)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(STATE_BOOTSTRAP_JSON_ARRAY_LIMIT)
                .map(|item| slim_state_json_value(item, string_limit))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, slim_state_json_value(value, string_limit)))
                .collect(),
        ),
        other => other,
    }
}

fn truncate_state_bootstrap_string(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }

    let truncated_char_limit = limit.saturating_sub(3);
    let mut truncate_at = None;
    for (index, (byte_index, _)) in text.char_indices().enumerate() {
        if limit <= 3 {
            if index == limit {
                return text[..byte_index].to_string();
            }
        } else {
            if index == truncated_char_limit {
                truncate_at = Some(byte_index);
            }
            if index == limit {
                let byte_index = truncate_at.unwrap_or(byte_index);
                return format!("{}...", &text[..byte_index]);
            }
        }
    }
    text.to_string()
}

fn state_work_item_rank(item: &WorkItemRecord) -> u8 {
    match item.state {
        WorkItemState::Open if item.blocked_by.is_none() => 0,
        WorkItemState::Open => 1,
        WorkItemState::Completed => 2,
    }
}

fn state_workspace_snapshot(agent: &AgentSummary, state: &AppState) -> StateWorkspaceSnapshot {
    let all_entries = state.host.workspace_entries().unwrap_or_default();

    // Collect all workspace IDs that might need alias resolution.
    let mut all_ws_ids: Vec<String> = agent.agent.attached_workspaces.clone();
    if let Some(active) = &agent.agent.active_workspace_entry {
        all_ws_ids.push(active.workspace_id.clone());
    }
    let alias_map = state
        .host
        .resolve_workspace_aliases(&all_ws_ids)
        .unwrap_or_default();
    let resolve_id = |ws_id: &str| -> String {
        alias_map
            .get(ws_id)
            .cloned()
            .unwrap_or_else(|| ws_id.to_string())
    };

    let mut workspace_entries: Vec<WorkspaceEntrySummary> = agent
        .agent
        .attached_workspaces
        .iter()
        .filter_map(|ws_id| {
            let resolved = resolve_id(ws_id);
            all_entries
                .iter()
                .find(|e| e.workspace_id == resolved)
                .map(WorkspaceEntrySummary::from_entry)
        })
        .collect();
    if let Some(active_entry) = agent.agent.active_workspace_entry.as_ref() {
        let resolved_active_id = resolve_id(&active_entry.workspace_id);
        if !workspace_entries
            .iter()
            .any(|entry| entry.workspace_id == resolved_active_id)
        {
            if let Some(entry) = all_entries
                .iter()
                .find(|entry| entry.workspace_id == resolved_active_id)
            {
                workspace_entries.push(WorkspaceEntrySummary::from_entry(entry));
            } else {
                workspace_entries.push(WorkspaceEntrySummary::from_active_entry(active_entry));
            }
        }
    }
    StateWorkspaceSnapshot {
        attached_workspaces: agent.agent.attached_workspaces.clone(),
        workspace_entries,
        active_workspace_entry: agent.agent.active_workspace_entry.clone(),
        active_workspace_occupancy: agent.active_workspace_occupancy.clone(),
        worktree_session: agent.agent.worktree_session.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        sort_state_work_items, STATE_BOOTSTRAP_JSON_ARRAY_LIMIT,
        STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT,
    };
    use crate::types::{
        TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TranscriptEntry,
        TranscriptEntryKind, WorkItemRecord, WorkItemState,
    };
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn state_sort_preserves_queue_display_order() {
        let mut active = WorkItemRecord::new("default", "active", WorkItemState::Open);
        active.updated_at = Utc::now() + Duration::minutes(5);

        let mut queued_early = WorkItemRecord::new("default", "queued first", WorkItemState::Open);
        queued_early.created_at = Utc::now();
        queued_early.updated_at = queued_early.created_at;

        let mut queued_late = WorkItemRecord::new("default", "queued second", WorkItemState::Open);
        queued_late.created_at = queued_early.created_at + Duration::minutes(1);
        queued_late.updated_at = queued_late.created_at;

        let mut waiting = WorkItemRecord::new("default", "waiting", WorkItemState::Open);
        waiting.created_at = queued_late.created_at + Duration::minutes(1);
        waiting.updated_at = waiting.created_at;

        let completed = WorkItemRecord::new("default", "completed", WorkItemState::Completed);
        let mut work_items = vec![
            waiting.clone(),
            completed,
            queued_late.clone(),
            active.clone(),
            queued_early.clone(),
        ];

        sort_state_work_items(&mut work_items);

        let ordered = work_items
            .iter()
            .map(|item| item.objective.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                active.objective.as_str(),
                queued_early.objective.as_str(),
                queued_late.objective.as_str(),
                waiting.objective.as_str(),
                "completed",
            ]
        );
    }

    #[test]
    fn state_bootstrap_omits_task_detail_and_slims_transcript_data() {
        let now = chrono::Utc::now();
        let task = TaskRecord {
            id: "task-1".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("large task".into()),
            detail: Some(json!({
                "cmd": "printf test",
                "output_path": "/tmp/output.log",
                "output_summary": "x".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64),
                "lines": (0..(STATE_BOOTSTRAP_JSON_ARRAY_LIMIT + 10)).collect::<Vec<_>>()
            })),
            recovery: None,
        };
        let slimmed = super::slim_state_task_record(task);
        assert!(slimmed.detail.is_none());
        assert!(slimmed.recovery.is_none());

        let entry = TranscriptEntry {
            id: "entry-1".into(),
            transcript_seq: None,
            agent_id: "default".into(),
            created_at: now,
            kind: TranscriptEntryKind::ToolResults,
            round: Some(1),
            related_message_id: None,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            data: json!({"content": "y".repeat(STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT + 64)}),
        };
        let slimmed_entry = super::slim_state_transcript_entry(entry);
        assert!(
            slimmed_entry.data["content"]
                .as_str()
                .expect("content")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT
        );
    }

    #[test]
    fn state_bootstrap_slims_work_item_records() {
        let mut item = WorkItemRecord::new(
            "default",
            "x".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64),
            WorkItemState::Open,
        );
        item.todo_list = vec![TodoItem {
            text: "large todo".into(),
            state: TodoItemState::InProgress,
        }];
        item.blocked_by = Some("b".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64));
        item.result_summary = Some("r".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64));

        let slimmed = super::slim_state_work_item_record(item);

        assert!(slimmed.objective.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        assert!(slimmed.todo_list.is_empty());
        assert!(slimmed.work_refs.is_empty());
        assert!(slimmed.plan_artifact.is_none());
        assert!(
            slimmed
                .blocked_by
                .as_deref()
                .expect("blocker")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
        );
        assert!(
            slimmed
                .result_summary
                .as_deref()
                .expect("result")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
        );
    }

    #[test]
    fn state_bootstrap_string_truncation_preserves_total_budget() {
        assert_eq!(super::truncate_state_bootstrap_string("abcdef", 0), "");
        assert_eq!(super::truncate_state_bootstrap_string("abcdef", 2), "ab");
        assert_eq!(
            super::truncate_state_bootstrap_string("abcdef", 6),
            "abcdef"
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("abcdefg", 6),
            "abc..."
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("你好世界", 3),
            "你好世"
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("你好世界a", 4),
            "你..."
        );
    }
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
