use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::{
    config::{AppConfig, ModelRef},
    host::RuntimeHost,
    ingress::InboundRequest,
    provider::ProviderCacheUsage,
    runtime::RuntimeHandle,
    storage::PollActivityMarker,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentStatus, AuditEvent, AuthorityClass, ClosureOutcome, ControlAction,
        FailureArtifact, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, TaskOutputSnapshot, TaskRecord, TaskStatus, TokenUsage,
        ToolExecutionRecord, ToolExecutionStatus, WaitingReason,
    },
};

const RUN_POLL_INTERVAL_MS: u64 = 100;
const RUN_QUIESCENCE_WINDOW_MS: u64 = 350;
const RUN_STOP_SETTLE_TIMEOUT_MS: u64 = 2_000;
const RUN_STOP_SETTLE_MIN_PER_TASK_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct RunOnceRequest {
    pub text: String,
    pub authority_class: AuthorityClass,
    pub agent_id: Option<String>,
    pub create_agent: bool,
    pub template: Option<String>,
    pub max_turns: Option<u64>,
    pub wait_for_tasks: bool,
    pub workspace_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFinalStatus {
    Completed,
    Waiting,
    Failed,
    MaxTurnsExceeded,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunTaskSummary {
    pub task: TaskOutputSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<RunWorktreeSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunWorktreeSummary {
    pub worktree_path: String,
    pub worktree_branch: String,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_for_review: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_cleaned_up: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunProviderCacheUsage {
    pub read_input_tokens: u64,
    pub creation_input_tokens: u64,
    pub cacheable_input_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOnceResponse {
    pub agent_id: String,
    pub final_status: RunFinalStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    pub final_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_final_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_artifact: Option<FailureArtifact>,
    pub tasks: Vec<RunTaskSummary>,
    pub message_count: usize,
    pub changed_files: Vec<String>,
    pub token_usage: TokenUsage,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_cache_usage: Option<RunProviderCacheUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_model: Option<ModelRef>,
    pub fallback_active: bool,
    pub model_rounds: u64,
    pub tool_calls: usize,
    pub shell_commands: usize,
    pub exec_command_items: usize,
    pub batched_exec_command_items: usize,
}

#[derive(Debug, Default)]
struct RunBaseline {
    task_ids: HashSet<String>,
    tool_ids: HashSet<String>,
    message_ids: HashSet<String>,
    event_ids: HashSet<String>,
    delivery_summary_ids: HashSet<String>,
    brief_ids: HashSet<String>,
    turn_index: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_model_rounds: u64,
}

impl RunOnceResponse {
    pub fn render_text(&self) -> String {
        let mut sections = Vec::new();

        if !self.final_text.trim().is_empty() {
            sections.push(self.final_text.trim().to_string());
        }

        if self.final_status != RunFinalStatus::Completed {
            sections.push(format!(
                "Run status: {}",
                final_status_label(self.final_status)
            ));
        }

        if let Some(reason) = self.waiting_reason {
            sections.push(format!("Waiting reason: {}", waiting_reason_label(reason)));
        }

        if let Some(reason) = self.sleep_reason.as_ref() {
            sections.push(format!("Sleep reason: {reason}"));
        }

        if !self.token_usage.is_zero() {
            sections.push(format!(
                "Token usage: input {}, output {}, total {}",
                self.token_usage.input_tokens,
                self.token_usage.output_tokens,
                self.token_usage.total_tokens
            ));
        }

        if let Some(cache_usage) = self.provider_cache_usage.as_ref() {
            let hit_rate = cache_usage
                .hit_rate
                .map(|value| format!("{:.1}%", value * 100.0))
                .unwrap_or_else(|| "n/a".to_string());
            sections.push(format!(
                "Provider cache: read {}, created {}, hit rate {}",
                cache_usage.read_input_tokens, cache_usage.creation_input_tokens, hit_rate
            ));
        }

        if let Some(active_model) = self.active_model.as_ref() {
            let mut line = format!("Model: {}", active_model.as_string());
            if let Some(requested_model) = self.requested_model.as_ref() {
                if requested_model != active_model {
                    line.push_str(&format!(" (requested {})", requested_model.as_string()));
                }
            }
            sections.push(line);
        }

        if !self.tasks.is_empty() {
            let task_lines = self
                .tasks
                .iter()
                .map(|task| {
                    let mut line = format!(
                        "- [{}] {}",
                        task_status_label(&task.task.status),
                        task.task.summary.as_deref().unwrap_or(&task.task.kind)
                    );
                    if let Some(worktree) = task.worktree.as_ref() {
                        line.push_str(&format!(" ({})", worktree.worktree_path));
                    }
                    line
                })
                .collect::<Vec<_>>();
            sections.push(format!("Tasks:\n{}", task_lines.join("\n")));
        }

        if sections.is_empty() {
            "Run completed without additional output.".to_string()
        } else {
            sections.join("\n\n")
        }
    }
}

pub async fn run_once(config: AppConfig, request: RunOnceRequest) -> Result<RunOnceResponse> {
    std::fs::create_dir_all(config.agent_root_dir())?;
    std::fs::create_dir_all(config.run_dir())?;
    let host = RuntimeHost::new(config)?;
    run_once_with_host(host, request).await
}

pub async fn run_once_with_host(
    host: RuntimeHost,
    request: RunOnceRequest,
) -> Result<RunOnceResponse> {
    let session = prepare_run_session(&host, &request).await?;
    bind_run_workspace(&host, &session.runtime, &request, session.is_persistent).await?;
    let baseline =
        capture_baseline(&session.runtime, &session.runtime.agent_state().await?).await?;

    let inbound = InboundRequest {
        agent_id: session.agent_id.clone(),
        kind: MessageKind::OperatorPrompt,
        priority: Priority::Normal,
        origin: MessageOrigin::Operator {
            actor_id: Some("holon_run".into()),
        },
        authority_class: request.authority_class.clone(),
        body: MessageBody::Text {
            text: request.text.clone(),
        },
        delivery_surface: MessageDeliverySurface::RunOnce,
        admission_context: AdmissionContext::LocalProcess,
        metadata: None,
        correlation_id: None,
        causation_id: None,
    };
    let message = inbound.into_message();
    let queued_message = session.runtime.enqueue(message).await?;

    let mut candidate_completion: Option<CandidateCompletion> = None;
    let mut observed_new_task_ids = HashSet::<String>::new();
    let final_candidate = loop {
        let state = session.runtime.agent_state().await?;
        let storage_marker = session.runtime.poll_activity_marker()?;
        let poll_view =
            collect_run_poll_view(&session.runtime, &baseline, &state, storage_marker.clone())
                .await?;
        observed_new_task_ids.extend(poll_view.new_task_ids.iter().cloned());
        let active_new_task_ids =
            active_new_task_ids(&session.runtime, &poll_view.new_task_ids).await?;
        let foreground_idle = state.current_run_id.is_none() && state.pending == 0;
        let max_turns_hit = request.max_turns.is_some_and(|max| {
            state
                .total_model_rounds
                .saturating_sub(baseline.total_model_rounds)
                >= max
        });
        let terminal_within_max_turns = request.max_turns.is_none_or(|max| {
            state
                .total_model_rounds
                .saturating_sub(baseline.total_model_rounds)
                <= max
        });

        let terminal_status = if foreground_idle && poll_view.turn_terminal_observed {
            session.runtime.current_closure().await?.map(|closure| {
                (
                    match closure.outcome {
                        ClosureOutcome::Completed => RunFinalStatus::Completed,
                        ClosureOutcome::Continuable => RunFinalStatus::Waiting,
                        ClosureOutcome::Failed => RunFinalStatus::Failed,
                        ClosureOutcome::Waiting => RunFinalStatus::Waiting,
                    },
                    closure.waiting_reason,
                )
            })
        } else {
            None
        };
        let terminal_final_text_observed = state
            .last_turn_terminal
            .as_ref()
            .filter(|record| record.turn_index > baseline.turn_index)
            .and_then(|record| record.last_assistant_message.as_ref())
            .is_some_and(|text| !text.trim().is_empty());

        let waiting_for_active_tasks = request.wait_for_tasks && !active_new_task_ids.is_empty();
        let candidate_status = if poll_view.runtime_error && foreground_idle {
            Some((RunFinalStatus::Failed, None))
        } else if terminal_within_max_turns
            && (matches!(terminal_status, Some((RunFinalStatus::Failed, _)))
                || (terminal_final_text_observed
                    && matches!(terminal_status, Some((RunFinalStatus::Completed, _)))))
        {
            if waiting_for_active_tasks {
                None
            } else {
                terminal_status
            }
        } else if max_turns_hit && foreground_idle {
            if waiting_for_active_tasks {
                None
            } else {
                Some((RunFinalStatus::MaxTurnsExceeded, None))
            }
        } else if terminal_status.is_some() {
            if waiting_for_active_tasks {
                None
            } else {
                terminal_status
            }
        } else {
            None
        };

        if let Some((status, waiting_reason)) = candidate_status {
            if let Some(candidate) = candidate_completion.as_ref() {
                if candidate.state == status
                    && candidate.waiting_reason == waiting_reason
                    && candidate.activity_signature == poll_view.activity_signature
                    && candidate.observed_at.elapsed()
                        >= Duration::from_millis(RUN_QUIESCENCE_WINDOW_MS)
                {
                    break candidate.clone();
                }
            }
            let should_reset_candidate = candidate_completion.as_ref().is_none_or(|candidate| {
                candidate.state != status
                    || candidate.waiting_reason != waiting_reason
                    || candidate.activity_signature != poll_view.activity_signature
            });
            if should_reset_candidate {
                candidate_completion = Some(CandidateCompletion::new(
                    status,
                    waiting_reason,
                    poll_view.activity_signature.clone(),
                ));
            }
        } else {
            candidate_completion = None;
        }

        tokio::time::sleep(Duration::from_millis(RUN_POLL_INTERVAL_MS)).await;
    };
    let final_status = final_candidate.state;
    let waiting_reason = final_candidate.waiting_reason;
    let mut final_state = session.runtime.agent_state().await?;

    let pre_cleanup_view = collect_run_poll_view(
        &session.runtime,
        &baseline,
        &final_state,
        session.runtime.poll_activity_marker()?,
    )
    .await?;
    observed_new_task_ids.extend(pre_cleanup_view.new_task_ids.iter().cloned());
    let active_tasks =
        active_new_task_ids(&session.runtime, &pre_cleanup_view.new_task_ids).await?;
    for task_id in &active_tasks {
        let _ = session
            .runtime
            .stop_task(task_id, &request.authority_class)
            .await;
    }
    settle_stopped_tasks(&session.runtime, &active_tasks, &request.authority_class).await?;
    final_state = session.runtime.agent_state().await?;

    let final_view = collect_run_view(&session.runtime, &baseline, &observed_new_task_ids).await?;
    let response = build_response(
        &session.runtime,
        &baseline,
        &queued_message,
        final_status,
        waiting_reason,
        final_state,
        final_view,
    )
    .await;

    match session.cleanup {
        RunSessionCleanup::Temporary { runtime_task } => {
            let _ = session.runtime.control(ControlAction::Stop).await;
            let _ = runtime_task.await;
            let data_dir = host.agent_data_dir(&session.agent_id);
            if data_dir.exists() {
                let _ = std::fs::remove_dir_all(&data_dir);
            }
        }
        RunSessionCleanup::Persistent => {
            host.unload_runtime(&session.agent_id).await;
        }
    }

    response
}

struct RunSession {
    agent_id: String,
    runtime: RuntimeHandle,
    cleanup: RunSessionCleanup,
    is_persistent: bool,
}

enum RunSessionCleanup {
    Temporary { runtime_task: JoinHandle<()> },
    Persistent,
}

async fn prepare_run_session(host: &RuntimeHost, request: &RunOnceRequest) -> Result<RunSession> {
    if request.template.is_some() && !request.create_agent {
        bail!("template requires create_agent=true");
    }

    if let Some(agent_id) = request.agent_id.as_deref() {
        if request.create_agent {
            host.create_named_agent(agent_id, request.template.as_deref())
                .await?;
        }
        let runtime = host.get_public_agent_for_external_ingress(agent_id).await?;
        Ok(RunSession {
            agent_id: agent_id.to_string(),
            runtime,
            cleanup: RunSessionCleanup::Persistent,
            is_persistent: true,
        })
    } else {
        let (agent_id, runtime, runtime_task) = host.spawn_temporary_runtime("run")?;
        Ok(RunSession {
            agent_id,
            runtime,
            cleanup: RunSessionCleanup::Temporary { runtime_task },
            is_persistent: false,
        })
    }
}

async fn bind_run_workspace(
    host: &RuntimeHost,
    runtime: &RuntimeHandle,
    request: &RunOnceRequest,
    preserve_existing_session: bool,
) -> Result<()> {
    let existing_state = runtime.agent_state().await?;
    let should_preserve_existing_workspace = preserve_existing_session
        && request.workspace_root.is_none()
        && request.cwd.is_none()
        && existing_state.active_workspace_entry.is_some();
    if should_preserve_existing_workspace {
        return Ok(());
    }

    let workspace_anchor = request
        .workspace_root
        .clone()
        .unwrap_or_else(|| host.config().workspace_dir.clone());
    let workspace = host.ensure_workspace_entry(workspace_anchor.clone())?;
    runtime.attach_workspace(&workspace).await?;
    let selected_cwd = request.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .filter(|cwd| cwd.starts_with(&workspace_anchor))
            .unwrap_or(workspace_anchor.clone())
    });
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(selected_cwd),
            None,
        )
        .await?;
    Ok(())
}

async fn capture_baseline(
    runtime: &RuntimeHandle,
    state: &crate::types::AgentState,
) -> Result<RunBaseline> {
    Ok(RunBaseline {
        task_ids: runtime
            .latest_task_records_snapshot()
            .await?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        tool_ids: runtime
            .all_tool_executions()?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        message_ids: runtime
            .all_messages()?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        event_ids: runtime
            .all_events()?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        delivery_summary_ids: runtime
            .storage()
            .read_recent_delivery_summaries(usize::MAX)?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        brief_ids: runtime
            .storage()
            .read_recent_briefs(usize::MAX)?
            .into_iter()
            .map(|item| item.id)
            .collect(),
        turn_index: state.turn_index,
        total_input_tokens: state.total_input_tokens,
        total_output_tokens: state.total_output_tokens,
        total_model_rounds: state.total_model_rounds,
    })
}

struct RunView {
    new_tasks: Vec<TaskRecord>,
    new_tools: Vec<ToolExecutionRecord>,
    new_messages: Vec<MessageEnvelope>,
    new_events: Vec<AuditEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PollActivitySignature {
    storage_marker: PollActivityMarker,
    agent_status: AgentStatus,
    current_run_id: Option<String>,
    pending: usize,
    turn_index: u64,
    last_terminal_turn_index: Option<u64>,
}

#[derive(Debug, Clone)]
struct CandidateCompletion {
    state: RunFinalStatus,
    waiting_reason: Option<WaitingReason>,
    activity_signature: PollActivitySignature,
    observed_at: Instant,
}

impl CandidateCompletion {
    fn new(
        state: RunFinalStatus,
        waiting_reason: Option<WaitingReason>,
        activity_signature: PollActivitySignature,
    ) -> Self {
        Self {
            state,
            waiting_reason,
            activity_signature,
            observed_at: Instant::now(),
        }
    }
}

#[derive(Clone)]
struct RunPollView {
    new_task_ids: Vec<String>,
    turn_terminal_observed: bool,
    runtime_error: bool,
    activity_signature: PollActivitySignature,
}

async fn collect_run_poll_view(
    runtime: &RuntimeHandle,
    baseline: &RunBaseline,
    state: &crate::types::AgentState,
    storage_marker: PollActivityMarker,
) -> Result<RunPollView> {
    let events = runtime.all_events()?;
    let latest_tasks = runtime.latest_task_records_snapshot().await?;
    let new_tools = runtime
        .all_tool_executions()?
        .into_iter()
        .filter(|tool| !baseline.tool_ids.contains(&tool.id))
        .collect::<Vec<_>>();
    let runtime_error = events
        .iter()
        .rev()
        .take_while(|event| !baseline.event_ids.contains(&event.id))
        .any(|event| event.kind == "runtime_error");

    let mut new_task_ids = latest_tasks
        .iter()
        .filter(|task| !baseline.task_ids.contains(&task.id))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    new_task_ids.extend(new_tools.iter().filter_map(exec_command_task_id));
    new_task_ids.extend(
        events
            .iter()
            .filter(|event| !baseline.event_ids.contains(&event.id))
            .filter_map(run_event_task_id),
    );
    new_task_ids.sort();
    new_task_ids.dedup();

    let activity_signature = PollActivitySignature {
        storage_marker,
        agent_status: state.status.clone(),
        current_run_id: state.current_run_id.clone(),
        pending: state.pending,
        turn_index: state.turn_index,
        last_terminal_turn_index: state
            .last_turn_terminal
            .as_ref()
            .map(|record| record.turn_index),
    };

    Ok(RunPollView {
        new_task_ids,
        turn_terminal_observed: state
            .last_turn_terminal
            .as_ref()
            .is_some_and(|record| record.turn_index > baseline.turn_index),
        runtime_error,
        activity_signature,
    })
}

async fn collect_run_view(
    runtime: &RuntimeHandle,
    baseline: &RunBaseline,
    observed_task_ids: &HashSet<String>,
) -> Result<RunView> {
    let new_messages = runtime
        .all_messages()?
        .into_iter()
        .filter(|message| !baseline.message_ids.contains(&message.id))
        .collect::<Vec<_>>();
    let new_events = runtime
        .all_events()?
        .into_iter()
        .filter(|event| !baseline.event_ids.contains(&event.id))
        .collect::<Vec<_>>();
    let mut task_ids = observed_task_ids.clone();
    task_ids.extend(new_messages.iter().filter_map(message_task_id));

    let new_tools = runtime
        .all_tool_executions()?
        .into_iter()
        .filter(|tool| !baseline.tool_ids.contains(&tool.id))
        .collect::<Vec<_>>();
    task_ids.extend(new_tools.iter().filter_map(exec_command_task_id));
    task_ids.extend(new_events.iter().filter_map(run_event_task_id));

    let mut tasks_by_id = runtime
        .latest_task_records_snapshot()
        .await?
        .into_iter()
        .filter(|task| !baseline.task_ids.contains(&task.id) || task_ids.contains(&task.id))
        .map(|task| (task.id.clone(), task))
        .collect::<HashMap<_, _>>();
    for task_id in &task_ids {
        if baseline.task_ids.contains(task_id) || tasks_by_id.contains_key(task_id) {
            continue;
        }
        if let Some(task) = runtime.task_record(task_id).await? {
            tasks_by_id.insert(task.id.clone(), task);
        }
    }
    let mut new_tasks = tasks_by_id
        .into_values()
        .filter(|task| !baseline.task_ids.contains(&task.id))
        .collect::<Vec<_>>();
    new_tasks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(RunView {
        new_tasks,
        new_tools,
        new_messages,
        new_events,
    })
}

fn message_task_id(message: &MessageEnvelope) -> Option<String> {
    message.task_id.clone().or_else(|| {
        message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_id"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    })
}

fn exec_command_task_id(tool: &ToolExecutionRecord) -> Option<String> {
    if tool.tool_name != "ExecCommand" || tool.status != ToolExecutionStatus::Success {
        return None;
    }

    exec_command_result_value(&tool.output)
        .and_then(|value| value.get("task_handle"))
        .and_then(|value| value.get("task_id"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn exec_command_event_task_id(event: &AuditEvent) -> Option<String> {
    if event.kind != "tool_executed"
        || event.data.get("tool_name").and_then(|value| value.as_str()) != Some("ExecCommand")
        || event
            .data
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| status != "success")
    {
        return None;
    }

    event
        .data
        .get("task_handle")
        .and_then(|value| value.get("task_id"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn process_execution_event_task_id(event: &AuditEvent) -> Option<String> {
    if event.kind != "process_execution_requested"
        || event.data.get("surface").and_then(|value| value.as_str()) != Some("command_task")
        || event
            .data
            .get("promoted_from_exec_command")
            .and_then(|value| value.as_bool())
            != Some(true)
    {
        return None;
    }

    event
        .data
        .get("task_id")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn run_event_task_id(event: &AuditEvent) -> Option<String> {
    exec_command_event_task_id(event).or_else(|| process_execution_event_task_id(event))
}

fn exec_command_result_value(output: &serde_json::Value) -> Option<&serde_json::Value> {
    output
        .get("result")
        .or_else(|| output.get("envelope").and_then(|value| value.get("result")))
}

async fn active_new_task_ids(
    runtime: &crate::runtime::RuntimeHandle,
    task_ids: &[String],
) -> Result<Vec<String>> {
    let mut active = Vec::new();
    for task_id in task_ids {
        let snapshot = runtime.task_output(task_id, false, 0).await?;
        if crate::storage::is_active_task_status(&snapshot.task.status) {
            active.push(task_id.clone());
        }
    }
    Ok(active)
}

async fn settle_stopped_tasks(
    runtime: &crate::runtime::RuntimeHandle,
    task_ids: &[String],
    authority_class: &AuthorityClass,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(RUN_STOP_SETTLE_TIMEOUT_MS);
    for (index, task_id) in task_ids.iter().enumerate() {
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let snapshot = runtime.task_output(task_id, false, 0).await?;
            if !crate::storage::is_active_task_status(&snapshot.task.status) {
                break;
            }

            let _ = runtime.stop_task(task_id, authority_class).await;

            let tasks_left = task_ids.len().saturating_sub(index).max(1) as u128;
            let remaining_ms_u128 = remaining.as_millis();
            let fair_share_ms = (remaining_ms_u128 / tasks_left)
                .max(RUN_STOP_SETTLE_MIN_PER_TASK_MS as u128)
                .min(remaining_ms_u128);
            let remaining_ms = fair_share_ms.min(u64::MAX as u128) as u64;
            if remaining_ms == 0 {
                break;
            }

            let waited = runtime.task_output(task_id, true, remaining_ms).await?;
            if !crate::storage::is_active_task_status(&waited.task.status) {
                break;
            }
        }
    }
    Ok(())
}

async fn build_response(
    runtime: &crate::runtime::RuntimeHandle,
    baseline: &RunBaseline,
    queued_message: &MessageEnvelope,
    final_status: RunFinalStatus,
    waiting_reason: Option<WaitingReason>,
    final_state: crate::types::AgentState,
    view: RunView,
) -> Result<RunOnceResponse> {
    let sleep_reason = view
        .new_tools
        .iter()
        .rev()
        .find(|tool| {
            tool.tool_name == "Sleep" && matches!(tool.status, ToolExecutionStatus::Success)
        })
        .and_then(|tool| {
            tool.output
                .get("envelope")
                .and_then(|value| value.get("result"))
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str())
                .or_else(|| {
                    tool.output
                        .get("envelope")
                        .and_then(|value| value.get("summary_text"))
                        .and_then(|value| value.as_str())
                })
        })
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(ToString::to_string);

    let workspace_root = runtime.workspace_root();
    let mut changed_files = changed_files_from_tools(&view.new_tools, &workspace_root);
    let mut task_summaries = Vec::new();
    for task in &view.new_tasks {
        let output = runtime.task_output(&task.id, false, 0).await?;
        let worktree =
            worktree_summary_for_task(task.id.as_str(), &view.new_messages).or_else(|| {
                output
                    .task
                    .child_supervision
                    .as_ref()
                    .and_then(|child| child.worktree.as_ref())
                    .map(worktree_summary_from_projection)
            });
        if let Some(worktree) = worktree.as_ref() {
            changed_files.extend(worktree.changed_files.iter().cloned());
        }
        task_summaries.push(RunTaskSummary {
            task: output.task,
            worktree,
        });
    }
    changed_files.sort();
    changed_files.dedup();

    let token_usage = TokenUsage::new(
        final_state
            .total_input_tokens
            .saturating_sub(baseline.total_input_tokens),
        final_state
            .total_output_tokens
            .saturating_sub(baseline.total_output_tokens),
    );
    let provider_cache_usage = aggregate_provider_cache_usage(&view.new_events);
    let failure_artifact = if final_status == RunFinalStatus::Failed {
        final_state
            .last_runtime_failure
            .as_ref()
            .and_then(|failure| failure.failure_artifact.clone())
            .or_else(|| {
                task_summaries
                    .iter()
                    .rev()
                    .find(|task| {
                        matches!(
                            task.task.status,
                            TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
                        )
                    })
                    .and_then(|task| task.task.failure_artifact.clone())
            })
    } else {
        None
    };
    let raw_final_text = raw_final_text(&final_state, baseline, final_status);
    let new_briefs = runtime
        .storage()
        .read_recent_briefs(usize::MAX)?
        .into_iter()
        .filter(|brief| !baseline.brief_ids.contains(&brief.id))
        .collect::<Vec<_>>();
    let new_brief_by_id = new_briefs
        .iter()
        .map(|brief| (brief.id.as_str(), brief))
        .collect::<HashMap<_, _>>();
    let promoted_completion_report_text = view
        .new_events
        .iter()
        .filter(|event| event.kind == "work_item_completion_report_promoted")
        .filter_map(|event| event.data["brief_id"].as_str())
        .filter_map(|id| new_brief_by_id.get(id))
        .last()
        .map(|brief| brief.text.trim().to_string())
        .filter(|text| !text.is_empty());
    let new_delivery_summaries = runtime
        .storage()
        .read_recent_delivery_summaries(usize::MAX)?
        .into_iter()
        .filter(|summary| !baseline.delivery_summary_ids.contains(&summary.id))
        .collect::<Vec<_>>();
    let new_delivery_summary_by_id = new_delivery_summaries
        .iter()
        .map(|summary| (summary.id.as_str(), summary))
        .collect::<HashMap<_, _>>();
    let legacy_promoted_completion_report_text = view
        .new_events
        .iter()
        .filter(|event| event.kind == "work_item_completion_report_promoted")
        .filter_map(|event| event.data["delivery_summary_id"].as_str())
        .filter_map(|id| new_delivery_summary_by_id.get(id))
        .last()
        .map(|summary| summary.text.trim().to_string())
        .filter(|text| !text.is_empty());
    let delivery_summary_text = promoted_completion_report_text
        .or(legacy_promoted_completion_report_text)
        .or_else(|| {
            new_delivery_summaries
                .last()
                .map(|summary| summary.text.trim().to_string())
                .filter(|text| !text.is_empty())
        });
    let final_text = delivery_summary_text
        .or_else(|| raw_final_text.clone())
        .unwrap_or_default();
    let (requested_model, active_model, fallback_active) = latest_run_model_state(
        &view.new_events,
        final_state.last_requested_model.clone(),
        final_state.last_active_model.clone(),
    );

    Ok(RunOnceResponse {
        agent_id: queued_message.agent_id.clone(),
        final_status,
        waiting_reason,
        final_text,
        raw_final_text,
        sleep_reason,
        failure_artifact,
        tasks: task_summaries,
        message_count: view.new_messages.len(),
        changed_files,
        input_tokens: token_usage.input_tokens,
        output_tokens: token_usage.output_tokens,
        provider_cache_usage,
        requested_model,
        active_model,
        fallback_active,
        token_usage,
        model_rounds: final_state
            .total_model_rounds
            .saturating_sub(baseline.total_model_rounds),
        tool_calls: view.new_tools.len(),
        shell_commands: view
            .new_tools
            .iter()
            .filter(|tool| tool.tool_name == "ExecCommand")
            .count()
            + batched_exec_command_items(&view.new_tools),
        exec_command_items: view
            .new_tools
            .iter()
            .filter(|tool| tool.tool_name == "ExecCommand")
            .count()
            + batched_exec_command_items(&view.new_tools),
        batched_exec_command_items: batched_exec_command_items(&view.new_tools),
    })
}

fn aggregate_provider_cache_usage(events: &[AuditEvent]) -> Option<RunProviderCacheUsage> {
    let mut usage = ProviderCacheUsage {
        read_input_tokens: 0,
        creation_input_tokens: 0,
    };
    let mut saw_usage = false;

    for event in events
        .iter()
        .filter(|event| event.kind == "provider_round_completed")
    {
        let Some(cache_usage) = event.data.get("provider_cache_usage") else {
            continue;
        };
        if cache_usage.is_null() {
            continue;
        }
        let read_input_tokens = cache_usage
            .get("read_input_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let creation_input_tokens = cache_usage
            .get("creation_input_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        saw_usage = true;
        usage.read_input_tokens = usage.read_input_tokens.saturating_add(read_input_tokens);
        usage.creation_input_tokens = usage
            .creation_input_tokens
            .saturating_add(creation_input_tokens);
    }

    if !saw_usage {
        return None;
    }

    let cacheable_input_tokens = usage
        .read_input_tokens
        .saturating_add(usage.creation_input_tokens);
    let hit_rate = if cacheable_input_tokens == 0 {
        None
    } else {
        Some(usage.read_input_tokens as f64 / cacheable_input_tokens as f64)
    };

    Some(RunProviderCacheUsage {
        read_input_tokens: usage.read_input_tokens,
        creation_input_tokens: usage.creation_input_tokens,
        cacheable_input_tokens,
        hit_rate,
    })
}

fn batched_exec_command_items(tools: &[ToolExecutionRecord]) -> usize {
    tools
        .iter()
        .filter(|tool| tool.tool_name == "ExecCommandBatch")
        .map(|tool| {
            let result = tool
                .output
                .get("envelope")
                .and_then(|value| value.get("result"));
            let completed_count = result
                .and_then(|value| value.get("completed_count"))
                .and_then(|value| value.as_u64())
                .map(|value| value as usize)
                .unwrap_or(0);
            let failed_count = result
                .and_then(|value| value.get("failed_count"))
                .and_then(|value| value.as_u64())
                .map(|value| value as usize)
                .unwrap_or(0);
            completed_count + failed_count
        })
        .sum()
}

fn latest_run_model_state(
    events: &[AuditEvent],
    fallback_requested_model: Option<ModelRef>,
    fallback_active_model: Option<ModelRef>,
) -> (Option<ModelRef>, Option<ModelRef>, bool) {
    let mut requested_model = fallback_requested_model;
    let mut active_model = fallback_active_model;
    let mut fallback_active = requested_model
        .as_ref()
        .zip(active_model.as_ref())
        .is_some_and(|(requested, active)| requested != active);

    for event in events
        .iter()
        .filter(|event| event.kind == "provider_round_completed")
    {
        requested_model = event
            .data
            .get("requested_model")
            .and_then(|value| value.as_str())
            .and_then(|value| ModelRef::parse(value).ok())
            .or(requested_model);
        active_model = event
            .data
            .get("active_model")
            .and_then(|value| value.as_str())
            .and_then(|value| ModelRef::parse(value).ok())
            .or(active_model);
        fallback_active = event
            .data
            .get("fallback_active")
            .and_then(|value| value.as_bool())
            .unwrap_or_else(|| {
                requested_model
                    .as_ref()
                    .zip(active_model.as_ref())
                    .is_some_and(|(requested, active)| requested != active)
            });
    }

    (requested_model, active_model, fallback_active)
}

fn raw_final_text(
    final_state: &crate::types::AgentState,
    baseline: &RunBaseline,
    final_status: RunFinalStatus,
) -> Option<String> {
    final_state
        .last_turn_terminal
        .as_ref()
        .filter(|record| record.turn_index > baseline.turn_index)
        .and_then(|record| record.last_assistant_message.clone())
        .or_else(|| {
            if final_status == RunFinalStatus::Failed {
                final_state
                    .last_runtime_failure
                    .as_ref()
                    .map(|failure| failure.summary.clone())
            } else {
                None
            }
        })
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn worktree_summary_for_task(
    task_id: &str,
    messages: &[MessageEnvelope],
) -> Option<RunWorktreeSummary> {
    let worktree = messages
        .iter()
        .filter(|message| matches!(message.kind, MessageKind::TaskResult))
        .filter(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("task_id"))
                .and_then(|value| value.as_str())
                == Some(task_id)
        })
        .filter_map(|message| message.metadata.as_ref())
        .find_map(|metadata| metadata.get("worktree"))?;
    Some(RunWorktreeSummary {
        worktree_path: worktree
            .get("worktree_path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        worktree_branch: worktree
            .get("worktree_branch")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        changed_files: worktree
            .get("changed_files")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .map(ToString::to_string)
            .collect(),
        retained_for_review: worktree
            .get("retained_for_review")
            .and_then(|value| value.as_bool()),
        auto_cleaned_up: worktree
            .get("auto_cleaned_up")
            .and_then(|value| value.as_bool()),
    })
}

fn worktree_summary_from_projection(
    worktree: &crate::types::ChildSupervisionWorktreeProjection,
) -> RunWorktreeSummary {
    RunWorktreeSummary {
        worktree_path: worktree.worktree_path.clone().unwrap_or_default(),
        worktree_branch: worktree.worktree_branch.clone().unwrap_or_default(),
        changed_files: worktree.changed_files.clone(),
        retained_for_review: worktree.retained_for_review,
        auto_cleaned_up: worktree.auto_cleaned_up,
    }
}

fn changed_files_from_tools(tools: &[ToolExecutionRecord], workspace_root: &Path) -> Vec<String> {
    let mut changed = Vec::new();
    for tool in tools {
        match tool.tool_name.as_str() {
            "ApplyPatch" => {
                let patch_input = tool
                    .input
                    .as_str()
                    .or_else(|| tool.input.get("patch").and_then(|value| value.as_str()));
                if let Some(input) = patch_input {
                    let parsed = extract_patch_files(input);
                    if !parsed.is_empty() {
                        changed.extend(parsed);
                        continue;
                    }
                }
                if let Some(paths) = tool
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
                    .and_then(|value| value.get("changed_paths"))
                    .and_then(|value| value.as_array())
                {
                    changed.extend(
                        paths
                            .iter()
                            .filter_map(|path| path.as_str())
                            .map(|path| normalize_workspace_relative_path(path, workspace_root)),
                    );
                }
            }
            _ => {}
        }
    }
    changed
}

fn extract_patch_files(input: &str) -> Vec<String> {
    let mut changed = Vec::new();
    let mut pending_rename_from: Option<String> = None;
    let lines = input.lines().collect::<Vec<_>>();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("rename from ") {
            pending_rename_from = Some(strip_diff_prefix(path).to_string());
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("rename to ") {
            if let Some(from) = pending_rename_from.take() {
                push_unique(&mut changed, from);
                push_unique(&mut changed, strip_diff_prefix(path).to_string());
            }
            index += 1;
            continue;
        }
        if let Some(old_path) = line.strip_prefix("--- ") {
            if index + 1 < lines.len() {
                if let Some(new_path) = lines[index + 1].strip_prefix("+++ ") {
                    let old_path = strip_diff_prefix(old_path);
                    let new_path = strip_diff_prefix(new_path);
                    if old_path != "/dev/null" {
                        push_unique(&mut changed, old_path.to_string());
                    }
                    if new_path != "/dev/null" && new_path != old_path {
                        push_unique(&mut changed, new_path.to_string());
                    }
                    index += 2;
                    continue;
                }
            }
        }
        index += 1;
    }

    changed
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

fn normalize_workspace_relative_path(path: &str, workspace_root: &Path) -> String {
    let candidate = Path::new(path);
    if candidate.is_relative() {
        return path.to_string();
    }

    candidate
        .strip_prefix(workspace_root)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

fn final_status_label(state: RunFinalStatus) -> &'static str {
    match state {
        RunFinalStatus::Completed => "completed",
        RunFinalStatus::Waiting => "waiting",
        RunFinalStatus::Failed => "failed",
        RunFinalStatus::MaxTurnsExceeded => "max_turns_exceeded",
    }
}

fn waiting_reason_label(reason: WaitingReason) -> &'static str {
    match reason {
        WaitingReason::AwaitingOperatorInput => "awaiting_operator_input",
        WaitingReason::AwaitingExternalChange => "awaiting_external_change",
        WaitingReason::AwaitingTaskResult => "awaiting_task_result",
        WaitingReason::AwaitingTimer => "awaiting_timer",
    }
}

fn task_status_label(state: &TaskStatus) -> &'static str {
    match state {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        context::ContextConfig,
        provider::test_support::ScriptedAgentProvider,
        runtime::InitialWorkspaceBinding,
        types::{AgentState, AuditEvent, TaskKind},
    };

    use super::*;

    fn test_runtime(data_dir: &Path) -> Result<RuntimeHandle> {
        RuntimeHandle::new(
            "default",
            data_dir.to_path_buf(),
            InitialWorkspaceBinding::Detached,
            "http://localhost".into(),
            Arc::new(ScriptedAgentProvider::new(Vec::new())),
            "default".into(),
            ContextConfig::default(),
        )
    }

    #[tokio::test]
    async fn poll_view_keeps_runtime_error_visible_after_many_events() -> Result<()> {
        let dir = tempdir()?;
        let runtime = test_runtime(dir.path())?;
        let state = AgentState::new("default");

        runtime
            .storage()
            .append_event(&AuditEvent::new("runtime_error", json!({})))?;
        for index in 0..129 {
            runtime
                .storage()
                .append_event(&AuditEvent::new("state_changed", json!({ "index": index })))?;
        }

        let view = collect_run_poll_view(
            &runtime,
            &RunBaseline::default(),
            &state,
            runtime.poll_activity_marker()?,
        )
        .await?;

        assert!(view.runtime_error);
        Ok(())
    }

    #[tokio::test]
    async fn poll_view_keeps_early_task_ids_visible_after_many_task_updates() -> Result<()> {
        let dir = tempdir()?;
        let runtime = test_runtime(dir.path())?;
        let state = AgentState::new("default");

        runtime.storage().append_task(&task_record("early-task"))?;
        for index in 0..129 {
            runtime
                .storage()
                .append_task(&task_record(format!("later-task-{index}")))?;
        }

        let view = collect_run_poll_view(
            &runtime,
            &RunBaseline::default(),
            &state,
            runtime.poll_activity_marker()?,
        )
        .await?;

        assert!(view.new_task_ids.iter().any(|id| id == "early-task"));
        Ok(())
    }

    fn task_record(id: impl Into<String>) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }

    #[test]
    fn extract_patch_files_includes_move_source_and_destination() {
        let files = extract_patch_files(
            "diff --git a/src/old.rs b/src/new.rs\nrename from src/old.rs\nrename to src/new.rs\n--- a/src/old.rs\n+++ b/src/new.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        );
        assert_eq!(files, vec!["src/old.rs", "src/new.rs"]);
    }

    #[test]
    fn changed_files_from_tools_prefers_patch_input_and_normalizes_paths() {
        let workspace_root = Path::new("/tmp/workspace");
        let tool = ToolExecutionRecord {
            id: "tool-1".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ApplyPatch".into(),
            created_at: Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({
                "patch": "diff --git a/notes/result.txt b/notes/final.txt\nrename from notes/result.txt\nrename to notes/final.txt\n--- a/notes/result.txt\n+++ b/notes/final.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
            }),
            output: json!({
                "envelope": {
                    "result": {
                        "changed_paths": [
                            "/tmp/workspace/notes/result.txt",
                            "/tmp/workspace/notes/final.txt"
                        ]
                    }
                }
            }),
            summary: "patched".into(),
            invocation_surface: None,
        };

        let changed = changed_files_from_tools(&[tool], workspace_root);
        assert_eq!(changed, vec!["notes/result.txt", "notes/final.txt"]);
    }

    #[test]
    fn exec_command_task_id_reads_promoted_task_handle() {
        let tool = ToolExecutionRecord {
            id: "tool-command".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({}),
            output: json!({
                "envelope": {
                    "result": {
                        "disposition": "promoted_to_task",
                        "task_handle": {
                            "task_id": "task-promoted",
                            "kind": "command_task",
                            "status": "running"
                        }
                    }
                }
            }),
            summary: "command promoted".into(),
            invocation_surface: None,
        };

        assert_eq!(exec_command_task_id(&tool), Some("task-promoted".into()));
    }

    #[test]
    fn exec_command_task_id_reads_current_result_shape() {
        let tool = ToolExecutionRecord {
            id: "tool-command".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({}),
            output: json!({
                "result": {
                    "disposition": "promoted_to_task",
                    "task_handle": {
                        "task_id": "task-promoted",
                        "kind": "command_task",
                        "status": "running"
                    }
                }
            }),
            summary: "command promoted".into(),
            invocation_surface: None,
        };

        assert_eq!(exec_command_task_id(&tool), Some("task-promoted".into()));
    }

    #[test]
    fn exec_command_event_task_id_reads_tool_executed_task_handle() {
        let event = AuditEvent::new(
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "status": "success",
                "task_handle": {
                    "task_id": "task-promoted",
                    "task_kind": "command_task",
                    "status": "running"
                }
            }),
        );

        assert_eq!(
            exec_command_event_task_id(&event),
            Some("task-promoted".into())
        );
    }

    #[test]
    fn batched_exec_command_items_counts_batch_result_items() {
        let tool = ToolExecutionRecord {
            id: "tool-batch".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommandBatch".into(),
            created_at: Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({}),
            output: json!({
                "envelope": {
                    "result": {
                        "item_count": 4,
                        "completed_count": 2,
                        "failed_count": 1,
                        "rejected_count": 1,
                        "skipped_count": 0
                    }
                }
            }),
            summary: "batch".into(),
            invocation_surface: None,
        };

        assert_eq!(batched_exec_command_items(&[tool]), 3);
    }

    #[test]
    fn aggregate_provider_cache_usage_sums_round_cache_tokens() {
        let events = vec![
            AuditEvent::new(
                "provider_round_completed",
                json!({
                    "provider_cache_usage": {
                        "read_input_tokens": 300,
                        "creation_input_tokens": 100
                    }
                }),
            ),
            AuditEvent::new(
                "provider_round_completed",
                json!({
                    "provider_cache_usage": {
                        "read_input_tokens": 200,
                        "creation_input_tokens": 400
                    }
                }),
            ),
            AuditEvent::new("state_changed", json!({})),
        ];

        let usage = aggregate_provider_cache_usage(&events).expect("cache usage");

        assert_eq!(usage.read_input_tokens, 500);
        assert_eq!(usage.creation_input_tokens, 500);
        assert_eq!(usage.cacheable_input_tokens, 1000);
        assert_eq!(usage.hit_rate, Some(0.5));
    }
}
