use super::*;

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
    let workspace = state_workspace_snapshot(&agent);
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

pub(crate) fn slim_state_task_record(mut task: TaskRecord) -> TaskRecord {
    let _ = task.detail.take();
    let _ = task.recovery.take();
    task
}

pub(crate) fn slim_state_work_item_record(mut record: WorkItemRecord) -> WorkItemRecord {
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

fn state_workspace_snapshot(agent: &AgentSummary) -> StateWorkspaceSnapshot {
    StateWorkspaceSnapshot {
        attached_workspaces: agent.agent.attached_workspaces.clone(),
        active_workspace_entry: agent.agent.active_workspace_entry.clone(),
        active_workspace_occupancy: agent.active_workspace_occupancy.clone(),
        worktree_session: agent.agent.worktree_session.clone(),
    }
}

fn state_work_item_rank(item: &WorkItemRecord) -> u8 {
    match item.state {
        WorkItemState::Open if item.blocked_by.is_none() => 0,
        WorkItemState::Open => 1,
        WorkItemState::Completed => 2,
    }
}
