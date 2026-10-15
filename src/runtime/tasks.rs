#[path = "tasks/work_planning.rs"]
mod work_planning;
use work_planning::{WorkCommandRead, WorkCommandReadMode};

use super::message_dispatch::message_text;
use super::waiting::WorkItemBlockerClearance;
use super::{task_state_reducer, *};
use crate::config::{ModelRef, ProviderId};
use crate::runtime_error::{
    sanitize_runtime_error_text, RuntimeError, RuntimeErrorContext, RuntimeErrorDomain,
};
use crate::tool::helpers::truncate_output_to_char_budget;
use crate::tool::ToolError;
use crate::types::{
    brief_created_event_for, AgentLifecycleDisposition, AgentModelRequest, AgentModelResolution,
    AgentModelResolutionStatus, BriefKind, BriefRecord, ChildAgentWorkspaceMode,
    CommandTaskStatusSnapshot, CompletionReportRequirement, CompletionReportState, FailureArtifact,
    FailureArtifactCategory, TaskInputResult, TaskKind, TaskListEntry, TaskOutputResult,
    TaskOutputRetrievalStatus, TaskOutputSnapshot, TaskStatusSnapshot, TodoItem, ToolArtifactRef,
    WaitConditionRecord, WaitConditionStatus, WorkItemCompletionIntent, WorkItemContinuationFrame,
    WorkItemContinuationReturnPolicy, WorkItemContinuationState, WorkItemPlanStatus,
    WorkItemReadiness, WorkItemRecord, WorkItemState, AGENT_MESSAGE_WAIT_TASK_KIND,
    CHILD_AGENT_TASK_KIND,
};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(super) enum ReplyReconciliation {
    Ready,
    Changed,
    WaitingForReceipt,
}

#[path = "task_output.rs"]
mod output;
pub(crate) use output::TaskOutputResultRead;

const TASK_OUTPUT_POLL_INTERVAL_MS: u64 = 100;
const TASK_OUTPUT_PREVIEW_CHAR_BUDGET: usize = 8_000;
const SPAWN_AGENT_TASK_LABEL_CHAR_BUDGET: usize = 120;

#[derive(Debug, Clone)]
enum WorkItemReadinessMutation {
    Pause(String),
    Resume,
}

#[derive(Debug, Clone)]
struct TaskMessageSnapshot {
    state: TaskStatus,
    text: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemFocusTransitionWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemFocusTransition {
    pub previous_work_item_id: Option<String>,
    pub current_work_item_id: String,
    pub reason: Option<String>,
    pub previous_readiness: Option<WorkItemReadiness>,
    pub current_readiness: WorkItemReadiness,
    pub switch_kind: String,
    pub terminal_transition: bool,
    pub current_focus_mode: String,
    pub blocker_cleared: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_wait_condition_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkItemFocusTransitionWarning>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemContinuationSummary {
    pub frame_id: String,
    pub suspended_work_item_id: String,
    pub active_work_item_id: String,
    pub return_policy: WorkItemContinuationReturnPolicy,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PickedWorkItem {
    pub previous_work_item: Option<WorkItemRecord>,
    pub current_work_item: WorkItemRecord,
    pub transition: WorkItemFocusTransition,
    pub continuation_created: Option<WorkItemContinuationSummary>,
    pub continuation_resolved: Option<WorkItemContinuationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContinuationPathTermination {
    ReachesCurrent,
    NoCurrentFocus,
    MissingLink {
        missing_work_item_id: String,
        originating_continuation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContinuationPathAnalysis {
    frames: Vec<WorkItemContinuationFrame>,
    termination: ContinuationPathTermination,
}

fn active_continuations_by_suspended<'a>(
    active_continuations: &[WorkItemContinuationFrame],
) -> Result<BTreeMap<&str, &WorkItemContinuationFrame>> {
    let mut by_suspended = BTreeMap::new();
    let mut by_active = BTreeMap::new();
    for frame in active_continuations {
        if let Some(existing) = by_suspended.insert(frame.suspended_work_item_id.as_str(), frame) {
            return Err(RuntimeError::validation(
                "continuation_topology_ambiguous",
                format!(
                    "active continuation topology has competing suspended edges {} and {} for work item {}",
                    existing.id, frame.id, frame.suspended_work_item_id
                ),
            )
            .into());
        }
        if let Some(existing) = by_active.insert(frame.active_work_item_id.as_str(), frame) {
            return Err(RuntimeError::validation(
                "continuation_topology_ambiguous",
                format!(
                    "active continuation topology has competing active edges {} and {} for work item {}",
                    existing.id, frame.id, frame.active_work_item_id
                ),
            )
            .into());
        }
    }
    Ok(by_suspended)
}

pub(super) fn analyze_continuation_path(
    target_work_item_id: &str,
    current_work_item_id: Option<&str>,
    active_continuations: &[WorkItemContinuationFrame],
) -> Result<ContinuationPathAnalysis> {
    let by_suspended = active_continuations_by_suspended(active_continuations)?;
    let mut frames: Vec<WorkItemContinuationFrame> = Vec::new();
    let mut visited = BTreeSet::new();
    let mut work_item_id = target_work_item_id;
    loop {
        let Some(frame) = by_suspended.get(work_item_id).copied() else {
            let originating = frames
                .last()
                .expect("continuation path starts from a yielded target");
            let termination = if current_work_item_id.is_none() {
                ContinuationPathTermination::NoCurrentFocus
            } else {
                ContinuationPathTermination::MissingLink {
                    missing_work_item_id: work_item_id.to_string(),
                    originating_continuation_id: originating.id.clone(),
                }
            };
            return Ok(ContinuationPathAnalysis {
                frames,
                termination,
            });
        };
        if !visited.insert(frame.id.clone()) {
            return Err(RuntimeError::validation(
                "continuation_topology_cycle",
                format!(
                    "active continuation topology contains a cycle at continuation {}",
                    frame.id
                ),
            )
            .into());
        }
        frames.push(frame.clone());
        if current_work_item_id == Some(frame.active_work_item_id.as_str()) {
            return Ok(ContinuationPathAnalysis {
                frames,
                termination: ContinuationPathTermination::ReachesCurrent,
            });
        }
        work_item_id = frame.active_work_item_id.as_str();
    }
}

#[derive(Debug, Clone)]
pub struct CompletedWorkItem {
    pub work_item: WorkItemRecord,
    pub continuation_resumed: Option<WorkItemContinuationSummary>,
}

fn child_agent_task_detail(workspace_mode: ChildAgentWorkspaceMode) -> serde_json::Value {
    serde_json::json!({
        "wait_policy": crate::types::TaskWaitPolicy::Background,
        "workspace_mode": workspace_mode,
        "lifecycle_disposition": AgentLifecycleDisposition::DeleteOnTerminal,
    })
}

const REJOIN_OBLIGATION_ID_DETAIL: &str = "rejoin_obligation_id";
const REJOIN_GENERATION_DETAIL: &str = "rejoin_generation";
const PARENT_TURN_ID_DETAIL: &str = "parent_turn_id";

fn copy_task_rejoin_contract(task: &TaskRecord, detail: &mut serde_json::Value) {
    let Some(target) = detail.as_object_mut() else {
        return;
    };
    let Some(source) = task.detail.as_ref().and_then(serde_json::Value::as_object) else {
        return;
    };
    for key in [
        REJOIN_OBLIGATION_ID_DETAIL,
        REJOIN_GENERATION_DETAIL,
        PARENT_TURN_ID_DETAIL,
    ] {
        if let Some(value) = source.get(key) {
            target.entry(key).or_insert_with(|| value.clone());
        }
    }
}

pub(super) fn task_rejoin_fence(
    task: &TaskRecord,
) -> Result<crate::domain::execution_protocol::RejoinFence> {
    task.rejoin_fence().map_err(anyhow::Error::msg)
}

/// The real creator may allocate obligation/generation before any parent Turn.
/// This shape carries no rejoin permission and borrows without allocation.
pub(super) fn unbound_task_rejoin_generation(task: &TaskRecord) -> Option<u64> {
    let detail = task.detail.as_ref()?.as_object()?;
    if detail.contains_key(PARENT_TURN_ID_DETAIL)
        || detail.get(REJOIN_OBLIGATION_ID_DETAIL)?.as_str()? != task.id
    {
        return None;
    }
    detail
        .get(REJOIN_GENERATION_DETAIL)?
        .as_u64()
        .filter(|generation| *generation > 0)
}

pub(super) fn spawn_agent_task_label(initial_message: &str) -> String {
    let collapsed = initial_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let label = if collapsed.is_empty() {
        "delegated child agent task".to_string()
    } else {
        collapsed
    };
    crate::tool::helpers::truncate_text(&label, SPAWN_AGENT_TASK_LABEL_CHAR_BUDGET)
}

fn inherited_model_parameters(
    model: &crate::types::AgentModelState,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut parameters = serde_json::Map::new();
    if let Some(reasoning_effort) = model.override_reasoning_effort.clone() {
        parameters.insert("reasoning_effort".into(), reasoning_effort.into());
    }
    (!parameters.is_empty()).then_some(parameters)
}

fn inherited_spawn_model_resolution(model: &crate::types::AgentModelState) -> AgentModelResolution {
    AgentModelResolution {
        requested: None,
        resolved_provider: model.effective_model.provider.as_str().to_string(),
        resolved_model: model.effective_model.model.clone(),
        resolved_parameters: inherited_model_parameters(model),
        resolution_status: AgentModelResolutionStatus::Inherited,
        policy_notes: Vec::new(),
    }
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn task_with_status(
    task: &TaskRecord,
    status: TaskStatus,
    detail: Option<serde_json::Value>,
) -> TaskRecord {
    TaskRecord {
        id: task.id.clone(),
        agent_id: task.agent_id.clone(),
        kind: task.kind.clone(),
        status,
        created_at: task.created_at,
        updated_at: Utc::now(),
        parent_message_id: task.parent_message_id.clone(),
        work_item_id: task.work_item_id.clone(),
        summary: task.summary.clone(),
        detail,
        recovery: task.recovery.clone(),
    }
}

pub(in crate::runtime) fn task_with_result_message(
    task: &TaskRecord,
    status: TaskStatus,
    mut detail: Option<serde_json::Value>,
    result_message: &MessageEnvelope,
) -> TaskRecord {
    {
        let detail = detail.get_or_insert_with(|| serde_json::json!({}));
        copy_task_rejoin_contract(task, detail);
        if let (Some(detail), Some(parent_turn_id)) =
            (detail.as_object_mut(), result_message.turn_id.as_ref())
        {
            detail
                .entry(PARENT_TURN_ID_DETAIL)
                .or_insert_with(|| serde_json::json!(parent_turn_id));
        }
    }
    let mut terminal_task = task_with_status(task, status, detail);
    terminal_task.parent_message_id = Some(result_message.id.clone());
    terminal_task
}

pub(super) fn restart_task_result_message_id(task_id: &str) -> String {
    format!("message:task-restart:{task_id}")
}

// Interruption observes the original Task. A Result's own synthetic Turn must
// not promote a real creator's parent-less obligation into a rejoin fence.
fn task_with_original_interrupted_result(
    task: &TaskRecord,
    result_message: &MessageEnvelope,
) -> TaskRecord {
    let mut terminal = task_with_result_message(
        task,
        TaskStatus::Interrupted,
        task.detail.clone(),
        result_message,
    );
    if unbound_task_rejoin_generation(task).is_some() {
        terminal
            .detail
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .expect("validated original unbound Task detail")
            .remove(PARENT_TURN_ID_DETAIL);
    }
    terminal
}

fn interrupted_task_result_message(
    task: &TaskRecord,
    interrupted_reason: &'static str,
) -> MessageEnvelope {
    let status_before = task
        .detail
        .as_ref()
        .and_then(|detail| {
            detail
                .get("status_before_restart")
                .or_else(|| detail.get("status_before_stop"))
        })
        .and_then(|value| value.as_str())
        .unwrap_or("running");
    let mut message = MessageEnvelope {
        id: if interrupted_reason == "runtime_restarted" {
            restart_task_result_message_id(&task.id)
        } else {
            crate::ids::message_id()
        },
        turn_id: Some(crate::ids::turn_id()),
        work_item_id: task.effective_work_item_id().map(ToString::to_string),
        metadata: Some(serde_json::json!({
            "task_id": task.id,
            "task_kind": task.kind,
            "task_status": "interrupted",
            "task_summary": task.summary,
            "task_detail": task.detail,
            "task_recovery": task.recovery,
            "work_item_id": task.effective_work_item_id(),
            "status_before_restart": status_before,
            "interrupted_reason": interrupted_reason,
        })),
        ..MessageEnvelope::new(
            task.agent_id.clone(),
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: format!(
                    "task {} was interrupted because the {}",
                    task.id,
                    if interrupted_reason == "runtime_restarted" {
                        "runtime restarted"
                    } else {
                        "agent stopped"
                    }
                ),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        )
    };
    message.task_id = Some(task.id.clone());
    message
}

/// Freeze all nondeterminism before the Control transaction; no cleanup/IO.
pub(super) fn prepare_lifecycle_stop_task_result(
    task: &TaskRecord,
    prepared_at: chrono::DateTime<chrono::Utc>,
) -> Result<crate::runtime_db::control_transition::ControlTaskResult> {
    let prior_status = task_status_name(&task.status);
    let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
    let detail_map = detail
        .as_object_mut()
        .ok_or_else(|| anyhow!("Task detail must be an object for lifecycle Stop"))?;
    detail_map.insert("status_before_stop".into(), serde_json::json!(prior_status));
    detail_map.insert("task_status".into(), serde_json::json!("interrupted"));
    detail_map.insert(
        "interrupted_reason".into(),
        serde_json::json!("agent_stopped"),
    );
    detail_map.insert("interrupted_at".into(), to_json_value(&prepared_at));
    let result_turn_id = crate::ids::turn_id();
    detail_map
        .entry(PARENT_TURN_ID_DETAIL)
        .or_insert_with(|| serde_json::json!(result_turn_id));
    let interrupted = TaskRecord {
        status: TaskStatus::Interrupted,
        updated_at: prepared_at,
        parent_message_id: None,
        detail: Some(detail),
        ..task.clone()
    };
    let mut message = interrupted_task_result_message(&interrupted, "agent_stopped");
    message.created_at = prepared_at;
    message.turn_id = Some(result_turn_id);
    let mut terminal = task_with_result_message(
        &interrupted,
        TaskStatus::Interrupted,
        interrupted.detail.clone(),
        &message,
    );
    terminal.updated_at = prepared_at;
    Ok(crate::runtime_db::control_transition::ControlTaskResult {
        task: terminal,
        message,
    })
}

impl RuntimeHandle {
    pub(super) async fn task_creation_detail(
        &self,
        task_id: &str,
        mut detail: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let parent_turn_id = self.agent_state().await?.current_turn_id;
        let detail = detail
            .as_object_mut()
            .ok_or_else(|| anyhow!("task detail must be a JSON object"))?;
        detail.insert(
            REJOIN_OBLIGATION_ID_DETAIL.into(),
            serde_json::json!(task_id),
        );
        detail.insert(REJOIN_GENERATION_DETAIL.into(), serde_json::json!(1));
        if let Some(parent_turn_id) = parent_turn_id {
            detail.insert(
                PARENT_TURN_ID_DETAIL.into(),
                serde_json::json!(parent_turn_id),
            );
        }
        Ok(serde_json::Value::Object(detail.clone()))
    }

    pub(super) fn task_detail_preserving_rejoin_contract(
        &self,
        task: &TaskRecord,
        mut detail: serde_json::Value,
    ) -> serde_json::Value {
        copy_task_rejoin_contract(task, &mut detail);
        detail
    }

    pub(super) async fn task_work_item_binding(&self) -> Option<String> {
        let guard = self.inner.agent.lock().await;
        guard
            .state
            .current_turn_work_item_id
            .clone()
            .or_else(|| guard.state.current_work_item_id.clone())
    }

    pub(super) async fn create_agent_invocation_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
        target_agent_id: Option<String>,
        created_new_subagent: bool,
        workspace_mode: ChildAgentWorkspaceMode,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(crate::types::ACTOR_INVOCATION_TASK_KIND)
            .await?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let recovery = TaskRecoverySpec::AgentInvocation {
            summary: summary.clone(),
            prompt,
            authority_class,
            target_agent_id,
            created_new_subagent,
            workspace_mode,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        };
        let task_id = crate::ids::task_id();
        let detail = self
            .task_creation_detail(
                &task_id,
                serde_json::json!({
                    "wait_policy": crate::types::TaskWaitPolicy::Background,
                    "workspace_mode": workspace_mode,
                    "created_new_subagent": created_new_subagent,
                }),
            )
            .await?;
        let task = TaskRecord {
            id: task_id,
            agent_id,
            kind: TaskKind::ActorInvocation,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary),
            detail: Some(detail),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;
        Ok(task)
    }

    pub(super) async fn create_agent_message_wait_task(
        &self,
        summary: String,
        message: String,
        target_agent_id: String,
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(AGENT_MESSAGE_WAIT_TASK_KIND)
            .await?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let recovery = TaskRecoverySpec::AgentMessageWait {
            summary: summary.clone(),
            message,
            target_agent_id: target_agent_id.clone(),
            authority_class,
            delivery_id: None,
            after_delivery_rowid: None,
        };
        let task_id = crate::ids::task_id();
        let detail = self
            .task_creation_detail(
                &task_id,
                serde_json::json!({
                    "wait_policy": crate::types::TaskWaitPolicy::Background,
                    "target_agent_id": target_agent_id,
                    "business_completion": false,
                }),
            )
            .await?;
        let task = TaskRecord {
            id: task_id,
            agent_id,
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary),
            detail: Some(detail),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;
        Ok(task)
    }

    pub(super) async fn start_agent_message_wait_monitor(
        &self,
        task: TaskRecord,
        authority_class: AuthorityClass,
        target_agent_id: String,
        delivery_id: String,
        after_delivery_rowid: i64,
        task_detail: serde_json::Value,
        recovered: bool,
    ) -> Result<TaskRecord> {
        let mut queued_task = TaskRecord {
            updated_at: Utc::now(),
            detail: Some(self.task_detail_preserving_rejoin_contract(&task, task_detail.clone())),
            ..task
        };
        if let Some(TaskRecoverySpec::AgentMessageWait {
            delivery_id: recovery_delivery_id,
            after_delivery_rowid: recovery_after_delivery_rowid,
            ..
        }) = queued_task.recovery.as_mut()
        {
            *recovery_delivery_id = Some(delivery_id);
            *recovery_after_delivery_rowid = Some(after_delivery_rowid);
        }
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &queued_task,
            "task_agent_message_wait_admitted",
        ))
        .await?;

        self.inner.notify.notify_one();
        let task_detail = queued_task.detail.clone().unwrap_or(task_detail);
        let runtime = self.clone();
        let task_record = queued_task.clone();
        let task_id = queued_task.id.clone();
        let _registration = self
            .admit_control_task_handle_registration(&task_record)
            .await?;
        let control_lease = self.inner.control_cancellations.task(&task_record);
        let control_lease_for_cleanup = control_lease.clone();
        let mut task_handles = self.inner.task_handles.lock().await;
        let handle = tokio::spawn(async move {
            let _ = runtime
                .monitor_agent_message_wait(
                    task_record,
                    authority_class,
                    target_agent_id,
                    after_delivery_rowid,
                    task_detail,
                    recovered,
                )
                .await;
            runtime
                .release_completed_task_handle_exact(&task_id, control_lease_for_cleanup.lease_id())
                .await;
        });
        control_lease.attach(handle);
        task_handles.insert(
            queued_task.id.clone(),
            command_task::ManagedTaskHandle::Async(
                super::control_cancellation::SharedControlTaskHandle(control_lease),
            ),
        );
        Ok(queued_task)
    }

    #[cfg(test)]
    pub(crate) async fn abort_async_task_monitor_for_test(&self, task_id: &str) {
        let handle = self.inner.task_handles.lock().await.remove(task_id);
        let Some(command_task::ManagedTaskHandle::Async(handle)) = handle else {
            panic!("expected async task monitor for {task_id}");
        };
        handle.abort();
        let _ = handle.await;
    }

    pub(super) async fn start_agent_invocation_monitor(
        &self,
        task: TaskRecord,
        authority_class: AuthorityClass,
        worktree: bool,
        child_agent_id: String,
        child_turn_baseline: u64,
        delivery_id: Option<String>,
        task_detail: serde_json::Value,
    ) -> Result<TaskRecord> {
        let queued_task = TaskRecord {
            updated_at: Utc::now(),
            detail: Some(self.task_detail_preserving_rejoin_contract(&task, task_detail.clone())),
            ..task
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &queued_task,
            "task_agent_invocation_admitted",
        ))
        .await?;

        let (_registration, control_lease, producer) =
            self.admit_original_child_monitor(&queued_task).await?;
        let runtime = self.clone();
        let task_record = queued_task.clone();
        let task_id = queued_task.id.clone();
        let lifecycle_disposition = match queued_task.recovery.as_ref() {
            Some(TaskRecoverySpec::AgentInvocation {
                lifecycle_disposition,
                ..
            }) => *lifecycle_disposition,
            _ => AgentLifecycleDisposition::Retain,
        };
        // Its invocation/delivery already exists in the canonical Task. Save
        // the actual monitor parameters before the Async lease can be aborted.
        producer.retain_monitor_stage(
            task_record.clone(),
            authority_class.clone(),
            worktree,
            false,
            lifecycle_disposition,
            child_agent_id.clone(),
            child_turn_baseline,
            delivery_id.clone(),
            task_detail.clone(),
        )?;
        let mut task_handles = self.inner.task_handles.lock().await;
        let runner_material = producer.runner_material()?;
        let handle = tokio::spawn(async move {
            let _runner_material = runner_material;
            #[cfg(test)]
            let clock_monitor_observation = producer.clone();
            if let Err(error) = runtime
                .monitor_spawned_child_agent_task(
                    task_record,
                    producer,
                    authority_class,
                    worktree,
                    false,
                    lifecycle_disposition,
                    child_agent_id,
                    child_turn_baseline,
                    delivery_id,
                    task_detail,
                )
                .await
            {
                #[cfg(test)]
                clock_monitor_observation.record_clock_monitor_failure_for_test(&error);
                tracing::warn!(task_id=%task_id,error=%error,"original child monitor stage remains retained");
            }
        });
        control_lease.attach(handle);
        task_handles.insert(
            queued_task.id.clone(),
            command_task::ManagedTaskHandle::Async(
                super::control_cancellation::SharedControlTaskHandle(control_lease),
            ),
        );
        Ok(queued_task)
    }

    pub(super) async fn fail_agent_invocation_task(
        &self,
        task: &TaskRecord,
        error: &anyhow::Error,
    ) -> Result<()> {
        let failed = super::agent_services::failed_invocation_task(task, error);
        self.persist_task_status_direct(&failed, "task_agent_invocation_failed")
            .await
    }

    pub(super) async fn ensure_background_tasks_allowed(&self, surface: &str) -> Result<()> {
        let state = self.agent_state().await?;
        crate::system::ensure_background_task_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &state.execution_profile,
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            surface,
        )
    }

    pub async fn schedule_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
        workspace_mode: ChildAgentWorkspaceMode,
    ) -> Result<TaskRecord> {
        let _admission = self.execution_admission_lease()?;
        match workspace_mode {
            ChildAgentWorkspaceMode::Inherit => {
                self.schedule_inherited_child_agent_task(summary, prompt, authority_class)
                    .await
            }
            ChildAgentWorkspaceMode::Worktree => {
                self.schedule_worktree_child_agent_task(summary, prompt, authority_class)
                    .await
            }
        }
    }

    async fn schedule_inherited_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let workspace_mode = ChildAgentWorkspaceMode::Inherit;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            authority_class: authority_class.clone(),
            workspace_mode,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        };
        let task_id = crate::ids::task_id();
        let detail = self
            .task_creation_detail(&task_id, child_agent_task_detail(workspace_mode))
            .await?;
        let task = TaskRecord {
            id: task_id,
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.clone()),
            detail: Some(detail),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, authority_class, false, false)
                .await?;
            return Ok(task);
        }

        let runtime = self.clone();
        let task_record = task.clone();
        let task_id = task.id.clone();
        let _registration = self
            .admit_control_task_handle_registration(&task_record)
            .await?;
        let control_lease = self.inner.control_cancellations.task(&task_record);
        let control_lease_for_cleanup = control_lease.clone();
        let mut task_handles = self.inner.task_handles.lock().await;
        let handle = tokio::spawn(async move {
            let running_message = MessageEnvelope {
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": "running",
                    "task_summary": task_record.summary,
                    "task_detail": task_record.detail,
                    "task_recovery": task_record.recovery,
                    "work_item_id": task_record.work_item_id.clone(),
                })),
                ..MessageEnvelope::new(
                    agent_id.clone(),
                    MessageKind::TaskStatus,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    authority_class.clone(),
                    Priority::Background,
                    MessageBody::Text {
                        text: format!(
                            "child agent task started: {}",
                            task_record.summary.clone().unwrap_or_default()
                        ),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let _ = runtime.enqueue(running_message).await;

            let subagent_result = runtime
                .run_subagent_prompt(&agent_id, &prompt, &authority_class)
                .await;
            let (text, status) = match subagent_result {
                Ok(text) => (text, TaskStatus::Completed),
                Err(err) => (format!("child agent failed: {err:#}"), TaskStatus::Failed),
            };
            let status_label = task_status_label(&status);
            let mut task_detail = task_record
                .detail
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            task_detail["output_summary"] = serde_json::json!(text.clone());

            let result_message = MessageEnvelope {
                turn_id: Some(crate::ids::turn_id()),
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": status_label,
                    "task_summary": task_record.summary,
                    "task_detail": task_detail.clone(),
                    "task_recovery": task_record.recovery,
                    "work_item_id": task_record.work_item_id.clone(),
                })),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let terminal_task =
                task_with_result_message(&task_record, status, Some(task_detail), &result_message);
            if let Err(error) = runtime
                .commit_terminal_task_result(&terminal_task, "task_status_updated", &result_message)
                .await
            {
                tracing::warn!(
                    task_id = %terminal_task.id,
                    error = %error,
                    "failed to persist terminal task status before task result"
                );
            }

            runtime
                .release_completed_task_handle_exact(
                    &task_record.id,
                    control_lease_for_cleanup.lease_id(),
                )
                .await;
        });
        control_lease.attach(handle);
        task_handles.insert(
            task_id,
            command_task::ManagedTaskHandle::Async(
                super::control_cancellation::SharedControlTaskHandle(control_lease),
            ),
        );
        drop(task_handles);

        Ok(task)
    }

    pub(crate) async fn resolve_agent_model_request(
        &self,
        tool_name: &str,
        request: Option<AgentModelRequest>,
    ) -> Result<AgentModelResolution> {
        let Some(request) = request else {
            let inherited = self.model_state_for(&self.agent_state().await?);
            return Ok(inherited_spawn_model_resolution(&inherited));
        };

        let provider = ProviderId::parse(&request.provider).map_err(|error| {
            ToolError::new(
                "invalid_tool_input",
                format!("{tool_name} model.provider is invalid: {error}"),
            )
            .with_details(serde_json::json!({
                "field": "model.provider",
                "validation_error": error.to_string(),
            }))
            .with_recovery_hint("provide a lowercase provider id from ListModelProviders")
        })?;
        let requested_model = request.model.trim();
        if requested_model.is_empty() {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "invalid_tool_input",
                    format!("{tool_name} model.model must not be empty"),
                )
                .with_details(serde_json::json!({
                    "field": "model.model",
                    "validation_error": "must not be empty",
                }))
                .with_recovery_hint("provide a model id from ListProviderModels"),
            ));
        }
        let model_ref = ModelRef::new(provider, requested_model.to_string());
        let model_ref_string = model_ref.as_string();
        let availability =
            self.model_availability().await?.into_iter().find(|entry| {
                entry.policy.model_ref == model_ref || entry.model == model_ref_string
            });
        let Some(availability) = availability else {
            return Err(anyhow::Error::from(
                ToolError::new(
                    "model_request_rejected",
                    format!("requested model {model_ref_string} is not in the model catalog"),
                )
                .with_details(serde_json::json!({
                    "requested_model": model_ref_string,
                    "resolution_status": "rejected",
                }))
                .with_recovery_hint(
                    "call ListModelProviders and ListProviderModels before requesting a model",
                ),
            ));
        };

        if !availability.available {
            let allow_fallback = request.allow_fallback.unwrap_or(true);
            let reason = availability
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "model_unavailable".to_string());
            let recovery_hint = if allow_fallback {
                "request an available/selectable model; fallback for explicit unavailable agent model requests is not used before agent creation"
            } else {
                "request an available/selectable model, or omit model to inherit the parent model"
            };
            return Err(anyhow::Error::from(
                ToolError::new(
                    "model_request_rejected",
                    format!("requested model {model_ref_string} is unavailable: {reason}"),
                )
                .with_details(serde_json::json!({
                    "requested_model": model_ref_string,
                    "unavailable_reason": reason,
                    "allow_fallback": allow_fallback,
                    "resolution_status": "rejected",
                }))
                .with_recovery_hint(recovery_hint),
            ));
        }

        let mut resolved_parameters = serde_json::Map::new();
        let mut policy_notes = Vec::new();
        if let Some(reasoning_effort) = request.reasoning_effort.clone() {
            resolved_parameters.insert("reasoning_effort".into(), reasoning_effort.into());
        }
        if let Some(temperature) = request.temperature {
            resolved_parameters.insert("temperature".into(), temperature.into());
            policy_notes.push(
                "temperature is accepted for audit output; provider support is transport-specific"
                    .to_string(),
            );
        }
        if let Some(max_output_tokens) = request.max_output_tokens {
            resolved_parameters.insert("max_output_tokens".into(), max_output_tokens.into());
            policy_notes.push(
                "max_output_tokens is accepted for audit output; runtime policy may cap effective output"
                    .to_string(),
            );
        }

        Ok(AgentModelResolution {
            requested: Some(request),
            resolved_provider: availability.policy.model_ref.provider.as_str().to_string(),
            resolved_model: availability.policy.model_ref.model,
            resolved_parameters: (!resolved_parameters.is_empty()).then_some(resolved_parameters),
            resolution_status: AgentModelResolutionStatus::Accepted,
            policy_notes,
        })
    }

    async fn schedule_worktree_child_agent_task(
        &self,
        summary: String,
        prompt: String,
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed(CHILD_AGENT_TASK_KIND)
            .await?;
        let workspace_mode = ChildAgentWorkspaceMode::Worktree;
        let state = self.agent_state().await?;
        crate::system::ensure_workspace_projection_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &state.execution_profile,
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            WorkspaceProjectionKind::GitWorktreeRoot,
            CHILD_AGENT_TASK_KIND,
        )?;
        let agent_id = self.agent_id().await?;
        let work_item_id = self.task_work_item_binding().await;
        let recovery = TaskRecoverySpec::ChildAgentTask {
            summary: summary.clone(),
            prompt: prompt.clone(),
            authority_class: authority_class.clone(),
            workspace_mode,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        };
        let task_id = crate::ids::task_id();
        let detail = self
            .task_creation_detail(&task_id, child_agent_task_detail(workspace_mode))
            .await?;
        let task = TaskRecord {
            id: task_id,
            agent_id: agent_id.clone(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.clone()),
            detail: Some(detail),
            recovery: Some(recovery),
        };
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;

        if self.inner.host_bridge.is_some() {
            self.spawn_child_agent_task(task.clone(), prompt, authority_class, true, false)
                .await?;
            return Ok(task);
        }

        let (_registration, control_lease, producer) =
            self.admit_original_child_monitor(&task).await?;
        let runtime = self.clone();
        let task_record = task.clone();
        let task_id = task.id.clone();
        let mut task_handles = self.inner.task_handles.lock().await;
        let runner_material = producer.runner_material()?;
        let handle = tokio::spawn(async move {
            let _runner_material = runner_material;
            let subagent_result = runtime
                .run_subagent_prompt_in_dedicated_worktree(
                    &agent_id,
                    &prompt,
                    &authority_class,
                    &task_record.id,
                )
                .await;

            let (text, status, mut task_detail, worktree_path): (
                String,
                TaskStatus,
                serde_json::Value,
                Option<(PathBuf, String, Vec<String>)>,
            ) = match subagent_result {
                Ok(result) => {
                    if let Ok(completion) = producer.completion_material() {
                        completion.observe_material(
                            super::input_evidence_budget::serialized_bytes(&(
                                &result.text,
                                &result.worktree_path,
                                &result.worktree_branch,
                                &result.changed_files,
                            ))
                            .unwrap_or(u64::MAX)
                            .saturating_mul(8),
                            1024,
                        );
                    }

                    let worktree_path = result.worktree_path.clone();
                    let worktree_branch = result.worktree_branch.clone();
                    let changed_files = result.changed_files.clone();

                    let worktree_metadata = serde_json::json!({
                        "worktree_path": result.worktree_path,
                        "worktree_branch": result.worktree_branch,
                        "changed_files": result.changed_files,
                    });
                    (
                        worktree::format_worktree_task_result(&result),
                        if result.failed {
                            TaskStatus::Failed
                        } else {
                            TaskStatus::Completed
                        },
                        {
                            let mut detail = task_record
                                .detail
                                .clone()
                                .unwrap_or_else(|| serde_json::json!({}));
                            detail["worktree"] = worktree_metadata;
                            detail
                        },
                        Some((worktree_path, worktree_branch, changed_files)),
                    )
                }
                Err(err) => (
                    format!("worktree child agent failed: {err:#}"),
                    TaskStatus::Failed,
                    task_record
                        .detail
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                    None,
                ),
            };

            // Actual cleanup is performed by the retained owned completion.
            let _original_worktree_path = worktree_path;
            task_detail["output_summary"] = serde_json::json!(text.clone());
            let status_label = task_status_label(&status);
            let mut metadata = serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": status_label,
                "task_summary": task_record.summary,
                "task_detail": task_detail.clone(),
                "task_recovery": task_record.recovery,
                "work_item_id": task_record.work_item_id.clone(),
            });
            if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
                metadata["worktree"] = worktree;
            }
            let result_message = MessageEnvelope {
                turn_id: Some(crate::ids::turn_id()),
                metadata: Some(metadata),
                ..MessageEnvelope::new(
                    agent_id,
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let terminal_task =
                task_with_result_message(&task_record, status, Some(task_detail), &result_message);
            if let Err(error) = runtime
                .complete_observed_child_monitor(
                    producer,
                    terminal_task,
                    result_message,
                    None,
                    None,
                    true,
                )
                .await
            {
                tracing::warn!(task_id=%task_record.id,error=%error,"original worktree child terminal remains retained");
            }
        });
        control_lease.attach(handle);
        task_handles.insert(
            task_id,
            command_task::ManagedTaskHandle::Async(
                super::control_cancellation::SharedControlTaskHandle(control_lease),
            ),
        );
        drop(task_handles);

        Ok(task)
    }

    pub(in crate::runtime) async fn spawn_child_agent_task(
        &self,
        task_record: TaskRecord,
        prompt: String,
        authority_class: AuthorityClass,
        worktree: bool,
        recovered: bool,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Err(anyhow!("child agent runtime requires a host bridge"));
        };
        let _agent_id = self.agent_id().await?;
        let lifecycle_disposition = match task_record.recovery.as_ref() {
            Some(TaskRecoverySpec::ChildAgentTask {
                lifecycle_disposition,
                ..
            }) => *lifecycle_disposition,
            _ => AgentLifecycleDisposition::DeleteOnTerminal,
        };

        let (_registration, control_lease, producer) =
            self.admit_original_child_monitor(&task_record).await?;
        let inherited_model_resolution =
            inherited_spawn_model_resolution(&self.model_state_for(&_registration.state));
        producer.retain_original_child_spawn(
            task_record,
            prompt,
            authority_class,
            worktree,
            recovered,
            lifecycle_disposition,
            inherited_model_resolution,
            self.now(),
        )?;
        let runtime = self.clone();
        let task_id = producer.address().admitted.key.id.clone();
        // Attach before the cancellable map wait. The barrier still prevents
        // any Host registration until the exact handle is published.
        let original = producer.clone();
        let (published, publication) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let result = async {
                publication
                    .await
                    .context("original child handle publication stopped")?;
                runtime
                    .continue_original_child_spawn(original.clone())
                    .await?;
                runtime
                    .continue_original_child_terminal(original.clone())
                    .await
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(task_id=%original.address().admitted.key.id,error=%error,"original owned child spawn/monitor remains retained");
            }
        });
        control_lease.attach(handle);
        // If the caller is cancelled at this await, dropping the publication
        // sender finishes the attached observer without starting any Host kernel.
        let mut task_handles = self.inner.task_handles.lock().await;
        task_handles.insert(
            task_id,
            command_task::ManagedTaskHandle::Async(
                super::control_cancellation::SharedControlTaskHandle(control_lease),
            ),
        );
        let _ = published.send(());
        drop(task_handles);
        // This registration waits only for the existing Host registry fence,
        // never for the finite kernel or a parent mutation owner.
        let registered = bridge
            .start_original_child_spawn(self.clone(), producer.clone())
            .await;
        if let Err(error) = registered {
            if error.downcast_ref::<super::child_task_terminal::OriginalChildMonitorCancellationPending>().is_none() {
                producer.record_original_child_spawn_completion(Err(error))?;
            }
        }
        Ok(())
    }

    async fn monitor_agent_message_wait(
        &self,
        task_record: TaskRecord,
        authority_class: AuthorityClass,
        target_agent_id: String,
        after_delivery_rowid: i64,
        task_detail: serde_json::Value,
        recovered: bool,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Err(anyhow!("agent message wait requires a host bridge"));
        };
        let agent_id = self.agent_id().await?;
        let running_message = MessageEnvelope {
            metadata: Some(serde_json::json!({
                "task_id": task_record.id,
                "task_created_at": task_record.created_at,
                "task_kind": task_record.kind,
                "task_status": "running",
                "task_summary": task_record.summary,
                "task_recovery": task_record.recovery,
                "work_item_id": task_record.work_item_id.clone(),
                "task_detail": task_detail.clone(),
            })),
            ..MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                authority_class,
                Priority::Background,
                MessageBody::Text {
                    text: format!(
                        "agent message wait {}: {}",
                        if recovered { "restarted" } else { "started" },
                        task_record.summary.clone().unwrap_or_default()
                    ),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            )
        };
        let _ = self.enqueue(running_message).await;

        let result = bridge
            .await_agent_message(&agent_id, &target_agent_id, after_delivery_rowid)
            .await;
        let (text, status, terminal_detail) = match result {
            Ok(result) => {
                let mut detail = task_detail;
                if let Some(result_detail) = result.task_detail {
                    if let (Some(detail), Some(result_detail)) =
                        (detail.as_object_mut(), result_detail.as_object())
                    {
                        detail.extend(result_detail.clone());
                    } else {
                        detail = result_detail;
                    }
                }
                (result.text, result.status, detail)
            }
            Err(error) => (
                format!("agent message wait failed: {error:#}"),
                TaskStatus::Failed,
                task_detail,
            ),
        };
        self.finish_agent_message_wait(&task_record, status, terminal_detail, text)
            .await?;
        Ok(())
    }

    pub(super) async fn finish_agent_message_wait(
        &self,
        task: &TaskRecord,
        status: TaskStatus,
        detail: serde_json::Value,
        failure_text: String,
    ) -> Result<()> {
        self.finish_agent_message_wait_with_material(task, status, detail, failure_text, (0, 0))
            .await
    }

    async fn finish_agent_message_wait_with_material(
        &self,
        task: &TaskRecord,
        status: TaskStatus,
        mut detail: serde_json::Value,
        failure_text: String,
        retained_material: (u64, usize),
    ) -> Result<()> {
        let selected = self.capture_current_task_record(&task.id)?;
        let current = selected
            .record()
            .ok_or_else(|| task_not_found_error(&task.id))?;
        self.validate_agent_message_wait_task(task, current)?;
        if task_state_reducer::is_terminal_task_status(&current.status) {
            return Ok(());
        }
        let _projection = self.agent_message_wait_projection_budget(
            &selected,
            &detail,
            &failure_text,
            retained_material,
        )?;
        // Covers the short-lived owned fences returned by the canonical API too.
        self.validate_agent_message_wait_generation(task, current)?;
        let task = current;
        let response = detail
            .get("message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let text = if status == TaskStatus::Completed {
            format!(
                "Agent reply received; see message {} / delivery {}.",
                response
                    .as_deref()
                    .ok_or_else(|| anyhow!("agent reply reference is missing"))?,
                detail
                    .get("response_delivery_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("agent reply delivery reference is missing"))?
            )
        } else {
            failure_text
        };
        detail["output_summary"] = serde_json::json!(text);
        detail["business_completion"] = serde_json::json!(false);
        if status == TaskStatus::Completed {
            detail["reply_content_source"] = serde_json::json!("original_message");
        }
        let mut message = MessageEnvelope::new(
            task.agent_id.clone(),
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text { text },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.task_id = Some(task.id.clone());
        message.id = format!("message:agent-message-wait:{}", task.id);
        message.turn_id = Some(format!("turn:agent-message-wait:{}", task.id));
        message.created_at = if status == TaskStatus::Completed {
            self.inner
                .storage
                .read_message_by_id(response.as_deref().expect("validated reply"))?
                .ok_or_else(|| anyhow!("agent reply evidence is missing"))?
                .created_at
        } else {
            Utc::now()
        };
        message.work_item_id = task.work_item_id.clone();
        message.metadata = Some(serde_json::json!({
            "task_id": task.id, "task_kind": task.kind, "task_status": status,
            "task_summary": task.summary, "task_recovery": task.recovery,
            "work_item_id": task.work_item_id, "task_detail": detail,
        }));
        let mut terminal = task_with_result_message(&task, status, Some(detail), &message);
        if unbound_task_rejoin_generation(task).is_some() {
            // The actual qualified Task had no parent. The Result's own Turn is
            // source provenance, not authority to bind this existing obligation.
            let original = task
                .detail
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .expect("validated original unbound contract");
            let detail = terminal
                .detail
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
                .expect("message Wait terminal detail object");
            for key in [REJOIN_OBLIGATION_ID_DETAIL, REJOIN_GENERATION_DETAIL] {
                detail.insert(key.into(), original[key].clone());
            }
            detail.remove(PARENT_TURN_ID_DETAIL);
        }
        message.metadata.as_mut().expect("task result metadata")["task_detail"] =
            serde_json::to_value(&terminal.detail)?;
        self.commit_terminal_task_result(&terminal, "task_agent_message_wait_completed", &message)
            .await
    }

    // Reconcile before admission, so a fast reply cannot outrun its monitor.
    pub(super) async fn reconcile_agent_message_reply(
        &self,
        message: &MessageEnvelope,
    ) -> Result<ReplyReconciliation> {
        if !crate::wake_contract::is_agent_message(message) {
            return Ok(ReplyReconciliation::Ready);
        }
        anyhow::ensure!(
            message.agent_id == self.inner.mutation_owner.agent_id(),
            "AgentMessageWait reply target differs from runtime scope"
        );
        let Some(delivery) = self
            .inner
            .runtime_db
            .agent_message_deliveries()
            .latest_for_message(&message.id)?
        else {
            return Ok(ReplyReconciliation::Ready);
        };
        let mut changed = false;
        // Select the complete current set once. Borrow Task bodies in their
        // original order and keep selected Arcs/reference slots across each ACK.
        let active = self
            .capture_task_read(&super::task_reads::active_task_request())
            .await?;
        let _active_references = self.agent_message_wait_reference_budget(&active.tasks)?;
        for task in super::task_reads::active_task_records(&active.tasks)? {
            let Some(TaskRecoverySpec::AgentMessageWait {
                target_agent_id,
                after_delivery_rowid,
                ..
            }) = &task.recovery
            else {
                continue;
            };
            if delivery.caller.caller_agent_id.as_deref() != Some(target_agent_id.as_str()) {
                continue;
            }
            let Some(boundary) = after_delivery_rowid else {
                // Invoke has accepted the request but has not attached its receipt yet.
                return Ok(ReplyReconciliation::WaitingForReceipt);
            };
            let Some(reply) = self
                .inner
                .runtime_db
                .agent_message_deliveries()
                .first_accepted_from_sender_after(&message.agent_id, target_agent_id, *boundary)?
            else {
                continue;
            };
            let (_detail_budget, retained_material) = self.agent_message_wait_reply_detail_budget(
                &active.tasks,
                task,
                target_agent_id,
                &message.agent_id,
                &reply.delivery_id,
                reply.message_id.as_deref(),
                *boundary,
            )?;
            let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
            detail["message_wait_satisfied"] = serde_json::json!(true);
            detail["sender_agent_id"] = serde_json::json!(target_agent_id);
            detail["recipient_agent_id"] = serde_json::json!(message.agent_id);
            detail["response_delivery_id"] = serde_json::json!(reply.delivery_id);
            detail["message_id"] = serde_json::json!(reply.message_id);
            detail["message_wait_after_delivery_rowid"] = serde_json::json!(boundary);
            self.finish_agent_message_wait_with_material(
                task,
                TaskStatus::Completed,
                detail,
                String::new(),
                retained_material,
            )
            .await?;
            changed = true;
        }
        Ok(if changed {
            ReplyReconciliation::Changed
        } else {
            ReplyReconciliation::Ready
        })
    }

    pub(super) async fn monitor_spawned_child_agent_task(
        &self,
        task_record: TaskRecord,
        producer: Arc<super::OriginalChildTaskProducer>,
        authority_class: AuthorityClass,
        worktree: bool,
        recovered: bool,
        lifecycle_disposition: AgentLifecycleDisposition,
        child_agent_id: String,
        child_turn_baseline: u64,
        delivery_id: Option<String>,
        task_detail: serde_json::Value,
    ) -> Result<()> {
        producer.retain_monitor_stage(
            task_record,
            authority_class,
            worktree,
            recovered,
            lifecycle_disposition,
            child_agent_id,
            child_turn_baseline,
            delivery_id,
            task_detail,
        )?;
        self.continue_original_child_monitor(producer).await
    }
    pub(in crate::runtime) fn continue_original_child_monitor(
        &self,
        producer: Arc<super::OriginalChildTaskProducer>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let _worker = producer.monitor_worker.lock().await;
            if producer.is_durable_finished() {
                return Ok(());
            }
            if producer.observed_terminal().is_some() {
                return self
                    .continue_original_child_terminal(producer.clone())
                    .await;
            }
            let original_lease = producer.original_lease()?;
            if original_lease.cancellation_requested() {
                return self
                    .finish_original_child_monitor_cancellation(&producer)
                    .await;
            }
            let stage = producer
                .monitor_stage()
                .context("original child monitor stage absent")?;
            let task_record = stage.task.clone();
            let authority_class = stage.authority.clone();
            let worktree = stage.worktree;
            let recovered = stage.recovered;
            let lifecycle_disposition = stage.lifecycle;
            let child_agent_id = stage.child.clone();
            let child_turn_baseline = stage.baseline;
            let delivery_id = stage.delivery.clone();
            let task_detail = stage.detail.clone();

            let Some(bridge) = self.inner.host_bridge.clone() else {
                return Err(anyhow!("child agent runtime requires a host bridge"));
            };
            let agent_id = producer.address().scope.agent_id.clone();

            self.bind_original_child_monitor_target(
                &producer,
                &child_agent_id,
                delivery_id.as_deref(),
                child_turn_baseline,
            )?;
            anyhow::ensure!(
                self.continue_original_child_monitor_binding(producer.clone())
                    .await?,
                "original child binding owner unavailable; monitor responsibility remains retained"
            );

            if stage.claim_running_attempt() {
                let mut running_message = MessageEnvelope {
                    metadata: Some(serde_json::json!({
                        "task_id": task_record.id,
                        "task_created_at":task_record.created_at,
                        "task_kind": task_record.kind,
                        "task_status": "running",
                        "task_summary": task_record.summary,
                        "task_recovery": task_record.recovery,
                        "work_item_id": task_record.work_item_id.clone(),
                        "task_detail": self.task_detail_preserving_rejoin_contract(&task_record, task_detail.clone()),
                    })),
                    ..MessageEnvelope::new(
                        agent_id.clone(),
                        MessageKind::TaskStatus,
                        MessageOrigin::Task {
                            task_id: task_record.id.clone(),
                        },
                        authority_class.clone(),
                        Priority::Background,
                        MessageBody::Text {
                            text: if recovered {
                                format!(
                                    "{} restarted after recovery: {}",
                                    if worktree {
                                        "worktree child agent"
                                    } else {
                                        "child agent"
                                    },
                                    task_record.summary.clone().unwrap_or_default()
                                )
                            } else {
                                format!(
                                    "{} started: {}",
                                    if worktree {
                                        "worktree child agent"
                                    } else {
                                        "child agent"
                                    },
                                    task_record.summary.clone().unwrap_or_default()
                                )
                            },
                        },
                    )
                    .with_admission(
                        MessageDeliverySurface::TaskRejoin,
                        AdmissionContext::RuntimeOwned,
                    )
                };
                running_message.normalize_admission_fields();
                // Freeze the same original fields enqueue would otherwise allocate.
                // Only the canonical DB sequence is assigned after this producer seal.
                if running_message.turn_id.is_none() {
                    running_message.turn_id = Some(crate::ids::turn_id());
                }
                if running_message.trace_context.is_none() {
                    running_message.trace_context =
                        Some(crate::observability::TraceContext::new_root(true));
                }
                producer.freeze_original_running_status(&running_message)?;
                let _running_enqueued = self.enqueue(running_message).await;
                #[cfg(test)]
                if let Err(error) = &_running_enqueued {
                    producer.record_clock_monitor_failure_for_test(error);
                }
                drop(_running_enqueued);
            }

            let task_detail_for_result = task_detail.clone();
            let result = tokio::select! {
                biased;
                _=original_lease.cancellation_observation().cancelled()=>{
                    return self.finish_original_child_monitor_cancellation(&producer).await;
                },
                result=bridge.await_agent_invocation_terminal_result(
                    &child_agent_id,child_turn_baseline,delivery_id.as_deref(),&task_record.id,worktree)=>result,
            };
            let (mut text, status, mut task_detail) = match result {
                Ok(result) => {
                    let completion = producer.completion_material()?;
                    let nodes = super::child_task_terminal::completion_value_nodes(
                        result.task_detail.as_ref(),
                    )
                    .unwrap_or(usize::MAX);
                    completion.observe_material(
                        super::input_evidence_budget::serialized_bytes(&(
                            &result.text,
                            &result.task_detail,
                        ))
                        .unwrap_or(u64::MAX)
                        .saturating_mul(8),
                        u64::try_from(nodes)
                            .unwrap_or(u64::MAX)
                            .saturating_add(1024)
                            .saturating_mul(8 * 256),
                    );

                    let mut detail = task_detail_for_result.clone();
                    if let Some(result_detail) = result.task_detail {
                        if let (Some(detail), Some(result_detail)) =
                            (detail.as_object_mut(), result_detail.as_object())
                        {
                            detail.extend(result_detail.clone());
                        } else {
                            detail = result_detail;
                        }
                    }
                    (result.text, result.status, detail)
                }
                Err(err) => (
                    format!("child agent failed: {err:#}"),
                    TaskStatus::Failed,
                    task_detail_for_result.clone(),
                ),
            };
            if worktree && task_detail.get("worktree").is_none() {
                if let Some(worktree_detail) = task_detail_for_result.get("worktree").cloned() {
                    task_detail["worktree"] = worktree_detail;
                }
            }
            if task_detail.get("workspace_mode").is_none() {
                if let Some(workspace_mode) = task_detail_for_result.get("workspace_mode").cloned()
                {
                    task_detail["workspace_mode"] = workspace_mode;
                }
            }
            if task_detail.get("wait_policy").is_none() {
                if let Some(wait_policy) = task_detail_for_result.get("wait_policy").cloned() {
                    task_detail["wait_policy"] = wait_policy;
                }
            }

            if worktree {
                if let Some(worktree) = task_detail.get("worktree").cloned() {
                    let worktree_path = worktree
                        .get("worktree_path")
                        .and_then(|value| value.as_str())
                        .map(PathBuf::from);
                    let worktree_branch = worktree
                        .get("worktree_branch")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned);
                    let changed_files = worktree
                        .get("changed_files")
                        .and_then(|value| value.as_array())
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(|entry| entry.as_str().map(str::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    if let (Some(path), Some(branch)) = (worktree_path, worktree_branch) {
                        text = worktree::format_worktree_task_result(&WorktreeSubagentResult {
                            text,
                            worktree_path: path.clone(),
                            worktree_branch: branch.clone(),
                            changed_files: changed_files.clone(),
                            failed: status == TaskStatus::Failed,
                        });
                    }
                }
            }

            let status_label = task_status_label(&status);
            let mut metadata = serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": status_label,
                "task_summary": task_record.summary,
                "task_recovery": task_record.recovery,
                "work_item_id": task_record.work_item_id.clone(),
                "task_detail": task_detail.clone(),
            });
            if let Some(worktree) = metadata["task_detail"].get("worktree").cloned() {
                metadata["worktree"] = worktree;
            }
            task_detail["output_summary"] = serde_json::json!(text.clone());
            let result_message = MessageEnvelope {
                turn_id: Some(crate::ids::turn_id()),
                metadata: Some(metadata),
                ..MessageEnvelope::new(
                    agent_id.clone(),
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text { text },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                )
            };
            let terminal_task =
                task_with_result_message(&task_record, status, Some(task_detail), &result_message);
            let agent_deletion = (lifecycle_disposition
            == AgentLifecycleDisposition::DeleteOnTerminal)
            .then(|| crate::runtime_db::repositories::AgentDeletionRequest {
                agent_id: child_agent_id.clone(),
                admission:
                    crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                        parent_agent_id: agent_id,
                        task_id: terminal_task.id.clone(),
                    },
                requested_by: "parent_task_terminal".into(),
                cascade_private_children: false,
            });
            self.complete_observed_child_monitor(
                producer.clone(),
                terminal_task,
                result_message,
                Some(child_agent_id),
                agent_deletion,
                worktree,
            )
            .await
        })
    }

    async fn finish_original_child_monitor_cancellation(
        &self,
        source: &Arc<super::OriginalChildTaskProducer>,
    ) -> Result<()> {
        let limits = self.inner.input_evidence_budget.limits();
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("child Stop transfer limits overflow")?,
            )?,
        };
        let proof = self.inner.runtime_db.transaction(|tx| {
            crate::runtime_db::child_task_terminal::observe_original_child_monitor_stop_handoff_tx(
                tx,
                source,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        if let Some(proof) = proof {
            source.acknowledge_monitor_stop_handoff(&proof)?;
        } else {
            let Some(bridge) = self.inner.host_bridge.as_ref() else {
                return Err(
                    super::child_task_terminal::OriginalChildMonitorCancellationPending.into(),
                );
            };
            if !bridge
                .handoff_original_child_monitor_recovery(source, &request)
                .await?
            {
                return Err(
                    super::child_task_terminal::OriginalChildMonitorCancellationPending.into(),
                );
            }
        }
        self.release_completed_task_handle_exact(
            &source.address().admitted.key.id,
            &source.address().lease_id,
        )
        .await;
        Ok(())
    }

    pub(crate) async fn interrupt_active_tasks(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<Vec<TaskRecord>> {
        self.persist_interrupted_tasks(tasks).await
    }

    pub(crate) async fn recover_supervised_child_tasks(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<(Vec<TaskRecord>, Vec<TaskRecord>)> {
        let Some(bridge) = self.inner.host_bridge.clone() else {
            return Ok((Vec::new(), tasks));
        };

        let mut reattached = Vec::new();
        let mut remaining = Vec::new();

        for task in tasks {
            if let Some(TaskRecoverySpec::AgentMessageWait {
                message,
                target_agent_id,
                authority_class,
                delivery_id,
                after_delivery_rowid,
                ..
            }) = task.recovery.clone()
            {
                if !bridge.reusable_agent_exists(&target_agent_id).await? {
                    remaining.push(task);
                    continue;
                }
                let recovery = async {
                    if let (Some(delivery_id), Some(after_delivery_rowid)) =
                        (delivery_id, after_delivery_rowid)
                    {
                        let mut task_detail =
                            task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
                        task_detail["request_delivery_id"] = serde_json::json!(&delivery_id);
                        if let Some(detail) = task_detail.as_object_mut() {
                            detail.remove("delivery_id");
                        }
                        task_detail["message_wait_after_delivery_rowid"] =
                            serde_json::json!(after_delivery_rowid);
                        task_detail["business_completion"] = serde_json::json!(false);
                        return self
                            .start_agent_message_wait_monitor(
                                task.clone(),
                                authority_class.clone(),
                                target_agent_id.clone(),
                                delivery_id,
                                after_delivery_rowid,
                                task_detail,
                                true,
                            )
                            .await;
                    }
                    let mut admitted = bridge
                        .invoke_existing_agent(
                            &task,
                            &target_agent_id,
                            message,
                            authority_class.clone(),
                        )
                        .await?;
                    let delivery_id = admitted.delivery_id.clone().ok_or_else(|| {
                        anyhow!("accepted agent message invocation is missing its delivery id")
                    })?;
                    let after_delivery_rowid = bridge.agent_message_delivery_rowid(&delivery_id)?;
                    admitted.task_detail["created_new_subagent"] = serde_json::json!(false);
                    admitted.task_detail["request_delivery_id"] = serde_json::json!(&delivery_id);
                    if let Some(detail) = admitted.task_detail.as_object_mut() {
                        detail.remove("delivery_id");
                    }
                    admitted.task_detail["message_wait_after_delivery_rowid"] =
                        serde_json::json!(after_delivery_rowid);
                    admitted.task_detail["business_completion"] = serde_json::json!(false);
                    self.start_agent_message_wait_monitor(
                        task.clone(),
                        authority_class.clone(),
                        target_agent_id.clone(),
                        delivery_id,
                        after_delivery_rowid,
                        admitted.task_detail,
                        true,
                    )
                    .await
                }
                .await;
                match recovery {
                    Ok(_) => reattached.push(task),
                    Err(error) => {
                        self.inner.storage.append_event(&AuditEvent::legacy(
                            "agent_message_wait_recovery_failed",
                            serde_json::json!({
                                "task_id": task.id,
                                "target_agent_id": target_agent_id,
                                "error": error.to_string(),
                            }),
                        ))?;
                        remaining.push(task);
                    }
                }
                continue;
            }

            let (prompt, authority_class, worktree, invocation_target) =
                match task.recovery.as_ref() {
                    Some(TaskRecoverySpec::AgentInvocation {
                        prompt,
                        authority_class,
                        target_agent_id,
                        workspace_mode,
                        ..
                    }) => {
                        let target_agent_id = target_agent_id
                            .clone()
                            .or_else(|| detail_string(&task.detail, "target_agent_id"))
                            .or_else(|| detail_string(&task.detail, "child_agent_id"));
                        (
                            prompt.clone(),
                            authority_class.clone(),
                            workspace_mode.is_worktree(),
                            target_agent_id,
                        )
                    }
                    Some(TaskRecoverySpec::ChildAgentTask {
                        prompt,
                        authority_class,
                        workspace_mode,
                        ..
                    }) => (
                        prompt.clone(),
                        authority_class.clone(),
                        workspace_mode.is_worktree(),
                        None,
                    ),
                    Some(TaskRecoverySpec::SubagentTask {
                        prompt,
                        authority_class,
                        ..
                    }) => (prompt.clone(), authority_class.clone(), false, None),
                    Some(TaskRecoverySpec::WorktreeSubagentTask {
                        prompt,
                        authority_class,
                        ..
                    }) => (prompt.clone(), authority_class.clone(), true, None),
                    _ => {
                        remaining.push(task);
                        continue;
                    }
                };

            let target_agent_id = invocation_target
                .clone()
                .or_else(|| detail_string(&task.detail, "child_agent_id"));
            let Some(target_agent_id) = target_agent_id else {
                remaining.push(task);
                continue;
            };

            if !bridge.reusable_agent_exists(&target_agent_id).await? {
                remaining.push(task);
                continue;
            }

            let recovery_result = if invocation_target.is_some() {
                let child_turn_baseline = task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("child_turn_baseline"))
                    .and_then(serde_json::Value::as_u64);
                if let Some(child_turn_baseline) = child_turn_baseline {
                    let delivery_id = detail_string(&task.detail, "delivery_id");
                    let task_detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
                    self.start_agent_invocation_monitor(
                        task.clone(),
                        authority_class,
                        worktree,
                        target_agent_id.clone(),
                        child_turn_baseline,
                        delivery_id,
                        task_detail,
                    )
                    .await
                    .map(|_| ())
                } else {
                    Err(anyhow!(
                        "legacy existing-agent invocation {} is missing its durable turn baseline",
                        task.id
                    ))
                }
            } else {
                self.spawn_child_agent_task(task.clone(), prompt, authority_class, worktree, true)
                    .await
            };
            match recovery_result {
                Ok(()) => reattached.push(task),
                Err(error) => {
                    self.inner.storage.append_event(&AuditEvent::legacy(
                        "supervised_child_task_recovery_failed",
                        serde_json::json!({
                            "task_id": task.id,
                            "target_agent_id": target_agent_id,
                            "error": error.to_string(),
                        }),
                    ))?;
                    remaining.push(task);
                }
            }
        }

        Ok((reattached, remaining))
    }

    pub async fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks = self.inner.runtime_db.tasks().latest_all()?;
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub async fn latest_task_list_entries(&self) -> Result<Vec<TaskListEntry>> {
        Ok(self.active_task_list_snapshot(usize::MAX).await?.entries)
    }

    pub async fn latest_task_list_entries_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskListEntry>> {
        anyhow::ensure!(
            agent_id == self.inner.mutation_owner.agent_id(),
            "Task list agent differs from resident scope"
        );
        let read = self.active_task_list_snapshot(limit).await?;
        Ok(read.entries)
    }

    pub(crate) async fn active_task_list_snapshot(
        &self,
        limit: usize,
    ) -> Result<super::task_reads::TaskListRead> {
        let read = self
            .capture_task_read(&super::task_reads::active_task_request())
            .await?;
        let active = super::task_reads::active_task_records(&read.tasks)?;
        let total_active = active.len();
        let selected = active.into_iter().take(limit).collect::<Vec<_>>();
        let owned_budget = self.task_projection_budget(&read.tasks, &selected, 2)?;
        let entries = selected
            .into_iter()
            .map(|task| TaskListEntry {
                id: task.id.clone(),
                kind: task.kind.as_str().to_string(),
                status: task.status.clone(),
                summary: task.summary.clone(),
                updated_at: task.updated_at,
                wait_policy: task.wait_policy(),
                command: CommandTaskStatusSnapshot::identity_from_task_record(task),
            })
            .collect();
        Ok(super::task_reads::TaskListRead {
            total_active,
            entries,
            _owned_budget: owned_budget,
        })
    }

    async fn persist_task_status_direct(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.persist_task_transition(task, event_kind).await
    }

    async fn persist_interrupted_tasks(&self, tasks: Vec<TaskRecord>) -> Result<Vec<TaskRecord>> {
        let mut interrupted = Vec::new();
        for task in tasks {
            let prior_status = task_status_name(&task.status);
            let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(detail) = detail.as_object_mut() {
                detail.insert(
                    "status_before_restart".into(),
                    serde_json::json!(prior_status),
                );
                detail.insert("task_status".into(), serde_json::json!("interrupted"));
                detail.insert(
                    "interrupted_reason".into(),
                    serde_json::json!("runtime_restarted"),
                );
                detail.insert("interrupted_at".into(), to_json_value(&Utc::now()));
            }
            let interrupted_task = TaskRecord {
                id: task.id.clone(),
                agent_id: task.agent_id.clone(),
                kind: task.kind.clone(),
                status: TaskStatus::Interrupted,
                created_at: task.created_at,
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: task.work_item_id.clone(),
                summary: task.summary.clone(),
                detail: Some(detail),
                recovery: task.recovery.clone(),
            };
            let result_message =
                interrupted_task_result_message(&interrupted_task, "runtime_restarted");
            let interrupted_task =
                task_with_original_interrupted_result(&interrupted_task, &result_message);
            self.commit_terminal_task_result(
                &interrupted_task,
                "task_interrupted_on_restart",
                &result_message,
            )
            .await?;
            interrupted.push(interrupted_task);
        }
        Ok(interrupted)
    }

    pub(super) async fn interrupt_active_tasks_for_lifecycle_stop(
        &self,
        tasks: Vec<TaskRecord>,
    ) -> Result<Vec<TaskRecord>> {
        let mut interrupted = Vec::new();
        for task in tasks {
            {
                let mut handles = self.inner.task_handles.lock().await;
                match handles.remove(&task.id) {
                    Some(command_task::ManagedTaskHandle::Async(handle)) => {
                        handle.abort();
                    }
                    Some(command_task::ManagedTaskHandle::Command(mut handle)) => {
                        if let Some(cancel_tx) = handle.cancel_tx.take() {
                            let _ = cancel_tx.send(());
                        }
                        if let Some(force_stop_tx) = handle.force_stop_tx.take() {
                            let _ = force_stop_tx.send(());
                        }
                    }
                    None => {}
                }
            }

            let prior_status = task_status_name(&task.status);
            let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(detail_map) = detail.as_object_mut() {
                detail_map.insert("status_before_stop".into(), serde_json::json!(prior_status));
                detail_map.insert("task_status".into(), serde_json::json!("interrupted"));
                detail_map.insert(
                    "interrupted_reason".into(),
                    serde_json::json!("agent_stopped"),
                );
                detail_map.insert("interrupted_at".into(), to_json_value(&Utc::now()));
            }
            if task.is_worktree_child_agent_task() {
                // Parent monitor cancellation does not prove original child exit.
                if let Some(worktree) = detail
                    .get_mut("worktree")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    worktree.insert("cleanup_status".into(), serde_json::json!("retained"));
                    worktree.insert(
                        "cleanup_reason".into(),
                        serde_json::json!("original_child_exit_unproven"),
                    );
                    worktree.insert("retained_for_review".into(), serde_json::json!(true));
                }
            }

            let interrupted_task = TaskRecord {
                id: task.id.clone(),
                agent_id: task.agent_id.clone(),
                kind: task.kind,
                status: TaskStatus::Interrupted,
                created_at: task.created_at,
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: task.work_item_id.clone(),
                summary: task.summary.clone(),
                detail: Some(detail),
                recovery: task.recovery.clone(),
            };
            let result_message =
                interrupted_task_result_message(&interrupted_task, "agent_stopped");
            let interrupted_task =
                task_with_original_interrupted_result(&interrupted_task, &result_message);
            self.commit_terminal_task_result(
                &interrupted_task,
                "task_interrupted_on_agent_stop",
                &result_message,
            )
            .await?;
            interrupted.push(interrupted_task);
        }
        Ok(interrupted)
    }

    pub async fn task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        self.inner.runtime_db.tasks().latest(task_id)
    }

    pub async fn task_status_snapshot(&self, task_id: &str) -> Result<TaskStatusSnapshot> {
        let read = self.task_status_read(task_id).await?;
        Ok(read.snapshot)
    }

    pub async fn stop_task(
        &self,
        task_id: &str,
        _authority_class: &AuthorityClass,
    ) -> Result<TaskRecord> {
        let original = self
            .start_original_task_stop_owned(task_id)?
            .await
            .context("owned TaskStop worker stopped before its initial reply")??;
        Ok(original.task)
    }

    pub async fn task_input(&self, task_id: &str, input: &str) -> Result<TaskInputResult> {
        let _admission = self.execution_admission_lease()?;
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| task_not_found_error(task_id))?;
        let snapshot = TaskStatusSnapshot::from_task_record(&task);
        let command = snapshot.command.clone();
        if matches!(
            task.status,
            TaskStatus::Cancelling
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: command.and_then(|value| value.input_target),
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }
        if task.kind != TaskKind::CommandTask {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task does not support input delivery".into()),
            });
        }
        self.deliver_command_task_input(&task, snapshot, command, input)
            .await
    }

    pub async fn task_input_with_trust(
        &self,
        task_id: &str,
        input: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskInputResult> {
        let _admission = self.execution_admission_lease()?;
        let task = self
            .task_record(task_id)
            .await?
            .ok_or_else(|| task_not_found_error(task_id))?;
        let snapshot = TaskStatusSnapshot::from_task_record(&task);
        let command = snapshot.command.clone();
        if matches!(
            task.status,
            TaskStatus::Cancelling
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: command.clone().and_then(|value| value.input_target),
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }
        if task.kind == TaskKind::CommandTask {
            return self
                .deliver_command_task_input(&task, snapshot, command, input)
                .await;
        }
        if task.is_child_agent_task() {
            return self
                .deliver_child_task_input(&task, snapshot, input, authority_class)
                .await;
        }
        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: false,
            input_target: None,
            bytes_written: None,
            summary_text: Some("task does not support input delivery".into()),
        })
    }

    async fn deliver_command_task_input(
        &self,
        task: &TaskRecord,
        snapshot: TaskStatusSnapshot,
        command: Option<CommandTaskStatusSnapshot>,
        input: &str,
    ) -> Result<TaskInputResult> {
        if command.as_ref().and_then(|value| value.accepts_input) != Some(true) {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }

        let input_tx = {
            let handles = self.inner.task_handles.lock().await;
            match handles.get(&task.id) {
                Some(command_task::ManagedTaskHandle::Command(handle)) => handle.input_tx.clone(),
                Some(command_task::ManagedTaskHandle::Async(_)) => {
                    return Ok(TaskInputResult {
                        task: snapshot,
                        accepted_input: false,
                        input_target: None,
                        bytes_written: None,
                        summary_text: Some("task does not support input delivery".into()),
                    });
                }
                None => {
                    return Ok(TaskInputResult {
                        task: snapshot,
                        accepted_input: false,
                        input_target: command.and_then(|value| value.input_target),
                        bytes_written: None,
                        summary_text: Some("task is not currently accepting input".into()),
                    });
                }
            }
        };

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        input_tx
            .send(command_task::CommandTaskInputRequest {
                text: input.to_string(),
                response_tx,
            })
            .await
            .map_err(|_| {
                RuntimeError::validation(
                    "task_not_running",
                    format!("task {} is not currently running", task.id),
                )
                .with_safe_context("task_id", &task.id)
            })?;
        let bytes_written = response_rx
            .await
            .map_err(|_| {
                RuntimeError::new(
                    RuntimeErrorDomain::Task,
                    "task_input_interrupted",
                    format!("task {} input delivery was interrupted", task.id),
                )
                .with_safe_context("task_id", &task.id)
                .with_retryable(true)
            })?
            .map_err(|_| {
                RuntimeError::new(
                    RuntimeErrorDomain::Task,
                    "task_input_failed",
                    format!("task {} input delivery failed", task.id),
                )
                .with_safe_context("task_id", &task.id)
            })?;

        let input_target = command
            .as_ref()
            .and_then(|value| value.input_target.clone())
            .unwrap_or_else(|| "stdin".into());
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_input_delivered",
            serde_json::json!({
                "task_id": task.id,
                "task_kind": task.kind,
                "bytes_written": bytes_written,
                "input_target": input_target,
            }),
        ))?;

        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: true,
            input_target: Some(input_target),
            bytes_written: Some(bytes_written),
            summary_text: Some(format!("delivered input to task {}", task.id)),
        })
    }

    async fn deliver_child_task_input(
        &self,
        task: &TaskRecord,
        snapshot: TaskStatusSnapshot,
        input: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskInputResult> {
        let Some(child_agent_id) = detail_string(&task.detail, "child_agent_id") else {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task does not support input delivery".into()),
            });
        };
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        };

        let parent_agent_id = self.agent_id().await?;
        let delivered = bridge
            .deliver_child_followup(
                &parent_agent_id,
                &task.id,
                &child_agent_id,
                input,
                authority_class.clone(),
            )
            .await?;
        if !delivered {
            return Ok(TaskInputResult {
                task: snapshot,
                accepted_input: false,
                input_target: None,
                bytes_written: None,
                summary_text: Some("task is not currently accepting input".into()),
            });
        }

        let bytes_written = input.len() as u64;
        let input_target = "child_followup";
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_input_delivered",
            serde_json::json!({
                "task_id": task.id,
                "task_kind": task.kind,
                "child_agent_id": child_agent_id,
                "bytes_written": bytes_written,
                "input_target": input_target,
            }),
        ))?;

        Ok(TaskInputResult {
            task: snapshot,
            accepted_input: true,
            input_target: Some(input_target.into()),
            bytes_written: Some(bytes_written),
            summary_text: Some(format!(
                "delivered parent follow-up to child {} via supervision task {}",
                child_agent_id, task.id
            )),
        })
    }

    pub async fn create_work_item(
        &self,
        objective: String,
        plan_status: Option<WorkItemPlanStatus>,
        plan: Option<String>,
        todo_list: Vec<TodoItem>,
    ) -> Result<WorkItemRecord> {
        let _admission = self.execution_admission_lease()?;
        let agent_id = self.agent_id().await?;
        let mut record = WorkItemRecord::new(agent_id.clone(), objective, WorkItemState::Open);
        if let Some(plan_status) = plan_status {
            record.plan_status = plan_status;
        }
        record.plan_artifact = Some(crate::work_item_plan::ensure_plan_artifact(
            self.agent_home().as_path(),
            &record,
            plan.as_deref(),
        )?);
        record.todo_list = todo_list;
        record.workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id)
            .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string());
        let execution_protocol = self.plan_work_item_execution_create(&record)?;
        let commit = {
            let mut mutation = self.begin_mutation().await?;
            mutation
                .commit_work_transition(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::WorkItemTransitionCommand {
                        agent_id,
                        mutation: crate::runtime_db::transitions::WorkItemMutation::Insert {
                            record: record.clone(),
                        },
                        agent_state: None,
                        brief_evidence: Vec::new(),
                        audit_events: vec![self.work_item_written_event(
                            "created",
                            &record,
                            Value::Null,
                        )],
                        index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                    },
                    execution_protocol,
                    true,
                )
                .await
        }?;
        self.apply_transition_commit(commit).await;
        Ok(record)
    }

    fn plan_work_item_execution_create(
        &self,
        record: &WorkItemRecord,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            ExecutionProtocolCommand, ExecutionProtocolState, RegisterWorkItemExecution,
            WorkItemExecutionRecord, WorkItemExecutionState,
        };

        let existing = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&record.agent_id)?;
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: existing
                    .is_none()
                    .then(|| ExecutionProtocolState::empty(&record.agent_id)),
                commands: vec![ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                    RegisterWorkItemExecution {
                        work_item_id: record.id.clone(),
                        record: WorkItemExecutionRecord {
                            source_revision: record.revision,
                            state: WorkItemExecutionState::Runnable {
                                generation: record.revision,
                                recovery_ref: None,
                            },
                        },
                    },
                ))],
            },
        )
    }

    fn plan_work_item_execution_update(
        &self,
        record: &WorkItemRecord,
        readiness_mutation: Option<WorkItemReadinessMutation>,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            AdvanceWorkItemSourceRevision, ExecutionProtocolCommand, ExecutionProtocolState,
            RegisterWorkItemExecution, SetWorkItemReadiness, WorkItemExecutionRecord,
            WorkItemExecutionState,
        };

        let existing = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&record.agent_id)?;
        let authoritative = existing
            .as_ref()
            .and_then(|state| state.work_items.get(&record.id));
        let Some(authoritative) = authoritative else {
            let state = match readiness_mutation {
                Some(WorkItemReadinessMutation::Pause(reason)) => WorkItemExecutionState::Paused {
                    generation: record.revision,
                    reason,
                },
                _ => WorkItemExecutionState::Runnable {
                    generation: record.revision,
                    recovery_ref: None,
                },
            };
            return Ok(
                crate::runtime_db::transitions::ExecutionProtocolTransition {
                    bootstrap: existing
                        .is_none()
                        .then(|| ExecutionProtocolState::empty(&record.agent_id)),
                    commands: vec![ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                        RegisterWorkItemExecution {
                            work_item_id: record.id.clone(),
                            record: WorkItemExecutionRecord {
                                source_revision: record.revision,
                                state,
                            },
                        },
                    ))],
                },
            );
        };
        if readiness_mutation.is_some()
            && matches!(
                authoritative.state,
                WorkItemExecutionState::InFlight { .. }
                    | WorkItemExecutionState::Waiting { .. }
                    | WorkItemExecutionState::NeedsRepair { .. }
            )
        {
            return Err(anyhow!(
                "cannot change WorkItem readiness while canonical execution state is active or requires repair"
            ));
        }
        let next_state = match (&authoritative.state, readiness_mutation) {
            (WorkItemExecutionState::InFlight { .. }, _)
            | (WorkItemExecutionState::NeedsRepair { .. }, _)
            | (WorkItemExecutionState::Waiting { .. }, None)
            | (WorkItemExecutionState::Waiting { .. }, Some(WorkItemReadinessMutation::Pause(_)))
            | (WorkItemExecutionState::Waiting { .. }, Some(WorkItemReadinessMutation::Resume))
            | (_, None) => None,
            (
                WorkItemExecutionState::Runnable { generation, .. },
                Some(WorkItemReadinessMutation::Pause(reason)),
            )
            | (
                WorkItemExecutionState::Paused { generation, .. },
                Some(WorkItemReadinessMutation::Pause(reason)),
            ) => Some(WorkItemExecutionState::Paused {
                generation: if matches!(
                    authoritative.state,
                    WorkItemExecutionState::Runnable { .. }
                ) {
                    generation
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("WorkItem scheduling generation overflow"))?
                } else {
                    *generation
                },
                reason,
            }),
            (
                WorkItemExecutionState::Runnable { generation, .. },
                Some(WorkItemReadinessMutation::Resume),
            )
            | (
                WorkItemExecutionState::Paused { generation, .. },
                Some(WorkItemReadinessMutation::Resume),
            ) => Some(WorkItemExecutionState::Runnable {
                generation: if matches!(authoritative.state, WorkItemExecutionState::Paused { .. })
                {
                    generation
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("WorkItem scheduling generation overflow"))?
                } else {
                    *generation
                },
                recovery_ref: None,
            }),
            (WorkItemExecutionState::Terminal { .. }, _) => {
                return Err(anyhow!("cannot update terminal WorkItem execution state"));
            }
        };
        let command = if let Some(state) = next_state {
            ExecutionProtocolCommand::SetWorkItemReadiness(Box::new(SetWorkItemReadiness {
                command_id: format!("work_item:update:{}:{}", record.id, record.revision),
                work_item_id: record.id.clone(),
                expected: authoritative.clone(),
                record: WorkItemExecutionRecord {
                    source_revision: record.revision,
                    state,
                },
            }))
        } else {
            ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(AdvanceWorkItemSourceRevision {
                command_id: format!("work_item:update:{}:{}", record.id, record.revision),
                work_item_id: record.id.clone(),
                expected_source_revision: authoritative.source_revision,
                source_revision: record.revision,
            })
        };
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: None,
                commands: vec![command],
            },
        )
    }

    fn plan_work_item_execution_clear_blocker(
        &self,
        record: &WorkItemRecord,
        wait_conditions: &[crate::types::WaitConditionRecord],
        read: &WorkCommandRead,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            AdvanceWorkItemSourceRevision, ExecutionProtocolCommand, ExecutionProtocolState,
            RegisterWorkItemExecution, SetWorkItemReadiness, WorkItemExecutionRecord,
            WorkItemExecutionState,
        };

        let existing = read.protocol.as_ref();
        let Some(authoritative) = existing
            .as_ref()
            .and_then(|state| state.work_items.get(&record.id))
        else {
            return Ok(
                crate::runtime_db::transitions::ExecutionProtocolTransition {
                    bootstrap: existing
                        .is_none()
                        .then(|| ExecutionProtocolState::empty(&record.agent_id)),
                    commands: vec![ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                        RegisterWorkItemExecution {
                            work_item_id: record.id.clone(),
                            record: WorkItemExecutionRecord {
                                source_revision: record.revision,
                                state: WorkItemExecutionState::Runnable {
                                    generation: record.revision,
                                    recovery_ref: None,
                                },
                            },
                        },
                    ))],
                },
            );
        };
        let revision_changed = authoritative.source_revision != record.revision;
        let command = match &authoritative.state {
            WorkItemExecutionState::Waiting { generation, .. }
            | WorkItemExecutionState::Paused { generation, .. } => Some(
                ExecutionProtocolCommand::SetWorkItemReadiness(Box::new(SetWorkItemReadiness {
                    command_id: format!(
                        "work_item:clear_blocker:{}:{}",
                        record.id, record.revision
                    ),
                    work_item_id: record.id.clone(),
                    expected: authoritative.clone(),
                    record: WorkItemExecutionRecord {
                        source_revision: record.revision,
                        state: WorkItemExecutionState::Runnable {
                            generation: generation.checked_add(1).ok_or_else(|| {
                                anyhow!("WorkItem scheduling generation overflow")
                            })?,
                            recovery_ref: None,
                        },
                    },
                })),
            ),
            WorkItemExecutionState::Runnable { generation, .. } if revision_changed => Some(
                ExecutionProtocolCommand::SetWorkItemReadiness(Box::new(SetWorkItemReadiness {
                    command_id: format!(
                        "work_item:clear_blocker:{}:{}",
                        record.id, record.revision
                    ),
                    work_item_id: record.id.clone(),
                    expected: authoritative.clone(),
                    record: WorkItemExecutionRecord {
                        source_revision: record.revision,
                        state: WorkItemExecutionState::Runnable {
                            generation: *generation,
                            recovery_ref: None,
                        },
                    },
                })),
            ),
            WorkItemExecutionState::InFlight { .. }
            | WorkItemExecutionState::NeedsRepair { .. }
                if revision_changed =>
            {
                Some(ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(
                    AdvanceWorkItemSourceRevision {
                        command_id: format!(
                            "work_item:clear_blocker:{}:{}",
                            record.id, record.revision
                        ),
                        work_item_id: record.id.clone(),
                        expected_source_revision: authoritative.source_revision,
                        source_revision: record.revision,
                    },
                ))
            }
            WorkItemExecutionState::Terminal { .. } => {
                return Err(anyhow!("cannot clear terminal WorkItem execution state"));
            }
            _ => None,
        };
        if command.is_none() && !wait_conditions.is_empty() {
            return Err(anyhow!(
                "cancelled WorkItem waits require a canonical readiness transition"
            ));
        }
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: None,
                commands: command.into_iter().collect(),
            },
        )
    }

    fn plan_work_item_execution_continuation_suspend(
        &self,
        parent: &WorkItemRecord,
        continuation: &WorkItemContinuationFrame,
        read: &WorkCommandRead,
    ) -> Result<Option<crate::domain::execution_protocol::ExecutionProtocolCommand>> {
        use crate::domain::execution_protocol::{
            ExecutionProtocolCommand, SuspendWorkItemContinuation,
        };

        let state = read
            .protocol
            .as_ref()
            .ok_or_else(|| anyhow!("continuation suspension requires canonical execution state"))?;
        let authoritative = state
            .work_items
            .get(&parent.id)
            .ok_or_else(|| anyhow!("continuation suspension parent execution state is missing"))?;
        anyhow::ensure!(
            authoritative.source_revision == parent.revision,
            "continuation suspension parent WorkItem fence is stale"
        );
        Ok(Some(ExecutionProtocolCommand::SuspendWorkItemContinuation(
            Box::new(SuspendWorkItemContinuation {
                command_id: format!("continuation:suspend:{}", continuation.id),
                work_item_id: parent.id.clone(),
                active_work_item_id: continuation.active_work_item_id.clone(),
                continuation_id: continuation.id.clone(),
                expected: authoritative.clone(),
            }),
        )))
    }

    fn plan_work_item_execution_continuation_resume(
        &self,
        continuation: &WorkItemContinuationFrame,
        command_reason: &str,
        projected_parent: Option<&WorkItemRecord>,
        projected_wait_conditions: &[WaitConditionRecord],
        read: &WorkCommandRead,
    ) -> Result<Option<crate::domain::execution_protocol::ExecutionProtocolCommand>> {
        use crate::domain::execution_protocol::{
            ExecutionProtocolCommand, ResumeWorkItemContinuation,
        };

        let state = read
            .protocol
            .as_ref()
            .ok_or_else(|| anyhow!("continuation resume requires canonical execution state"))?;
        let authoritative = state
            .work_items
            .get(&continuation.suspended_work_item_id)
            .ok_or_else(|| anyhow!("continuation resume parent execution state is missing"))?;
        let (source, outcome) = read.continuation_resume_source(
            &continuation.suspended_work_item_id,
            projected_parent,
            projected_wait_conditions,
        )?;
        Ok(Some(ExecutionProtocolCommand::ResumeWorkItemContinuation(
            Box::new(ResumeWorkItemContinuation {
                command_id: format!("continuation:{command_reason}:{}", continuation.id),
                work_item_id: continuation.suspended_work_item_id.clone(),
                active_work_item_id: continuation.active_work_item_id.clone(),
                continuation_id: continuation.id.clone(),
                expected: authoritative.clone(),
                source,
                outcome,
            }),
        )))
    }

    pub async fn pick_work_item(
        &self,
        work_item_id: String,
    ) -> Result<(Option<WorkItemRecord>, WorkItemRecord)> {
        let picked = self.pick_work_item_with_reason(work_item_id, None).await?;
        Ok((picked.previous_work_item, picked.current_work_item))
    }

    pub async fn pick_work_item_with_reason(
        &self,
        work_item_id: String,
        reason: Option<String>,
    ) -> Result<PickedWorkItem> {
        self.pick_work_item_with_reason_and_clear_blocker(work_item_id, reason, false)
            .await
    }

    pub async fn pick_work_item_with_reason_and_clear_blocker(
        &self,
        work_item_id: String,
        reason: Option<String>,
        clear_blocker: bool,
    ) -> Result<PickedWorkItem> {
        let _admission = self.execution_admission_lease()?;
        let agent_id = self.agent_id().await?;
        let mut record = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if record.state != WorkItemState::Open {
            return Err(RuntimeError::validation(
                "work_item_not_open",
                format!("cannot pick non-open work item {work_item_id}"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into());
        }
        let read = self
            .prepare_work_command_read(&record, WorkCommandReadMode::Focus { clear_blocker })
            .await?;
        let state = read.state.as_ref().clone();
        let current_id = state.current_work_item_id.clone();
        let previous = current_id
            .as_deref()
            .map(|id| read.work.work_item(id))
            .transpose()?
            .flatten()
            .cloned();
        let normalized_reason = reason.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
        if clear_blocker && normalized_reason.is_none() {
            return Err(anyhow!(
                "PickWorkItem clear_blocker requires a non-empty reason"
            ));
        }
        let blocker_clearance = if clear_blocker {
            self.clear_work_item_blocker_for_pick(
                &agent_id,
                record,
                &read.work,
                normalized_reason
                    .as_deref()
                    .expect("clear_blocker reason validated"),
            )
            .await?
        } else {
            WorkItemBlockerClearance::unchanged(record)
        };
        let WorkItemBlockerClearance {
            work_item,
            expected_revision,
            wait_conditions,
            mut audit_events,
            index_changes,
            blocker_cleared,
            cancelled_wait_condition_ids,
        } = blocker_clearance;
        record = work_item;
        let switching = current_id.as_deref().is_some_and(|id| id != record.id);
        let work_queue = read
            .queue
            .as_ref()
            .context("Focus command queue projection missing")?;
        let previous_readiness = previous.as_ref().map(|record| {
            work_queue
                .items
                .iter()
                .find(|item| item.work_item.id == record.id)
                .map(|item| item.readiness)
                .unwrap_or_else(|| record.readiness())
        });
        let current_readiness = if blocker_cleared {
            record.readiness()
        } else {
            work_queue
                .items
                .iter()
                .find(|item| item.work_item.id == record.id)
                .map(|item| item.readiness)
                .unwrap_or_else(|| record.readiness())
        };
        let mut warnings = Vec::new();
        let mut continuation_created = None;
        let mut continuation_resolved = None;
        let mut continuation_expectations = Vec::new();
        let mut continuation_records = Vec::new();
        let active_continuations = read
            .active_frames()?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let by_suspended = active_continuations_by_suspended(&active_continuations)?;
        let target_was_yielded = by_suspended.contains_key(record.id.as_str());
        if target_was_yielded {
            let analysis = analyze_continuation_path(
                &record.id,
                current_id.as_deref(),
                &active_continuations,
            )?;
            if let ContinuationPathTermination::MissingLink {
                missing_work_item_id,
                originating_continuation_id,
            } = &analysis.termination
            {
                return Err(RuntimeError::new(
                    RuntimeErrorDomain::Conflict,
                    "continuation_unwind_incomplete",
                    format!(
                        "continuation unwind from work item {} is disconnected from current focus; chain breaks at work item {} after continuation {}",
                        record.id, missing_work_item_id, originating_continuation_id
                    ),
                )
                .with_safe_context("work_item_id", missing_work_item_id)
                .with_safe_context("record_id", originating_continuation_id)
                .with_recovery_hint(
                    "refresh continuation state or run scheduler recovery before retrying the pick",
                )
                .into());
            }
            for (index, frame) in analysis.frames.iter().enumerate() {
                let (resolved, reason, event_kind) = if index == 0 {
                    (
                        frame.clone().resume("explicit_pick"),
                        "explicit_pick",
                        "work_item_continuation_resumed",
                    )
                } else {
                    let reason = match &analysis.termination {
                        ContinuationPathTermination::NoCurrentFocus => {
                            "orphaned_descendant_without_current_focus"
                        }
                        ContinuationPathTermination::ReachesCurrent => "current_focus_reselected",
                        ContinuationPathTermination::MissingLink { .. } => unreachable!(),
                    };
                    (
                        frame.clone().cancel(reason),
                        reason,
                        "work_item_continuation_cancelled",
                    )
                };
                let summary = continuation_summary(&resolved, reason);
                if index == 0 {
                    continuation_resolved = Some(summary.clone());
                }
                audit_events.push(AuditEvent::legacy(
                    event_kind,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "continuation": summary,
                    }),
                ));
                continuation_expectations.push(frame.clone());
                continuation_records.push(resolved);
            }
        } else if let Some(id) = current_id.as_deref() {
            let mut resolved_frame_ids = BTreeSet::new();
            let mut parent_id = id;
            while let Some(frame) = by_suspended.get(parent_id).copied() {
                anyhow::ensure!(
                    resolved_frame_ids.insert(frame.id.clone()),
                    "continuation cancellation path contains a cycle"
                );
                let cancelled = frame.clone().cancel("current_focus_reselected");
                audit_events.push(AuditEvent::legacy(
                    "work_item_continuation_cancelled",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "continuation": continuation_summary(
                            &cancelled,
                            "current_focus_reselected"
                        ),
                    }),
                ));
                continuation_expectations.push(frame.clone());
                continuation_records.push(cancelled);
                parent_id = frame.active_work_item_id.as_str();
            }
        }
        let yield_current = switching
            && !target_was_yielded
            && previous_readiness == Some(WorkItemReadiness::Runnable)
            && previous
                .as_ref()
                .is_some_and(|record| record.state == WorkItemState::Open)
            && record.state == WorkItemState::Open;
        // A yield records a continuation only when the pick also suspends the
        // current WorkItem. An execution-bound current WorkItem keeps its
        // activation, so a pick away from it is a terminal focus transition:
        // nothing suspends the current WorkItem and no return path exists.
        // Writing a continuation frame there would leave an active frame whose
        // suspended WorkItem is not paused-yielded, and resolving that frame
        // when the new focus later completes would fail the exact-yield fence.
        let terminal_transition = yield_current
            && state
                .current_execution_binding
                .as_ref()
                .is_some_and(|binding| {
                    binding.activation_id.is_some()
                        && binding.work_item_id.as_deref() == current_id.as_deref()
                });
        let records_continuation = yield_current && !terminal_transition;
        if yield_current {
            if let Some(existing) = active_continuations
                .iter()
                .find(|frame| frame.active_work_item_id == record.id)
            {
                return Err(anyhow!(
                    "cannot yield to work item {} because continuation {} already uses it as active work item",
                    record.id,
                    existing.id
                ));
            }
            if records_continuation {
                if let Some(previous) = previous.as_ref() {
                    let frame = WorkItemContinuationFrame::new_on_completed(
                        agent_id.clone(),
                        previous.id.clone(),
                        record.id.clone(),
                        state.current_turn_id.clone(),
                    );
                    continuation_created = Some(continuation_summary(&frame, "pick_work_item"));
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_created",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "continuation": continuation_summary(&frame, "pick_work_item"),
                        }),
                    ));
                    continuation_records.push(frame);
                }
            }
        } else if switching
            && previous_readiness == Some(WorkItemReadiness::Runnable)
            && normalized_reason.is_none()
            && !target_was_yielded
        {
            warnings.push(WorkItemFocusTransitionWarning {
                code: "missing_pick_reason_for_runnable_focus_switch".into(),
                message: "PickWorkItem switched away from a runnable current work item without a reason; include reason on future explicit focus overrides.".into(),
            });
        }
        let switch_kind = if !switching {
            "same_work_item"
        } else if yield_current {
            "yield_current"
        } else if continuation_resolved.is_some() {
            "explicit_yield_return"
        } else if previous_readiness == Some(WorkItemReadiness::Runnable) {
            "explicit_focus_override"
        } else {
            "explicit_focus_pick"
        }
        .to_string();
        let current_focus_mode = if current_readiness == WorkItemReadiness::Runnable {
            "runnable"
        } else {
            "inspection"
        }
        .to_string();
        let transition = WorkItemFocusTransition {
            previous_work_item_id: current_id.clone(),
            current_work_item_id: record.id.clone(),
            reason: normalized_reason,
            previous_readiness,
            current_readiness,
            switch_kind,
            terminal_transition,
            current_focus_mode,
            blocker_cleared,
            cancelled_wait_condition_ids,
            warnings,
        };
        audit_events.push(AuditEvent::legacy(
            "work_item_picked",
            serde_json::json!({
                "agent_id": agent_id,
                "previous_work_item_id": transition.previous_work_item_id.clone(),
                "current_work_item_id": transition.current_work_item_id.clone(),
                "reason": transition.reason.clone(),
                "previous_readiness": transition.previous_readiness,
                "current_readiness": transition.current_readiness,
                "switch_kind": transition.switch_kind.clone(),
                "terminal_transition": transition.terminal_transition,
                "current_focus_mode": transition.current_focus_mode.clone(),
                "blocker_cleared": transition.blocker_cleared,
                "cancelled_wait_condition_ids": transition.cancelled_wait_condition_ids.clone(),
                "warnings": transition.warnings.clone(),
                "continuation_created": continuation_created.clone(),
                "continuation_resolved": continuation_resolved.clone(),
            }),
        ));
        let mut next_state = state.clone();
        next_state.current_work_item_id = Some(record.id.clone());
        if !terminal_transition {
            next_state.current_turn_work_item_id = Some(record.id.clone());
        }
        let work_items = expected_revision
            .map(|expected_revision| {
                vec![crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision,
                }]
            })
            .unwrap_or_default();
        let mut execution_protocol = if blocker_cleared {
            self.plan_work_item_execution_clear_blocker(&record, &wait_conditions, &read)?
        } else {
            Default::default()
        };
        for continuation in &continuation_records {
            if continuation.state == WorkItemContinuationState::Active {
                continue;
            }
            let command_reason = match continuation.state {
                WorkItemContinuationState::Resumed => "resume",
                WorkItemContinuationState::Cancelled => "cancel",
                WorkItemContinuationState::Active => unreachable!(),
            };
            if let Some(command) = self.plan_work_item_execution_continuation_resume(
                continuation,
                command_reason,
                (record.id == continuation.suspended_work_item_id).then_some(&record),
                &wait_conditions,
                &read,
            )? {
                execution_protocol.commands.push(command);
            }
        }
        if yield_current && !terminal_transition {
            let previous = previous
                .as_ref()
                .expect("yielding current focus requires the previous WorkItem");
            let continuation = continuation_records
                .iter()
                .find(|frame| {
                    frame.state == WorkItemContinuationState::Active
                        && frame.suspended_work_item_id == previous.id
                        && frame.active_work_item_id == record.id
                })
                .expect("yielding current focus creates an active continuation");
            if let Some(command) =
                self.plan_work_item_execution_continuation_suspend(previous, continuation, &read)?
            {
                execution_protocol.commands.push(command);
            }
        }
        let commit = {
            let mut mutation = self.begin_mutation().await?;
            mutation
                .commit_focus_transition(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
                        agent_id: agent_id.clone(),
                        work_items,
                        wait_conditions,
                        active_continuation_expectations: Some(active_continuations),
                        continuation_expectations,
                        continuations: continuation_records,
                        agent_state: crate::runtime_db::transitions::AgentStateMutation {
                            expected: Some(Box::new(state)),
                            record: Box::new(next_state),
                        },
                        brief_evidence: Vec::new(),
                        audit_events,
                        index_changes,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                    },
                    execution_protocol,
                    true,
                    None,
                )
                .await
        }?;
        self.apply_transition_commit(commit).await;
        if blocker_cleared {
            let wake_at = self.now();
            self.apply_task_result_operation((&record.id, wake_at), || {
                crate::runtime_db::transitions::TaskResultOperation::Wake {
                    work_item_id: record.id.clone(),
                    at: wake_at,
                }
            })
            .await?
            .count()?;
        }
        Ok(PickedWorkItem {
            previous_work_item: previous,
            current_work_item: record,
            transition,
            continuation_created,
            continuation_resolved,
        })
    }

    pub async fn update_work_item_fields(
        &self,
        work_item_id: String,
        objective: Option<String>,
        plan_status: Option<WorkItemPlanStatus>,
        _plan: Option<Option<String>>,
        todo_list: Option<Vec<TodoItem>>,
        blocked_by: Option<Option<String>>,
    ) -> Result<WorkItemRecord> {
        self.update_work_item_fields_with_recheck(
            work_item_id,
            objective,
            plan_status,
            _plan,
            todo_list,
            blocked_by,
            None,
        )
        .await
    }

    pub async fn update_work_item_fields_with_recheck(
        &self,
        work_item_id: String,
        objective: Option<String>,
        plan_status: Option<WorkItemPlanStatus>,
        _plan: Option<Option<String>>,
        todo_list: Option<Vec<TodoItem>>,
        blocked_by: Option<Option<String>>,
        recheck_after_ms: Option<u64>,
    ) -> Result<WorkItemRecord> {
        let _admission = self.execution_admission_lease()?;
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state != WorkItemState::Open {
            return Err(RuntimeError::validation(
                "work_item_not_open",
                format!("cannot update non-open work item {work_item_id}"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into());
        }
        let mut record = existing.clone();
        let mut wrote_item = false;
        let previous_objective = record.objective.clone();
        let clear_blocker = blocked_by.as_ref().is_some_and(Option::is_none);
        let focus_release_reason = blocked_by
            .as_ref()
            .is_some_and(Option::is_some)
            .then_some("work_item_blocked");
        if let Some(objective) = objective {
            record.objective = objective;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        if let Some(plan_status) = plan_status {
            record.plan_status = plan_status;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        if let Some(todo_list) = todo_list {
            record.todo_list = todo_list;
            record.updated_at = Utc::now();
            wrote_item = true;
        }
        let readiness_mutation = blocked_by.as_ref().map(|blocked_by| match blocked_by {
            Some(reason) => WorkItemReadinessMutation::Pause(reason.clone()),
            None => WorkItemReadinessMutation::Resume,
        });
        if let Some(blocked_by) = blocked_by {
            let now = self.now();
            record.blocked_by = blocked_by;
            // An explicit operator/agent blocker is never wait-derived.
            record.blocked_by_wait_id = None;
            match record.blocked_by {
                Some(_) => {
                    let recheck_after_ms = recheck_after_ms.unwrap_or(60 * 60 * 1000);
                    let recheck_after_ms = i64::try_from(recheck_after_ms).unwrap_or(i64::MAX);
                    let recheck_after = chrono::Duration::try_milliseconds(recheck_after_ms)
                        .unwrap_or(chrono::Duration::MAX);
                    record.recheck_at = Some(now + recheck_after);
                    record.recheck_consumed_at = None;
                }
                None => {
                    record.recheck_at = None;
                    record.recheck_consumed_at = None;
                }
            }
            record.updated_at = now;
            wrote_item = true;
        }
        if wrote_item {
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut record,
            )?;
            record.revision = existing.revision + 1;
            let mut audit_events = Vec::new();
            if plan_artifact_changed && record.plan_artifact != existing.plan_artifact {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                    audit_events.push(event);
                }
            }
            audit_events.push(self.work_item_written_event(
                "updated",
                &record,
                serde_json::json!({
                    "previous_objective_preview": crate::types::truncate_audit_preview(
                        &previous_objective,
                        600
                    ),
                    "objective_changed": previous_objective != record.objective,
                }),
            ));
            let mut state = self.agent_state().await?;
            let expected_state = state.clone();
            let mut agent_state = None;
            if let Some(reason) = focus_release_reason {
                let release_current =
                    state.current_work_item_id.as_deref() == Some(record.id.as_str());
                let release_turn =
                    state.current_turn_work_item_id.as_deref() == Some(record.id.as_str());
                if release_current {
                    state.current_work_item_id = None;
                }
                if release_turn {
                    state.current_turn_work_item_id = None;
                }
                if release_current || release_turn {
                    audit_events.push(AuditEvent::legacy(
                        "work_item_focus_released",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "work_item_id": record.id.as_str(),
                            "reason": reason,
                            "readiness": record.readiness(),
                            "revision": record.revision,
                        }),
                    ));
                    agent_state = Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_state)),
                        record: Box::new(state),
                    });
                }
            }
            let execution_protocol =
                self.plan_work_item_execution_update(&record, readiness_mutation)?;
            let commit = {
                let mut mutation = self.begin_mutation().await?;
                mutation
                    .commit_work_transition(
                        crate::ids::audit_event_id(),
                        crate::runtime_db::transitions::WorkItemTransitionCommand {
                            agent_id,
                            mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                                record: record.clone(),
                                expected_revision: existing.revision,
                            },
                            agent_state,
                            brief_evidence: Vec::new(),
                            audit_events,
                            index_changes: self
                                .inner
                                .storage
                                .index_changes_for_work_item(&record)?,
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                        },
                        execution_protocol,
                        true,
                    )
                    .await
            }?;
            self.apply_transition_commit(commit).await;
            if clear_blocker {
                let wake_at = self.now();
                self.apply_task_result_operation((&record.id, wake_at), || {
                    crate::runtime_db::transitions::TaskResultOperation::Wake {
                        work_item_id: record.id.clone(),
                        at: wake_at,
                    }
                })
                .await?
                .count()?;
            }
        }
        Ok(record)
    }

    pub async fn consume_work_item_recheck(
        &self,
        work_item_id: &str,
    ) -> Result<Option<WorkItemRecord>> {
        let agent_id = self.agent_id().await?;
        let existing = match self.validate_owned_work_item(&agent_id, work_item_id) {
            Ok(record) => record,
            Err(_) => return Ok(None),
        };
        if existing.state != WorkItemState::Open || existing.blocked_by.is_none() {
            return Ok(None);
        }
        let Some(recheck_at) = existing.recheck_at else {
            return Ok(None);
        };
        if existing
            .recheck_consumed_at
            .is_some_and(|consumed_at| consumed_at >= recheck_at)
        {
            return Ok(Some(existing));
        }

        let consumed_at = self.now();
        let mut record = WorkItemRecord {
            revision: existing.revision + 1,
            recheck_consumed_at: Some(consumed_at),
            updated_at: consumed_at,
            ..existing
        };
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut record,
        )?;
        let mut audit_events = Vec::new();
        if plan_artifact_changed {
            if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                audit_events.push(event);
            }
        }
        audit_events.push(AuditEvent::legacy(
            "work_item_recheck_consumed",
            serde_json::json!({
                "work_item_id": record.id.clone(),
                "revision": record.revision,
                "recheck_at": record.recheck_at,
                "recheck_consumed_at": record.recheck_consumed_at,
            }),
        ));
        let commit = {
            let mut mutation = self.begin_mutation().await?;
            mutation
                .commit_work_transition(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::WorkItemTransitionCommand {
                        agent_id,
                        mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                            record: record.clone(),
                            expected_revision: existing.revision,
                        },
                        agent_state: None,
                        brief_evidence: Vec::new(),
                        audit_events,
                        index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                    },
                    crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                    true,
                )
                .await
        }?;
        self.apply_transition_commit(commit).await;
        Ok(Some(record))
    }

    pub async fn complete_work_item(
        &self,
        work_item_id: String,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemRecord> {
        Ok(self
            .complete_work_item_with_continuation(work_item_id, warnings)
            .await?
            .work_item)
    }

    pub async fn complete_work_item_with_continuation(
        &self,
        work_item_id: String,
        warnings: Vec<serde_json::Value>,
    ) -> Result<CompletedWorkItem> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state != WorkItemState::Open {
            return Ok(CompletedWorkItem {
                work_item: existing,
                continuation_resumed: None,
            });
        }
        let read = self
            .prepare_work_command_read(&existing, WorkCommandReadMode::CompletionIntent)
            .await?;
        let state = read.state.as_ref().clone();
        let mut next_state = state.clone();
        let now = Utc::now();
        let execution_binding = state.current_execution_binding.clone();
        let matching_execution_binding = execution_binding
            .as_ref()
            .filter(|binding| binding.work_item_id.as_deref() == Some(existing.id.as_str()));
        let completion_intent = WorkItemCompletionIntent {
            work_item_id: existing.id.clone(),
            source_activation_id: matching_execution_binding
                .and_then(|binding| binding.activation_id.clone()),
            source_message_id: matching_execution_binding
                .map(|binding| binding.source_message_id.clone()),
            source_turn_id: matching_execution_binding.map(|binding| binding.turn_id.clone()),
            expected_work_revision: matching_execution_binding
                .and_then(|binding| binding.claimed_work_revision)
                .unwrap_or(existing.revision),
            report_requirement: CompletionReportRequirement::Required,
            report_state: if existing.result_brief_id.is_some() {
                CompletionReportState::Bound
            } else {
                CompletionReportState::Pending
            },
            result_brief_id: existing.result_brief_id.clone(),
            created_at: now,
            updated_at: now,
        };
        let mut record = WorkItemRecord {
            revision: existing.revision + 1,
            state: WorkItemState::Completing,
            blocked_by: None,
            recheck_at: None,
            recheck_consumed_at: None,
            result_brief_id: existing.result_brief_id.clone(),
            result_summary: existing.result_summary.clone(),
            completion_intent: Some(completion_intent),
            updated_at: now,
            ..existing
        };
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut record,
        )?;
        let mut audit_events = Vec::new();
        if plan_artifact_changed {
            if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                audit_events.push(event);
            }
        }
        let active_waits = read
            .active_waits_for(&record.id)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut wait_conditions = Vec::with_capacity(active_waits.len());
        let mut cancelled_wait_condition_ids = Vec::with_capacity(active_waits.len());
        for condition in active_waits {
            let mut cancelled = condition.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at = now;
            cancelled.cancelled_at = Some(now);
            cancelled_wait_condition_ids.push(condition.id);
            wait_conditions.push(cancelled);
        }
        let release_current =
            next_state.current_work_item_id.as_deref() == Some(record.id.as_str());
        let release_turn =
            next_state.current_turn_work_item_id.as_deref() == Some(record.id.as_str());
        if release_current {
            next_state.current_work_item_id = None;
        }
        if release_turn {
            next_state.current_turn_work_item_id = None;
        }
        if release_current || release_turn {
            audit_events.push(AuditEvent::legacy(
                "work_item_focus_released",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id,
                    "reason": "completion_intent_recorded",
                    "readiness": record.readiness(),
                    "revision": record.revision,
                }),
            ));
        }
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id,
                    "reason": "completion_intent_recorded",
                    "wait_condition_ids": cancelled_wait_condition_ids,
                }),
            ));
        }
        audit_events.push(self.work_item_written_event(
            "completing",
            &record,
            serde_json::json!({
                "warning_count": warnings.len(),
                "completion_intent": record.completion_intent,
            }),
        ));
        let commit = {
            let mut mutation = self.begin_mutation().await?;
            mutation
                .commit_focus_transition(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
                        agent_id,
                        work_items: vec![
                            crate::runtime_db::transitions::WorkItemMutation::Update {
                                record: record.clone(),
                                expected_revision: existing.revision,
                            },
                        ],
                        wait_conditions,
                        active_continuation_expectations: None,
                        continuation_expectations: Vec::new(),
                        continuations: Vec::new(),
                        agent_state: crate::runtime_db::transitions::AgentStateMutation {
                            expected: Some(Box::new(state)),
                            record: Box::new(next_state),
                        },
                        brief_evidence: Vec::new(),
                        audit_events,
                        index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                    },
                    Default::default(),
                    true,
                    None,
                )
                .await
        }?;
        self.apply_transition_commit(commit).await;
        Ok(CompletedWorkItem {
            work_item: record,
            continuation_resumed: None,
        })
    }

    pub async fn promote_work_item_completion_report(
        &self,
        work_item_id: String,
        report_text: String,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemRecord> {
        Ok(self
            .promote_work_item_completion_report_with_metadata(
                work_item_id,
                report_text,
                Vec::new(),
                source_turn_index,
                source_round,
                warnings,
            )
            .await?
            .into_record())
    }

    pub(crate) async fn complete_work_item_with_report(
        &self,
        work_item_id: String,
        authority: WorkItemCompletionAuthority,
        report_text: String,
        citations: Vec<crate::types::Citation>,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        source_turn_id: Option<String>,
        source_message_id: Option<String>,
        source_assistant_round_id: Option<String>,
        source_tool_call_id: Option<String>,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemCompletionReportPromotionOutcome> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state == WorkItemState::Completed {
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(
                existing,
            ));
        }
        if existing.state != WorkItemState::Open {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_completion_in_progress",
                format!(
                    "work item {work_item_id} already has a legacy completion intent in progress"
                ),
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_recovery_hint(
                "complete the existing legacy completion intent through the control completion API",
            )
            .into());
        }
        let report_text = report_text.trim();
        if report_text.is_empty() {
            return Err(RuntimeError::validation(
                "missing_completion_report",
                "CompleteWorkItem requires nearby preceding same-round operator-facing report text",
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_recovery_hint(
                "write the concise operator-facing completion report immediately before CompleteWorkItem in the same assistant response",
            )
            .into());
        }

        let mut brief =
            BriefRecord::new(agent_id.clone(), BriefKind::Result, report_text, None, None);
        if !citations.is_empty() {
            brief.citations = Some(citations);
        }
        brief.work_item_id = Some(existing.id.clone());
        brief.workspace_id = existing.workspace_id.clone();
        brief.turn_index = source_turn_index;
        brief.turn_id = source_turn_id;
        brief.related_message_id = source_message_id;
        brief.finalizes_assistant_round_id = source_assistant_round_id.clone();

        let Some(completed) = self
            .finalize_open_work_item_completion_with_brief(
                &work_item_id,
                &authority,
                &brief,
                AuditEvent::legacy(
                    "work_item_completion_report_promoted",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": work_item_id,
                        "source_turn_index": source_turn_index,
                        "source_round": source_round,
                        "source_assistant_round_id": source_assistant_round_id,
                        "source_tool_call_id": source_tool_call_id,
                        "source": "same_assistant_round_preceding_text",
                        "text_preview": crate::tool::helpers::truncate_text(report_text, 600),
                        "warnings": warnings.clone(),
                        "warning_count": warnings.len(),
                        "brief_id": brief.id.clone(),
                    }),
                ),
            )
            .await?
        else {
            let latest = self.validate_owned_work_item(&self.agent_id().await?, &work_item_id)?;
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(latest));
        };
        Ok(WorkItemCompletionReportPromotionOutcome::Promoted(
            WorkItemCompletionReportPromotion {
                record: completed.work_item,
                brief_id: brief.id,
                continuation_resumed: completed.continuation_resumed,
            },
        ))
    }

    #[inline(never)]
    pub(crate) fn prepare_work_item_completion_with_report<'a>(
        &'a self,
        work_item_id: String,
        authority: WorkItemCompletionAuthority,
        report_text: String,
        citations: Vec<crate::types::Citation>,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        source_turn_id: Option<String>,
        source_message_id: Option<String>,
        source_assistant_round_id: Option<String>,
        source_tool_call_id: Option<String>,
        report_source: &'static str,
        warnings: Vec<serde_json::Value>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<WorkItemCompletionDispatch>> + Send + 'a>,
    > {
        Box::pin(self.prepare_work_item_completion_with_report_inner(
            work_item_id,
            authority,
            report_text,
            citations,
            source_turn_index,
            source_round,
            source_turn_id,
            source_message_id,
            source_assistant_round_id,
            source_tool_call_id,
            report_source,
            warnings,
        ))
    }

    async fn prepare_work_item_completion_with_report_inner(
        &self,
        work_item_id: String,
        authority: WorkItemCompletionAuthority,
        report_text: String,
        citations: Vec<crate::types::Citation>,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        source_turn_id: Option<String>,
        source_message_id: Option<String>,
        source_assistant_round_id: Option<String>,
        source_tool_call_id: Option<String>,
        report_source: &'static str,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemCompletionDispatch> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state == WorkItemState::Completed {
            return Ok(WorkItemCompletionDispatch::Unchanged(existing));
        }
        if existing.state != WorkItemState::Open {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_completion_in_progress",
                format!(
                    "work item {work_item_id} already has a legacy completion intent in progress"
                ),
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_recovery_hint(
                "complete the existing legacy completion intent through the control completion API",
            )
            .into());
        }
        let report_text = report_text.trim();
        if report_text.is_empty() {
            return Err(RuntimeError::validation(
                "missing_completion_report",
                "CompleteWorkItem requires nearby preceding same-round operator-facing report text",
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_recovery_hint(
                "write the concise operator-facing completion report immediately before CompleteWorkItem in the same assistant response",
            )
            .into());
        }

        let mut brief =
            BriefRecord::new(agent_id.clone(), BriefKind::Result, report_text, None, None);
        if !citations.is_empty() {
            brief.citations = Some(citations);
        }
        brief.work_item_id = Some(existing.id.clone());
        brief.workspace_id = existing.workspace_id.clone();
        brief.turn_index = source_turn_index;
        brief.turn_id = source_turn_id;
        brief.related_message_id = source_message_id;
        brief.finalizes_assistant_round_id = source_assistant_round_id.clone();

        let Some(prepared) = self
            .plan_work_item_completion_with_brief_mode(
                &work_item_id,
                Some(&authority),
                &brief,
                AuditEvent::legacy(
                    "work_item_completion_report_promoted",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": work_item_id,
                        "source_turn_index": source_turn_index,
                        "source_round": source_round,
                        "source_assistant_round_id": source_assistant_round_id,
                        "source_tool_call_id": source_tool_call_id,
                        "source": report_source,
                        "text_preview": crate::tool::helpers::truncate_text(report_text, 600),
                        "warnings": warnings.clone(),
                        "warning_count": warnings.len(),
                        "brief_id": brief.id.clone(),
                    }),
                ),
                true,
                true,
            )
            .await?
        else {
            return Ok(WorkItemCompletionDispatch::Unchanged(
                self.validate_owned_work_item(&agent_id, &work_item_id)?,
            ));
        };
        Ok(WorkItemCompletionDispatch::Prepared(prepared))
    }

    pub(crate) async fn validate_work_item_completion_request(
        &self,
        work_item_id: &str,
        authority: &WorkItemCompletionAuthority,
    ) -> Result<(u64, WorkItemCompletionSettlement)> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, work_item_id)?;
        if existing.state != WorkItemState::Open {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_completion_in_progress",
                format!("work item {work_item_id} is not open"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .into());
        }
        let read = self
            .prepare_work_command_read(&existing, WorkCommandReadMode::Completion)
            .await?;
        let settlement = self.work_item_completion_settlement(&existing, authority, &read)?;
        let WorkItemCompletionAuthority::AgentExecution { binding, .. } = authority else {
            return Ok((existing.revision, settlement));
        };
        let mut brief = BriefRecord::new(
            agent_id,
            BriefKind::Result,
            "completion report request validation",
            None,
            None,
        );
        brief.work_item_id = Some(existing.id.clone());
        brief.workspace_id = existing.workspace_id.clone();
        brief.turn_id = Some(binding.turn_id.clone());
        brief.related_message_id = Some(binding.source_message_id.clone());
        self.plan_work_item_completion_with_brief_mode(
            work_item_id,
            Some(authority),
            &brief,
            AuditEvent::legacy(
                "work_item_completion_request_validated",
                serde_json::json!({
                    "agent_id": brief.agent_id,
                    "work_item_id": work_item_id,
                }),
            ),
            true,
            false,
        )
        .await?;
        Ok((existing.revision, settlement))
    }

    fn work_item_completion_settlement(
        &self,
        existing: &WorkItemRecord,
        authority: &WorkItemCompletionAuthority,
        read: &WorkCommandRead,
    ) -> Result<WorkItemCompletionSettlement> {
        let state = read.state.as_ref();
        let protocol_state = read.protocol.as_ref();
        if protocol_state.as_ref().is_some_and(|protocol| {
            protocol.work_items.get(&existing.id).is_some_and(|record| {
                matches!(
                    record.state,
                    crate::domain::execution_protocol::WorkItemExecutionState::NeedsRepair { .. }
                )
            })
        }) {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_execution_needs_repair",
                format!(
                    "work item {} execution state requires repair before completion",
                    existing.id
                ),
            )
            .with_safe_context("work_item_id", existing.id.clone())
            .with_recovery_hint("repair the WorkItem execution state before retrying completion")
            .into());
        }
        let active_execution = state.status == AgentStatus::AwakeRunning
            && state.current_run_id.is_some()
            && state.current_execution_binding.is_some();
        let target_in_flight = (active_execution
            && state
                .current_execution_binding
                .as_ref()
                .and_then(|binding| binding.work_item_id.as_deref())
                == Some(existing.id.as_str()))
            || protocol_state.as_ref().is_some_and(|protocol| {
                protocol.work_items.get(&existing.id).is_some_and(|record| {
                    let crate::domain::execution_protocol::WorkItemExecutionState::InFlight {
                        attempt_id,
                        ..
                    } = &record.state
                    else {
                        return false;
                    };
                    protocol.attempts.get(attempt_id).is_some_and(|attempt| {
                        attempt.state
                            == crate::domain::execution_protocol::ExecutionAttemptState::Open
                    })
                })
            });

        let settlement = match authority {
            WorkItemCompletionAuthority::AgentExecution {
                binding: expected_binding,
                effective_work_item_id,
            } => {
                let Some(current_binding) = state.current_execution_binding.as_ref() else {
                    return Err(RuntimeError::policy(
                        "work_item_execution_binding_missing",
                        "the execution binding that authorized this completion is no longer active",
                    )
                    .with_safe_context("work_item_id", existing.id.clone())
                    .into());
                };
                if current_binding != expected_binding {
                    return Err(RuntimeError::policy(
                        "work_item_execution_binding_stale",
                        "the execution binding that authorized this completion changed before commit",
                    )
                    .with_safe_context("work_item_id", existing.id.clone())
                    .with_recovery_hint("retry completion from the current execution")
                    .into());
                }
                let authorized_work_item_id = expected_binding
                    .work_item_id
                    .as_deref()
                    .or(effective_work_item_id.as_deref());
                if authorized_work_item_id == Some(existing.id.as_str()) {
                    WorkItemCompletionSettlement::BoundExecution
                } else {
                    if target_in_flight {
                        return Err(work_item_execution_active_error(existing));
                    }
                    WorkItemCompletionSettlement::Detached
                }
            }
            WorkItemCompletionAuthority::Control => {
                if target_in_flight {
                    return Err(work_item_execution_active_error(existing));
                }
                WorkItemCompletionSettlement::Detached
            }
        };

        let matching_continuations = read
            .active_frames()?
            .into_iter()
            .filter(|frame| frame.active_work_item_id == existing.id)
            .collect::<Vec<_>>();
        if matching_continuations.len() > 1 {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_continuation_ambiguous",
                format!(
                    "work item {} has multiple active caller continuations",
                    existing.id
                ),
            )
            .with_safe_context("work_item_id", existing.id.clone())
            .with_recovery_hint("repair the continuation stack before retrying completion")
            .into());
        }
        if settlement == WorkItemCompletionSettlement::Detached
            && active_execution
            && !matching_continuations.is_empty()
        {
            return Err(RuntimeError::new(
                RuntimeErrorDomain::Conflict,
                "work_item_continuation_execution_active",
                format!(
                    "work item {} cannot resume its caller while another execution is active",
                    existing.id
                ),
            )
            .with_safe_context("work_item_id", existing.id.clone())
            .with_recovery_hint("complete the target after the current execution becomes quiescent")
            .into());
        }
        Ok(settlement)
    }

    pub(super) async fn promote_work_item_completion_report_with_metadata(
        &self,
        work_item_id: String,
        report_text: String,
        citations: Vec<crate::types::Citation>,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
        warnings: Vec<serde_json::Value>,
    ) -> Result<WorkItemCompletionReportPromotionOutcome> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, &work_item_id)?;
        if existing.state == WorkItemState::Open {
            return Err(anyhow!(
                "cannot promote completion report for open work item {}",
                work_item_id
            ));
        }
        let report_text = report_text.trim();
        if report_text.is_empty() {
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(
                existing,
            ));
        }
        if existing.result_brief_id.is_some()
            || existing
                .completion_intent
                .as_ref()
                .is_some_and(|intent| intent.report_state == CompletionReportState::Bound)
        {
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(
                existing,
            ));
        }
        let mut brief =
            BriefRecord::new(agent_id.clone(), BriefKind::Result, report_text, None, None);
        if !citations.is_empty() {
            brief.citations = Some(citations);
        }
        brief.work_item_id = Some(existing.id.clone());
        brief.workspace_id = existing.workspace_id.clone();
        brief.turn_index = source_turn_index;
        brief.turn_id = existing
            .completion_intent
            .as_ref()
            .and_then(|intent| intent.source_turn_id.clone());
        brief.related_message_id = existing
            .completion_intent
            .as_ref()
            .and_then(|intent| intent.source_message_id.clone());
        let Some(completed) = self
            .finalize_work_item_completion_with_brief(
                &work_item_id,
                &brief,
                AuditEvent::legacy(
                    "work_item_completion_report_promoted",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": work_item_id,
                        "source_turn_index": source_turn_index,
                        "source_round": source_round,
                        "text_preview": crate::tool::helpers::truncate_text(report_text, 600),
                        "warnings": warnings.clone(),
                        "warning_count": warnings.len(),
                        "brief_id": brief.id.clone(),
                    }),
                ),
            )
            .await?
        else {
            let latest = self.validate_owned_work_item(&self.agent_id().await?, &work_item_id)?;
            return Ok(WorkItemCompletionReportPromotionOutcome::Unchanged(latest));
        };
        Ok(WorkItemCompletionReportPromotionOutcome::Promoted(
            WorkItemCompletionReportPromotion {
                record: completed.work_item,
                brief_id: brief.id,
                continuation_resumed: completed.continuation_resumed,
            },
        ))
    }

    pub(super) async fn finalize_work_item_completion_with_brief(
        &self,
        work_item_id: &str,
        brief: &BriefRecord,
        binding_event: AuditEvent,
    ) -> Result<Option<CompletedWorkItem>> {
        self.finalize_work_item_completion_with_brief_mode(
            work_item_id,
            None,
            brief,
            binding_event,
            false,
        )
        .await
    }

    async fn finalize_open_work_item_completion_with_brief(
        &self,
        work_item_id: &str,
        authority: &WorkItemCompletionAuthority,
        brief: &BriefRecord,
        binding_event: AuditEvent,
    ) -> Result<Option<CompletedWorkItem>> {
        self.finalize_work_item_completion_with_brief_mode(
            work_item_id,
            Some(authority),
            brief,
            binding_event,
            true,
        )
        .await
    }

    async fn finalize_work_item_completion_with_brief_mode(
        &self,
        work_item_id: &str,
        authority: Option<&WorkItemCompletionAuthority>,
        brief: &BriefRecord,
        binding_event: AuditEvent,
        require_open: bool,
    ) -> Result<Option<CompletedWorkItem>> {
        const MAX_ATTEMPTS: usize = 8;

        for attempt in 0..MAX_ATTEMPTS {
            let Some(prepared) = self
                .plan_work_item_completion_with_brief_mode(
                    work_item_id,
                    authority,
                    brief,
                    binding_event.clone(),
                    require_open,
                    false,
                )
                .await?
            else {
                return Ok(None);
            };
            let record = prepared.record.clone();
            let continuation_resumed = prepared.continuation_resumed.clone();
            #[cfg(test)]
            self.apply_completion_binding_replacement_before_commit()
                .await?;
            let execution_protocol =
                if prepared.settlement == WorkItemCompletionSettlement::Detached {
                    execution_protocol_detached_completion_transition_from_prepared(&prepared)?
                } else {
                    crate::runtime_db::transitions::ExecutionProtocolTransition::default()
                };
            let commit = {
                let mut mutation = self.begin_mutation().await?;
                mutation
                    .commit_focus_transition(
                        crate::ids::audit_event_id(),
                        crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
                            agent_id: record.agent_id.clone(),
                            work_items: vec![
                                crate::runtime_db::transitions::WorkItemMutation::Update {
                                    record: record.clone(),
                                    expected_revision: record.revision - 1,
                                },
                            ],
                            wait_conditions: prepared.wait_conditions,
                            active_continuation_expectations: None,
                            continuation_expectations: prepared.continuation_expectations,
                            continuations: prepared.continuations,
                            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                                expected: Some(Box::new(prepared.expected_agent_state)),
                                record: Box::new(prepared.committed_agent_state),
                            },
                            brief_evidence: vec![prepared.brief],
                            audit_events: prepared.audit_events,
                            index_changes: prepared.index_changes,
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                        },
                        execution_protocol,
                        true,
                        None,
                    )
                    .await
            };
            match commit {
                Ok(commit) => {
                    self.apply_transition_commit(commit).await;
                    return Ok(Some(CompletedWorkItem {
                        work_item: record,
                        continuation_resumed,
                    }));
                }
                Err(error)
                    if attempt + 1 < MAX_ATTEMPTS
                        && crate::runtime_db::retryable_state_transition_conflict(&error)
                            .is_some() =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("completion finalization attempts return or continue")
    }

    pub(crate) async fn commit_prepared_detached_work_item_completion(
        &self,
        prepared: PreparedWorkItemCompletion,
    ) -> Result<CompletedWorkItem> {
        anyhow::ensure!(
            prepared.settlement == WorkItemCompletionSettlement::Detached,
            "detached completion commit requires detached settlement"
        );
        let record = prepared.record.clone();
        let continuation_resumed = prepared.continuation_resumed.clone();
        let execution_protocol =
            execution_protocol_detached_completion_transition_from_prepared(&prepared)?;
        let tool_execution = prepared.tool_execution.clone();
        let command = crate::runtime_db::transitions::WorkItemFocusTransitionCommand {
            agent_id: record.agent_id.clone(),
            work_items: vec![crate::runtime_db::transitions::WorkItemMutation::Update {
                record: record.clone(),
                expected_revision: record.revision - 1,
            }],
            wait_conditions: prepared.wait_conditions,
            active_continuation_expectations: None,
            continuation_expectations: prepared.continuation_expectations,
            continuations: prepared.continuations,
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(prepared.expected_agent_state)),
                record: Box::new(prepared.committed_agent_state),
            },
            brief_evidence: vec![prepared.brief],
            audit_events: prepared.audit_events,
            index_changes: prepared.index_changes,
            notify_scheduler: true,
            fault: self.take_transition_fault(),
        };
        let commit = if let Some(tool_execution) = tool_execution.as_ref() {
            {
                let mut mutation = self.begin_mutation().await?;
                mutation
                    .commit_focus_transition(
                        crate::ids::audit_event_id(),
                        command,
                        execution_protocol,
                        true,
                        Some(tool_execution.clone()),
                    )
                    .await
            }?
        } else {
            {
                let mut mutation = self.begin_mutation().await?;
                mutation
                    .commit_focus_transition(
                        crate::ids::audit_event_id(),
                        command,
                        execution_protocol,
                        true,
                        None,
                    )
                    .await
            }?
        };
        self.apply_transition_commit(commit).await;
        Ok(CompletedWorkItem {
            work_item: record,
            continuation_resumed,
        })
    }

    #[inline(never)]
    fn plan_work_item_completion_with_brief_mode<'a>(
        &'a self,
        work_item_id: &'a str,
        authority: Option<&'a WorkItemCompletionAuthority>,
        brief: &'a BriefRecord,
        binding_event: AuditEvent,
        require_open: bool,
        require_completion_commit_attempt: bool,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<Output = Result<Option<PreparedWorkItemCompletion>>> + Send + 'a,
        >,
    > {
        Box::pin(self.plan_work_item_completion_with_brief_mode_inner(
            work_item_id,
            authority,
            brief,
            binding_event,
            require_open,
            require_completion_commit_attempt,
        ))
    }

    async fn plan_work_item_completion_with_brief_mode_inner(
        &self,
        work_item_id: &str,
        authority: Option<&WorkItemCompletionAuthority>,
        brief: &BriefRecord,
        binding_event: AuditEvent,
        require_open: bool,
        require_completion_commit_attempt: bool,
    ) -> Result<Option<PreparedWorkItemCompletion>> {
        let agent_id = self.agent_id().await?;
        let existing = self.validate_owned_work_item(&agent_id, work_item_id)?;
        if existing.state == WorkItemState::Completed {
            return Ok(None);
        }
        let expected_state = if require_open {
            WorkItemState::Open
        } else {
            WorkItemState::Completing
        };
        if existing.state != expected_state {
            return Err(anyhow!(
                "cannot finalize completion report for work item {} in state {:?}; expected {:?}",
                work_item_id,
                existing.state,
                expected_state,
            ));
        }
        let read = self
            .prepare_work_command_read(&existing, WorkCommandReadMode::Completion)
            .await?;
        let now = Utc::now();
        let mut state = read.state.as_ref().clone();
        let settlement = match authority {
            Some(authority) => self.work_item_completion_settlement(&existing, authority, &read)?,
            None => {
                let intent = existing.completion_intent.as_ref().ok_or_else(|| {
                    anyhow!("completion brief binding requires a completion intent")
                })?;
                if intent.source_activation_id.is_some() {
                    WorkItemCompletionSettlement::BoundExecution
                } else {
                    WorkItemCompletionSettlement::Detached
                }
            }
        };
        let expected_execution_protocol_state = if require_completion_commit_attempt
            || settlement == WorkItemCompletionSettlement::Detached
        {
            read.protocol.clone()
        } else {
            None
        };
        let legacy_work_item_execution = expected_execution_protocol_state
            .as_ref()
            .filter(|state| !state.work_items.contains_key(&existing.id))
            .map(|_| {
                use crate::domain::execution_protocol::{
                    WaitReference, WorkItemExecutionRecord, WorkItemExecutionState,
                };

                let active_wait_ids = read
                    .active_waits_for(&existing.id)?
                    .into_iter()
                    .map(|condition| condition.id.clone())
                    .collect::<Vec<_>>();
                let generation = existing.revision.max(1);
                let state = match active_wait_ids.as_slice() {
                    [wait_id] => WorkItemExecutionState::Waiting {
                        generation,
                        wait: WaitReference {
                            wait_id: wait_id.clone(),
                        },
                    },
                    [] if existing.blocked_by.is_some() => WorkItemExecutionState::Paused {
                        generation,
                        reason: existing
                            .blocked_by
                            .clone()
                            .expect("manual blocker checked above"),
                    },
                    [] => WorkItemExecutionState::Runnable {
                        generation,
                        recovery_ref: None,
                    },
                    _ => WorkItemExecutionState::NeedsRepair {
                        generation,
                        repair_id: format!("work_item_waits_ambiguous:{}", existing.id),
                    },
                };
                Ok::<_, anyhow::Error>(WorkItemExecutionRecord {
                    source_revision: existing.revision,
                    state,
                })
            })
            .transpose()?;
        let expected_agent_state = state.clone();
        let matching_execution_binding = match authority {
            Some(WorkItemCompletionAuthority::AgentExecution {
                binding: expected_binding,
                ..
            }) => {
                let Some(current_binding) = state.current_execution_binding.as_ref() else {
                    return Err(RuntimeError::policy(
                        "work_item_execution_binding_missing",
                        "the execution binding that authorized this completion is no longer active",
                    )
                    .with_safe_context("work_item_id", work_item_id)
                    .into());
                };
                if current_binding != expected_binding {
                    return Err(RuntimeError::policy(
                            "work_item_execution_binding_stale",
                            "the execution binding that authorized this completion changed before commit",
                        )
                        .with_safe_context("work_item_id", work_item_id)
                        .with_recovery_hint(
                            "retry completion from the WorkItem's current execution",
                        )
                        .into());
                }
                if Some(expected_binding.turn_id.as_str()) != brief.turn_id.as_deref()
                    || Some(expected_binding.source_message_id.as_str())
                        != brief.related_message_id.as_deref()
                {
                    return Err(RuntimeError::policy(
                        "work_item_completion_report_binding_mismatch",
                        "the completion report does not belong to the authorizing execution",
                    )
                    .with_safe_context("work_item_id", work_item_id)
                    .into());
                }
                (settlement == WorkItemCompletionSettlement::BoundExecution)
                    .then_some(expected_binding)
            }
            Some(WorkItemCompletionAuthority::Control) | None => None,
        };
        let mut source_activation_id =
            matching_execution_binding.and_then(|binding| binding.activation_id.clone());
        if source_activation_id.is_none()
            && settlement == WorkItemCompletionSettlement::BoundExecution
            && require_completion_commit_attempt
        {
            if let Some(protocol_state) = expected_execution_protocol_state.as_ref() {
                let mut matching_attempts = protocol_state.attempts.values().filter(|attempt| {
                    attempt.state
                        == crate::domain::execution_protocol::ExecutionAttemptState::Open
                        && attempt.source_message_id.as_deref()
                            == brief.related_message_id.as_deref()
                        && match &attempt.binding {
                            crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                                work_item_id,
                            } => {
                                work_item_id == &existing.id
                                    && attempt.admitted_fences.work_item_source_revision
                                        == matching_execution_binding
                                            .and_then(|binding| binding.claimed_work_revision)
                                            .or(Some(existing.revision))
                                    && protocol_state
                                        .work_items
                                        .get(&existing.id)
                                        .is_some_and(|work_item| {
                                            matches!(
                                                &work_item.state,
                                                crate::domain::execution_protocol::WorkItemExecutionState::InFlight {
                                                    attempt_id,
                                                    generation,
                                                } if attempt_id == &attempt.attempt_id
                                                    && Some(*generation)
                                                        == attempt
                                                            .admitted_fences
                                                            .work_item_generation
                                            )
                                        })
                            }
                            crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                                agent_id: binding_agent_id,
                            } => {
                                binding_agent_id == &agent_id
                                    && protocol_state.work_items.get(&existing.id).map_or_else(
                                        || legacy_work_item_execution.is_some(),
                                        |work_item| {
                                            work_item.source_revision == existing.revision
                                                && !matches!(
                                                    work_item.state,
                                                    crate::domain::execution_protocol::WorkItemExecutionState::InFlight { .. }
                                                        | crate::domain::execution_protocol::WorkItemExecutionState::Terminal { .. }
                                                )
                                        },
                                    )
                            }
                            crate::domain::execution_protocol::ExecutionBinding::Conversation {
                                ..
                            } => {
                                attempt.agent_id == agent_id
                                    && protocol_state.work_items.get(&existing.id).map_or_else(
                                        || legacy_work_item_execution.is_some(),
                                        |work_item| {
                                            work_item.source_revision == existing.revision
                                                && !matches!(
                                                    work_item.state,
                                                    crate::domain::execution_protocol::WorkItemExecutionState::InFlight { .. }
                                                        | crate::domain::execution_protocol::WorkItemExecutionState::Terminal { .. }
                                                )
                                        },
                                    )
                            }
                            crate::domain::execution_protocol::ExecutionBinding::Command => false,
                        }
                });
                let matching_attempt = matching_attempts
                    .next()
                    .ok_or_else(|| anyhow!("completion commit execution attempt is missing"))?;
                if matching_attempts.next().is_some() {
                    return Err(anyhow!("completion commit execution attempt is ambiguous"));
                }
                source_activation_id = Some(matching_attempt.attempt_id.clone());
            }
        }
        let mut completion_intent = if require_open {
            WorkItemCompletionIntent {
                work_item_id: existing.id.clone(),
                source_activation_id,
                source_message_id: brief.related_message_id.clone(),
                source_turn_id: brief.turn_id.clone(),
                expected_work_revision: matching_execution_binding
                    .and_then(|binding| binding.claimed_work_revision)
                    .unwrap_or(existing.revision),
                report_requirement: CompletionReportRequirement::Required,
                report_state: CompletionReportState::Pending,
                result_brief_id: None,
                created_at: now,
                updated_at: now,
            }
        } else {
            let intent = existing
                .completion_intent
                .clone()
                .ok_or_else(|| anyhow!("completion brief binding requires a completion intent"))?;
            if intent.work_item_id != existing.id
                || intent.source_turn_id != brief.turn_id
                || intent.source_message_id != brief.related_message_id
            {
                return Err(anyhow!(
                    "completion brief identity does not match work item {} intent",
                    work_item_id
                ));
            }
            intent
        };
        completion_intent.report_state = CompletionReportState::Bound;
        completion_intent.result_brief_id = Some(brief.id.clone());
        completion_intent.updated_at = now;
        let mut record = WorkItemRecord {
            revision: existing.revision + 1,
            state: WorkItemState::Completed,
            blocked_by: None,
            recheck_at: None,
            recheck_consumed_at: None,
            result_brief_id: Some(brief.id.clone()),
            completion_intent: Some(completion_intent),
            updated_at: now,
            ..existing
        };
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut record,
        )?;

        let active_waits = read
            .active_waits_for(&record.id)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut wait_conditions = Vec::with_capacity(active_waits.len());
        let mut cancelled_wait_condition_ids = Vec::with_capacity(active_waits.len());
        for condition in active_waits {
            let mut cancelled = condition.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at = now;
            cancelled.cancelled_at = Some(now);
            cancelled_wait_condition_ids.push(condition.id);
            wait_conditions.push(cancelled);
        }

        state.last_brief_at = Some(brief.created_at);
        let release_current = state.current_work_item_id.as_deref() == Some(record.id.as_str());
        let release_turn = state.current_turn_work_item_id.as_deref() == Some(record.id.as_str());
        if release_current {
            state.current_work_item_id = None;
        }
        if release_turn {
            state.current_turn_work_item_id = None;
        }
        if settlement == WorkItemCompletionSettlement::BoundExecution {
            state.current_execution_binding = None;
            state.current_run_id = None;
            if !matches!(state.status, AgentStatus::Asleep | AgentStatus::Stopped) {
                state.status = AgentStatus::AwakeIdle;
            }
        } else if (release_current || release_turn)
            && state.current_execution_binding.is_none()
            && !matches!(state.status, AgentStatus::Asleep | AgentStatus::Stopped)
        {
            state.status = AgentStatus::AwakeIdle;
        }

        let mut audit_events = vec![
            // Deterministic identity so a replayed completion transition
            // reuses the committed brief_created event and links the Brief
            // to its event sequence in the same transaction.
            brief_created_event_for(brief)?,
            binding_event.clone(),
        ];
        if plan_artifact_changed {
            if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                audit_events.push(event);
            }
        }
        if release_current || release_turn {
            audit_events.push(AuditEvent::legacy(
                "work_item_focus_released",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id,
                    "reason": "work_item_completed",
                    "readiness": record.readiness(),
                    "revision": record.revision,
                }),
            ));
        }

        let mut continuation_expectations = Vec::new();
        let mut continuation_records = Vec::new();
        let mut continuation_resumed = None;
        let mut continuation_resumes = BTreeMap::new();
        if let Some(frame) = read
            .active_frames()?
            .into_iter()
            .find(|frame| frame.active_work_item_id == record.id)
            .cloned()
        {
            continuation_expectations.push(frame.clone());
            let suspended = read.work.work_item(&frame.suspended_work_item_id)?.cloned();
            match suspended {
                Some(suspended)
                    if suspended.agent_id == agent_id && suspended.state == WorkItemState::Open =>
                {
                    continuation_resumes.insert(
                        frame.id.clone(),
                        read.continuation_resume_source(
                            &frame.suspended_work_item_id,
                            None,
                            &wait_conditions,
                        )?,
                    );
                    let resumed = frame.resume("active_work_item_completed");
                    let summary = continuation_summary(&resumed, "active_work_item_completed");
                    state.current_work_item_id = Some(suspended.id.clone());
                    state.current_turn_work_item_id = Some(suspended.id.clone());
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_resumed",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "continuation": summary,
                            "completed_work_item_id": record.id,
                            "resumed_work_item_id": suspended.id,
                        }),
                    ));
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_scheduler_evidence",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "reason": "continuation_resumed",
                            "work_item_id": suspended.id,
                            "completed_work_item_id": record.id,
                            "continuation_frame_id": summary.frame_id,
                        }),
                    ));
                    continuation_resumed = Some(summary);
                    continuation_records.push(resumed);
                }
                suspended => {
                    let reason = if suspended.is_some() {
                        "suspended_work_item_not_open"
                    } else {
                        "suspended_work_item_missing"
                    };
                    let cancelled = frame.cancel(reason);
                    audit_events.push(AuditEvent::legacy(
                        "work_item_continuation_cancelled",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "continuation": continuation_summary(&cancelled, reason),
                            "suspended_work_item_state": suspended.map(|record| record.state),
                        }),
                    ));
                    continuation_records.push(cancelled);
                }
            }
        }
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": record.id,
                    "reason": "work_item_completed",
                    "wait_condition_ids": cancelled_wait_condition_ids,
                }),
            ));
        }
        audit_events.push(self.work_item_written_event(
            "completed",
            &record,
            serde_json::json!({
                "continuation_resumed": continuation_resumed,
                "completion_intent": record.completion_intent,
                "brief_id": brief.id,
            }),
        ));

        let index_changes = self.inner.storage.index_changes_for_work_item(&record)?;
        Ok(Some(PreparedWorkItemCompletion {
            settlement,
            record,
            brief: brief.clone(),
            expected_execution_protocol_state,
            legacy_work_item_execution,
            expected_agent_state,
            committed_agent_state: state,
            wait_conditions,
            continuation_expectations,
            continuations: continuation_records,
            continuation_resumed,
            continuation_resumes,
            audit_events,
            index_changes,
            tool_execution: None,
            transcript_entries: Vec::new(),
            _work_read_budget: Some(read.owned.clone()),
        }))
    }

    pub async fn record_work_item_completion_warning(
        &self,
        work_item_id: String,
        kind: &str,
        message: &str,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "work_item_completion_warning",
            serde_json::json!({
                "agent_id": agent_id,
                "work_item_id": work_item_id,
                "kind": kind,
                "message": message,
                "source_turn_index": source_turn_index,
                "source_round": source_round,
            }),
        ))?;
        Ok(())
    }

    pub(super) fn validate_owned_work_item(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<WorkItemRecord> {
        let record = self
            .inner
            .runtime_db
            .work_items()
            .latest(work_item_id)?
            .ok_or_else(|| {
                RuntimeError::not_found(
                    "work_item_not_found",
                    format!("unknown work item {work_item_id}"),
                )
                .with_safe_context("work_item_id", work_item_id)
            })?;
        if record.agent_id != agent_id {
            return Err(RuntimeError::policy(
                "work_item_access_denied",
                format!("work item {work_item_id} belongs to another agent"),
            )
            .with_safe_context("work_item_id", work_item_id)
            .with_safe_context("agent_id", agent_id)
            .into());
        }
        Ok(record)
    }
}

fn task_not_found_error(task_id: &str) -> RuntimeError {
    RuntimeError::not_found("task_not_found", format!("task {task_id} not found"))
        .with_safe_context("task_id", task_id)
}

fn work_item_execution_active_error(record: &WorkItemRecord) -> anyhow::Error {
    RuntimeError::new(
        RuntimeErrorDomain::Conflict,
        "work_item_execution_active",
        format!(
            "work item {} is owned by an active execution and cannot be completed externally",
            record.id
        ),
    )
    .with_safe_context("work_item_id", record.id.clone())
    .with_recovery_hint(
        "let the active execution complete the WorkItem, or stop that execution before retrying",
    )
    .into()
}

fn continuation_summary(
    frame: &WorkItemContinuationFrame,
    reason: impl Into<String>,
) -> WorkItemContinuationSummary {
    WorkItemContinuationSummary {
        frame_id: frame.id.clone(),
        suspended_work_item_id: frame.suspended_work_item_id.clone(),
        active_work_item_id: frame.active_work_item_id.clone(),
        return_policy: frame.return_policy,
        reason: reason.into(),
    }
}

fn task_status_name(state: &TaskStatus) -> &'static str {
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

pub(super) fn append_task_owned_worktree_cleanup_note(
    text: &mut String,
    cleanup: &worktree::TaskOwnedWorktreeCleanup,
    worktree_path: &PathBuf,
) {
    match cleanup.status {
        worktree::TaskOwnedWorktreeCleanupStatus::Cleaned => {
            text.push_str("\nWorktree cleanup: auto-removed clean task-owned artifact.");
        }
        worktree::TaskOwnedWorktreeCleanupStatus::AlreadyRemoved => {
            text.push_str("\nWorktree cleanup: already removed.");
        }
        worktree::TaskOwnedWorktreeCleanupStatus::Retained => {
            if cleanup.changed_files.is_empty() {
                text.push_str(
                    "\nWorktree retained for review: cleanup skipped after artifact metadata mismatch.",
                );
            } else {
                text.push_str(&format!(
                    "\nWorktree retained for review: {} changes detected. Use 'git worktree remove {}' when done.",
                    cleanup.changed_files.len(),
                    worktree_path.display()
                ));
            }
        }
        worktree::TaskOwnedWorktreeCleanupStatus::Failed => {
            text.push_str("\nWorktree cleanup: failed; artifact retained for manual inspection.");
        }
    }
}

pub(super) fn task_from_message(message: &MessageEnvelope, agent_id: &str) -> Result<TaskRecord> {
    let metadata = message.metadata.as_ref();
    let task_id = metadata
        .and_then(|value| value.get("task_id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task message missing metadata.task_id"))?
        .to_string();
    let task_kind = metadata
        .and_then(|value| value.get("task_kind"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("task message missing metadata.task_kind"))
        .and_then(|kind| {
            serde_json::from_value::<TaskKind>(serde_json::Value::String(kind.to_string()))
                .with_context(|| format!("unknown task kind {kind:?}"))
        })?;
    let task_status = metadata
        .and_then(|value| value.get("task_status"))
        .and_then(|value| value.as_str());
    let status = match task_status.unwrap_or(match message.kind {
        MessageKind::TaskStatus => "running",
        MessageKind::TaskResult => "completed",
        _ => "queued",
    }) {
        "running" => TaskStatus::Running,
        "cancelling" => TaskStatus::Cancelling,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "interrupted" => TaskStatus::Interrupted,
        _ => TaskStatus::Queued,
    };
    let summary = metadata
        .and_then(|value| value.get("task_summary"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| Some(message_text(&message.body)));

    let mut detail = metadata.and_then(|value| value.get("task_detail")).cloned();
    if let Some(parent_turn_id) = message.turn_id.as_ref() {
        let detail = detail.get_or_insert_with(|| serde_json::json!({}));
        if let Some(detail) = detail.as_object_mut() {
            detail
                .entry("parent_turn_id")
                .or_insert_with(|| serde_json::json!(parent_turn_id));
        }
    }
    if message.correlation_id.is_some() || message.causation_id.is_some() {
        let detail = detail.get_or_insert_with(|| serde_json::json!({}));
        if let Some(detail) = detail.as_object_mut() {
            if let Some(correlation_id) = message.correlation_id.as_ref() {
                detail
                    .entry("correlation_id")
                    .or_insert_with(|| serde_json::json!(correlation_id));
            }
            if let Some(causation_id) = message.causation_id.as_ref() {
                detail
                    .entry("causation_id")
                    .or_insert_with(|| serde_json::json!(causation_id));
            }
        }
    }

    Ok(TaskRecord {
        id: task_id,
        agent_id: agent_id.to_string(),
        kind: task_kind,
        status,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: metadata
            .and_then(|value| value.get("work_item_id"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .or_else(|| message.work_item_id.clone()),
        summary,
        detail,
        recovery: metadata
            .and_then(|value| value.get("task_recovery"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?,
    })
}

fn detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn effective_task_output_status(
    task_status: &TaskStatus,
    latest_message: Option<&TaskMessageSnapshot>,
) -> TaskStatus {
    if is_terminal_task_status(task_status) || matches!(task_status, TaskStatus::Cancelling) {
        return task_status.clone();
    }

    match latest_message {
        Some(message) => message.state.clone(),
        None => task_status.clone(),
    }
}

fn task_output_ready(task: &TaskRecord, state: &TaskStatus) -> bool {
    if matches!(
        state,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    ) {
        return false;
    }

    if task.kind != TaskKind::CommandTask {
        return true;
    }

    if task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("terminal_snapshot_ready"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if is_terminal_task_status(&task.status)
        && task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("output_path"))
            .and_then(|value| value.as_str())
            .is_some()
    {
        return true;
    }

    task.detail.as_ref().is_some_and(|detail| {
        detail
            .get("exit_status")
            .is_some_and(|value| !value.is_null())
            || detail
                .get("error")
                .and_then(|value| value.as_str())
                .is_some()
            || detail_string(&task.detail, "output_summary").is_some()
            || detail_string(&task.detail, "initial_output").is_some()
    })
}

fn task_failure_artifact(
    task: &TaskRecord,
    state: &TaskStatus,
    output: &str,
    output_path: Option<&str>,
    exit_status: Option<i32>,
) -> Option<FailureArtifact> {
    if !matches!(
        state,
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
    ) {
        return None;
    }

    let mut metadata = BTreeMap::new();
    if let Some(cmd) = detail_string(&task.detail, "cmd") {
        metadata.insert("command".into(), cmd);
    }
    if let Some(path) = output_path {
        metadata.insert("output_path".into(), path.to_string());
    } else if let Some(path) = detail_string(&task.detail, "output_path") {
        metadata.insert("output_path".into(), path);
    }
    let has_error = detail_string(&task.detail, "error").is_some();
    let source_chain = detail_string(&task.detail, "error")
        .map(|error| sanitize_runtime_error_text(&error))
        .filter(|error| !error.is_empty())
        .into_iter()
        .collect();
    if has_error {
        metadata.insert("error_present".into(), "true".into());
    }
    if let Some(task_status) = detail_string(&task.detail, "task_status") {
        metadata.insert("task_status".into(), task_status);
    }

    let (kind, summary, exit_status) = if task.kind == TaskKind::CommandTask {
        let kind = if let Some(code) = exit_status {
            if code != 0 {
                "command_task_exit_nonzero"
            } else if matches!(state, TaskStatus::Interrupted) {
                "command_task_interrupted"
            } else {
                "command_task_failed"
            }
        } else if matches!(state, TaskStatus::Interrupted) {
            "command_task_interrupted"
        } else if has_error {
            "command_task_error"
        } else if output.is_empty() {
            "command_task_failed"
        } else {
            "command_task_output"
        };

        let summary = if matches!(state, TaskStatus::Interrupted) {
            "command task interrupted by runtime restart".to_string()
        } else if let Some(code) = exit_status {
            format!("command task exited with status {code}")
        } else if has_error && metadata.contains_key("output_path") {
            "command task failed; inspect output_path for details".to_string()
        } else if has_error {
            "command task failed before producing output".to_string()
        } else if !output.is_empty() && metadata.contains_key("output_path") {
            "command task failed; inspect output_path for details".to_string()
        } else if !output.is_empty() {
            "command task failed and produced output".to_string()
        } else {
            task.summary
                .as_deref()
                .map(ToString::to_string)
                .unwrap_or_else(|| task.kind.as_str().to_string())
        };

        (kind, summary, exit_status)
    } else {
        let kind = match state {
            TaskStatus::Cancelled => "task_cancelled",
            TaskStatus::Interrupted => "task_interrupted",
            _ => "task_failed",
        };
        let summary = task
            .summary
            .as_deref()
            .map(ToString::to_string)
            .unwrap_or_else(|| task.kind.as_str().to_string());
        (kind, summary, None)
    };

    Some(FailureArtifact {
        category: FailureArtifactCategory::Task,
        kind: kind.to_string(),
        summary,
        domain: Some(RuntimeErrorDomain::Task),
        retryable: Some(false),
        recovery_hint: None,
        provider: None,
        model_ref: None,
        status: None,
        task_id: Some(task.id.clone()),
        exit_status,
        source_chain,
        context: Box::new(RuntimeErrorContext {
            message_id: task.parent_message_id.clone(),
            turn_id: detail_string(&task.detail, "parent_turn_id"),
            work_item_id: task.work_item_id.clone(),
            task_id: Some(task.id.clone()),
            correlation_id: detail_string(&task.detail, "correlation_id"),
            causation_id: detail_string(&task.detail, "causation_id"),
            ..RuntimeErrorContext::default()
        }),
        metadata,
    })
}

fn task_output_artifacts(output_path: Option<&str>) -> (Vec<ToolArtifactRef>, Option<usize>) {
    let Some(path) = output_path else {
        return (Vec::new(), None);
    };
    (
        vec![ToolArtifactRef {
            path: path.to_string(),
        }],
        Some(0),
    )
}

fn is_terminal_task_status(state: &TaskStatus) -> bool {
    matches!(
        state,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

fn render_task_message_body(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn task_status_from_message(message: &MessageEnvelope, metadata: &serde_json::Value) -> TaskStatus {
    super::resident_task::task_output_status_from_message(message, metadata)
}

#[cfg(test)]
mod control_task_result_tests {
    use super::*;

    fn running_task(detail: serde_json::Value) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            id: "control-task".into(),
            agent_id: "control-agent".into(),
            kind: TaskKind::ActorInvocation,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: Some("work-item".into()),
            summary: Some("original task".into()),
            detail: Some(detail),
            recovery: None,
        }
    }

    #[test]
    fn stop_result_preserves_rejoin_generation_and_freezes_consistent_message_detail() {
        let task = running_task(serde_json::json!({
            "rejoin_obligation_id": "control-task", "rejoin_generation": 7,
            "parent_turn_id": "original-turn", "custom": "preserved"
        }));
        let prepared_at = task.created_at + chrono::Duration::seconds(1);
        let result = prepare_lifecycle_stop_task_result(&task, prepared_at).unwrap();
        assert_eq!(
            result.task.rejoin_fence().unwrap(),
            task.rejoin_fence().unwrap()
        );
        assert_eq!(result.task.status, TaskStatus::Interrupted);
        assert_eq!(result.task.updated_at, prepared_at);
        assert_eq!(result.message.created_at, prepared_at);
        assert_eq!(
            result.task.parent_message_id.as_deref(),
            Some(result.message.id.as_str())
        );
        assert_eq!(
            result.message.metadata.as_ref().unwrap()["task_detail"],
            result.task.detail.clone().unwrap()
        );
        assert_eq!(result.message.task_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(result.task.detail.as_ref().unwrap()["custom"], "preserved");
        assert_eq!(task.status, TaskStatus::Running);
    }

    #[test]
    fn legacy_missing_parent_fallback_is_present_before_message_metadata_is_frozen() {
        let task = running_task(serde_json::json!({
            "rejoin_obligation_id": "control-task", "rejoin_generation": 1
        }));
        let result = prepare_lifecycle_stop_task_result(&task, task.created_at).unwrap();
        assert_eq!(
            result.task.detail.as_ref().unwrap()["parent_turn_id"],
            result.message.turn_id.as_deref().unwrap()
        );
        assert_eq!(
            result.message.metadata.as_ref().unwrap()["task_detail"],
            result.task.detail.clone().unwrap()
        );
        assert_eq!(result.task.rejoin_fence().unwrap().generation, 1);
    }

    #[test]
    fn original_interruption_preserves_unbound_bound_and_invalid_parent_contracts() {
        for parent in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("original-parent")),
        ] {
            let mut task = running_task(serde_json::json!({
                "rejoin_obligation_id":"control-task", "rejoin_generation":1, "custom":"preserved"
            }));
            if let Some(parent) = &parent {
                task.detail.as_mut().unwrap()[PARENT_TURN_ID_DETAIL] = parent.clone();
            }
            let result = interrupted_task_result_message(&task, "agent_stopped");
            let frozen = serde_json::to_value(&result).unwrap();
            let normalized = task_with_original_interrupted_result(&task, &result);
            let mut expected = task_with_result_message(
                &task,
                TaskStatus::Interrupted,
                task.detail.clone(),
                &result,
            );
            if parent.is_none() {
                expected
                    .detail
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(PARENT_TURN_ID_DETAIL);
                assert_eq!(unbound_task_rejoin_generation(&normalized), Some(1));
                assert!(normalized.rejoin_fence().is_err());
            }
            expected.updated_at = normalized.updated_at;
            assert_eq!(normalized, expected);
            assert_eq!(serde_json::to_value(&result).unwrap(), frozen);
        }
    }
}
