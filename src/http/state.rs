use super::*;

const STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT: usize = 16;

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
    runtime.prune_stale_attached_workspaces().await.ok();
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    let tasks_started = std::time::Instant::now();
    let tasks = runtime
        .active_tasks(STATE_BOOTSTRAP_TASK_LIMIT)
        .await
        .map_err(error_response)?
        .into_iter()
        .map(crate::http_dto::SlimTaskDto::from)
        .collect();
    crate::diagnostics::record_projection_state_tasks(tasks_started.elapsed());
    let timers_started = std::time::Instant::now();
    let timers = runtime.recent_timers(50).await.map_err(error_response)?;
    crate::diagnostics::record_projection_state_timers(timers_started.elapsed());
    let work_items_started = std::time::Instant::now();
    let work_items = runtime
        .storage()
        .work_queue_read_model()
        .map_err(error_response)?
        .items
        .into_iter()
        .take(STATE_BOOTSTRAP_WORK_ITEM_LIMIT)
        .map(slim_state_work_item_dto)
        .collect::<Vec<_>>();
    crate::diagnostics::record_projection_state_work_items(work_items_started.elapsed());
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
    let mut agent_dto = crate::http_dto::SlimAgentDto::from(&agent);
    agent_dto.agent.last_turn_terminal = agent
        .agent
        .last_turn_terminal
        .clone()
        .map(slim_state_turn_terminal_record);
    agent_dto.agent.last_runtime_failure = agent
        .agent
        .last_runtime_failure
        .clone()
        .map(slim_state_runtime_failure);
    let session = crate::http_dto::StateSessionSnapshotDto {
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
        crate::http_dto::AgentStateSnapshotDto {
            agent: agent_dto,
            session,
            tasks,
            timers,
            work_items,
            external_triggers,
            workspace,
        },
    )
}

fn slim_state_work_item_dto(
    projection: crate::work_item_scheduling::WorkItemSchedulingProjection,
) -> crate::http_dto::SlimWorkItemDto {
    let mut dto = crate::http_dto::SlimWorkItemDto::from(projection);
    dto.objective =
        truncate_state_bootstrap_string(&dto.objective, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    dto.blocked_by = dto
        .blocked_by
        .take()
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    dto.result_summary = dto
        .result_summary
        .take()
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    dto
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

fn slim_state_runtime_failure(
    mut failure: crate::types::RuntimeFailureSummary,
) -> crate::types::RuntimeFailureSummary {
    failure.summary =
        truncate_state_bootstrap_string(&failure.summary, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    failure.detail_hint = failure
        .detail_hint
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    failure.failure_artifact = failure.failure_artifact.map(slim_state_failure_artifact);
    failure
}

fn slim_state_failure_artifact(
    mut artifact: crate::types::FailureArtifact,
) -> crate::types::FailureArtifact {
    artifact.kind =
        truncate_state_bootstrap_string(&artifact.kind, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    artifact.summary =
        truncate_state_bootstrap_string(&artifact.summary, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    artifact.recovery_hint = artifact
        .recovery_hint
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    artifact.provider = artifact
        .provider
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    artifact.model_ref = artifact
        .model_ref
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    artifact.task_id = artifact
        .task_id
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    for field in [
        &mut artifact.context.message_id,
        &mut artifact.context.turn_id,
        &mut artifact.context.run_id,
        &mut artifact.context.work_item_id,
        &mut artifact.context.tool_execution_id,
        &mut artifact.context.task_id,
        &mut artifact.context.correlation_id,
        &mut artifact.context.causation_id,
        &mut artifact.context.provider,
        &mut artifact.context.model_ref,
    ] {
        *field = field
            .take()
            .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
    }
    artifact.source_chain = artifact
        .source_chain
        .into_iter()
        .take(STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT)
        .map(|text| truncate_state_bootstrap_string(&text, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT))
        .collect();
    artifact.metadata = artifact
        .metadata
        .into_iter()
        .take(STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT)
        .map(|(key, value)| {
            (
                truncate_state_bootstrap_string(&key, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT),
                truncate_state_bootstrap_string(&value, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT),
            )
        })
        .collect();
    artifact
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

fn state_workspace_snapshot(
    agent: &AgentSummary,
    state: &AppState,
) -> crate::http_dto::StateWorkspaceSnapshotDto {
    let all_entries = state.host.workspace_entries().unwrap_or_default();

    use crate::types::AgentWorkspaceInfo;
    use std::collections::HashSet;

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut workspaces: Vec<AgentWorkspaceInfo> = Vec::new();

    // Add active workspace first (with full runtime info).
    if let Some(active_entry) = agent.agent.active_workspace_entry.as_ref() {
        seen_ids.insert(active_entry.workspace_id.clone());

        // Try to enrich with registry data (alias, repo_name).
        let registry_entry = all_entries
            .iter()
            .find(|e| e.workspace_id == active_entry.workspace_id);
        let worktree = build_worktree_info(
            active_entry.projection_metadata.as_ref(),
            agent.agent.worktree_session.as_ref(),
        );

        workspaces.push(AgentWorkspaceInfo {
            workspace_id: active_entry.workspace_id.clone(),
            workspace_alias: registry_entry.and_then(|e| e.workspace_alias.clone()),
            workspace_anchor: Some(active_entry.workspace_anchor.display().to_string()),
            repo_name: registry_entry.and_then(|e| e.repo_name.clone()),
            is_active: true,
            execution_root_id: Some(active_entry.execution_root_id.clone()),
            execution_root: Some(active_entry.execution_root.display().to_string()),
            cwd: Some(active_entry.cwd.display().to_string()),
            projection_kind: Some(active_entry.projection_kind),
            access_mode: Some(active_entry.access_mode),
            worktree,
        });
    }

    // Add remaining attached workspaces (identity-only info from registry).
    for ws_id in &agent.agent.attached_workspaces {
        if seen_ids.contains(ws_id) {
            continue;
        }
        seen_ids.insert(ws_id.clone());

        if let Some(entry) = all_entries.iter().find(|e| e.workspace_id == *ws_id) {
            workspaces.push(AgentWorkspaceInfo {
                workspace_id: entry.workspace_id.clone(),
                workspace_alias: entry.workspace_alias.clone(),
                workspace_anchor: Some(entry.workspace_anchor.display().to_string()),
                repo_name: entry.repo_name.clone(),
                is_active: false,
                execution_root_id: None,
                execution_root: None,
                cwd: None,
                projection_kind: None,
                access_mode: None,
                worktree: None,
            });
        } else {
            // Fallback for IDs without a registry entry.
            workspaces.push(AgentWorkspaceInfo {
                workspace_id: ws_id.clone(),
                workspace_alias: None,
                workspace_anchor: None,
                repo_name: None,
                is_active: false,
                execution_root_id: None,
                execution_root: None,
                cwd: None,
                projection_kind: None,
                access_mode: None,
                worktree: None,
            });
        }
    }

    crate::http_dto::StateWorkspaceSnapshotDto {
        workspaces: workspaces.into_iter().map(Into::into).collect(),
    }
}

/// Merge projection_metadata and worktree_session into a unified WorktreeInfo.
fn build_worktree_info(
    metadata: Option<&crate::types::WorkspaceProjectionMetadata>,
    session: Option<&crate::types::WorktreeSession>,
) -> Option<crate::types::WorktreeInfo> {
    use crate::types::WorkspaceProjectionMetadata;
    let (branch, path) = match metadata {
        Some(WorkspaceProjectionMetadata::ManagedWorktree {
            worktree_branch,
            worktree_path,
            ..
        }) => (
            Some(worktree_branch.clone()),
            Some(worktree_path.display().to_string()),
        ),
        Some(WorkspaceProjectionMetadata::ExistingGitWorktree { worktree_root }) => {
            (None, Some(worktree_root.display().to_string()))
        }
        None => match session {
            Some(s) => (
                Some(s.worktree_branch.clone()),
                Some(s.worktree_path.display().to_string()),
            ),
            None => (None, None),
        },
    };
    let original_branch = session.map(|s| s.original_branch.clone());
    let original_cwd = session.map(|s| s.original_cwd.display().to_string());

    if branch.is_some() || path.is_some() || original_branch.is_some() || original_cwd.is_some() {
        Some(crate::types::WorktreeInfo {
            branch,
            path,
            original_branch,
            original_cwd,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT, STATE_BOOTSTRAP_JSON_ARRAY_LIMIT,
        STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT,
    };
    use crate::types::{
        FailureArtifact, FailureArtifactCategory, RuntimeFailurePhase, RuntimeFailureSummary,
        TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TranscriptEntry,
        TranscriptEntryKind, WorkItemRecord, WorkItemState,
    };
    use serde_json::json;

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
        let slimmed = crate::http_dto::SlimTaskDto::from(task);
        let value = serde_json::to_value(slimmed).expect("serialize slim task");
        assert!(value.get("detail").is_none());
        assert!(value.get("recovery").is_none());

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

        let projection = crate::work_item_scheduling::derive_work_item_scheduling(
            crate::work_item_scheduling::WorkItemSchedulingFacts {
                work_item: &item,
                is_current: false,
                is_yielded: false,
                active_wait_conditions: &[],
                trigger_delivery_by_id: &std::collections::BTreeMap::new(),
            },
        );
        let slimmed = super::slim_state_work_item_dto(projection);

        assert!(slimmed.objective.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        let value = serde_json::to_value(&slimmed).expect("serialize slim work item");
        assert!(value.get("todo_list").is_none());
        assert!(value.get("work_refs").is_none());
        assert!(value.get("plan_artifact").is_none());
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
    fn state_bootstrap_preserves_and_slims_last_runtime_failure() {
        let long_text = "x".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64);
        let failure = RuntimeFailureSummary {
            occurred_at: chrono::Utc::now(),
            summary: "s".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64),
            phase: RuntimeFailurePhase::RuntimeTurn,
            detail_hint: Some("d".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64)),
            failure_artifact: Some(FailureArtifact {
                category: FailureArtifactCategory::Transport,
                kind: long_text.clone(),
                summary: long_text.clone(),
                domain: Some(crate::runtime_error::RuntimeErrorDomain::Provider),
                retryable: Some(true),
                recovery_hint: Some(long_text.clone()),
                provider: Some(long_text.clone()),
                model_ref: Some(long_text.clone()),
                status: Some(500),
                task_id: Some(long_text.clone()),
                exit_status: None,
                source_chain: (0..STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT + 4)
                    .map(|_| long_text.clone())
                    .collect(),
                context: Box::new(crate::runtime_error::RuntimeErrorContext {
                    message_id: Some(long_text.clone()),
                    turn_id: Some(long_text.clone()),
                    run_id: Some(long_text.clone()),
                    work_item_id: Some(long_text.clone()),
                    tool_execution_id: Some(long_text.clone()),
                    task_id: Some(long_text.clone()),
                    correlation_id: Some(long_text.clone()),
                    causation_id: Some(long_text.clone()),
                    provider: Some(long_text.clone()),
                    model_ref: Some(long_text.clone()),
                }),
                metadata: (0..STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT + 4)
                    .map(|index| (format!("{index}-{long_text}"), long_text.clone()))
                    .collect(),
            }),
        };

        let slimmed = super::slim_state_runtime_failure(failure);

        assert!(slimmed.summary.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        assert!(
            slimmed
                .detail_hint
                .as_deref()
                .expect("detail hint")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
        );
        let artifact = slimmed.failure_artifact.expect("failure artifact");
        for text in [
            artifact.kind.as_str(),
            artifact.summary.as_str(),
            artifact.recovery_hint.as_deref().expect("recovery hint"),
            artifact.provider.as_deref().expect("provider"),
            artifact.model_ref.as_deref().expect("model ref"),
            artifact.task_id.as_deref().expect("task id"),
        ] {
            assert!(text.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        }
        for text in [
            artifact.context.message_id.as_deref().expect("message id"),
            artifact.context.turn_id.as_deref().expect("turn id"),
            artifact.context.run_id.as_deref().expect("run id"),
            artifact
                .context
                .work_item_id
                .as_deref()
                .expect("work item id"),
            artifact
                .context
                .tool_execution_id
                .as_deref()
                .expect("tool execution id"),
            artifact
                .context
                .task_id
                .as_deref()
                .expect("context task id"),
            artifact
                .context
                .correlation_id
                .as_deref()
                .expect("correlation id"),
            artifact
                .context
                .causation_id
                .as_deref()
                .expect("causation id"),
            artifact
                .context
                .provider
                .as_deref()
                .expect("context provider"),
            artifact
                .context
                .model_ref
                .as_deref()
                .expect("context model ref"),
        ] {
            assert!(text.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        }
        assert_eq!(
            artifact.source_chain.len(),
            STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT
        );
        assert!(artifact
            .source_chain
            .iter()
            .all(|text| text.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT));
        assert_eq!(
            artifact.metadata.len(),
            STATE_BOOTSTRAP_FAILURE_ARTIFACT_ENTRY_LIMIT
        );
        assert!(artifact.metadata.iter().all(|(key, value)| {
            key.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
                && value.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
        }));
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

    // ── build_worktree_info tests ──

    use crate::types::{WorkspaceProjectionMetadata, WorktreeInfo, WorktreeSession};
    use std::path::PathBuf;

    #[test]
    fn build_worktree_info_managed_worktree_metadata() {
        let metadata = WorkspaceProjectionMetadata::ManagedWorktree {
            original_cwd: PathBuf::from("/tmp/project"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/tmp/project/.worktrees/feature"),
            worktree_branch: "feature".into(),
        };
        let session = Some(WorktreeSession {
            original_cwd: PathBuf::from("/tmp/project"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/tmp/project/.worktrees/feature"),
            worktree_branch: "feature".into(),
        });

        let info = super::build_worktree_info(Some(&metadata), session.as_ref());
        let info = info.expect("should produce WorktreeInfo");
        assert_eq!(info.branch.as_deref(), Some("feature"));
        assert!(info
            .path
            .as_deref()
            .unwrap()
            .contains("/tmp/project/.worktrees/feature"));
        assert_eq!(info.original_branch.as_deref(), Some("main"));
        assert!(info
            .original_cwd
            .as_deref()
            .unwrap()
            .contains("/tmp/project"));
    }

    #[test]
    fn build_worktree_info_existing_git_worktree_metadata() {
        let metadata = WorkspaceProjectionMetadata::ExistingGitWorktree {
            worktree_root: PathBuf::from("/tmp/existing-wt"),
        };

        let info = super::build_worktree_info(Some(&metadata), None);
        let info = info.expect("should produce WorktreeInfo");
        assert!(
            info.branch.is_none(),
            "existing git worktree should not have branch from metadata"
        );
        assert_eq!(info.path.as_deref(), Some("/tmp/existing-wt"));
    }

    #[test]
    fn build_worktree_info_fallback_to_session_only() {
        let session = WorktreeSession {
            original_cwd: PathBuf::from("/tmp/project"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/tmp/project/.worktrees/dev"),
            worktree_branch: "dev".into(),
        };

        let info = super::build_worktree_info(None, Some(&session));
        let info = info.expect("should produce WorktreeInfo from session");
        assert_eq!(info.branch.as_deref(), Some("dev"));
        assert!(info
            .path
            .as_deref()
            .unwrap()
            .contains("/tmp/project/.worktrees/dev"));
        assert_eq!(info.original_branch.as_deref(), Some("main"));
    }

    #[test]
    fn build_worktree_info_returns_none_when_all_none() {
        let info = super::build_worktree_info(None, None);
        assert!(
            info.is_none(),
            "should return None when both metadata and session are absent"
        );
    }

    #[test]
    fn build_worktree_info_session_enriches_metadata_originals() {
        let metadata = WorkspaceProjectionMetadata::ManagedWorktree {
            original_cwd: PathBuf::from("/tmp/project"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/tmp/wt"),
            worktree_branch: "feat".into(),
        };
        let session = WorktreeSession {
            original_cwd: PathBuf::from("/tmp/project"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/tmp/wt"),
            worktree_branch: "feat".into(),
        };

        let info = super::build_worktree_info(Some(&metadata), Some(&session));
        let info = info.expect("should produce WorktreeInfo");
        assert_eq!(info.original_branch.as_deref(), Some("main"));
        assert!(info.original_cwd.is_some());
    }

    // ── AgentWorkspaceInfo serde round-trip ──

    use crate::system::{WorkspaceAccessMode, WorkspaceProjectionKind};
    use crate::types::AgentWorkspaceInfo;

    #[test]
    fn agent_workspace_info_serde_roundtrip_full() {
        let info = AgentWorkspaceInfo {
            workspace_id: "ws_abc".into(),
            workspace_alias: Some("my-project".into()),
            workspace_anchor: Some("/home/user/project".into()),
            repo_name: Some("project".into()),
            is_active: true,
            execution_root_id: Some("canonical_root:ws_abc".into()),
            execution_root: Some("/home/user/project".into()),
            cwd: Some("/home/user/project/src".into()),
            projection_kind: Some(WorkspaceProjectionKind::CanonicalRoot),
            access_mode: Some(WorkspaceAccessMode::ExclusiveWrite),
            worktree: Some(WorktreeInfo {
                branch: Some("feature".into()),
                path: Some("/tmp/wt".into()),
                original_branch: Some("main".into()),
                original_cwd: Some("/tmp/project".into()),
            }),
        };

        let json = serde_json::to_string(&info).unwrap();
        let decoded: AgentWorkspaceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, decoded);
    }

    #[test]
    fn agent_workspace_info_serde_roundtrip_minimal() {
        let info = AgentWorkspaceInfo {
            workspace_id: "ws_xyz".into(),
            workspace_alias: None,
            workspace_anchor: None,
            repo_name: None,
            is_active: false,
            execution_root_id: None,
            execution_root: None,
            cwd: None,
            projection_kind: None,
            access_mode: None,
            worktree: None,
        };

        let json = serde_json::to_string(&info).unwrap();
        let decoded: AgentWorkspaceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, decoded);
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
