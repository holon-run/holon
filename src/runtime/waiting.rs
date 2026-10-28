use super::*;

use crate::ingress::WakeHint;
use crate::runtime_error::RuntimeError;
use crate::types::{
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource, WorkItemRecord,
    WorkItemState,
};
use chrono::{DateTime, Utc};

mod task_result_repair;

struct ReplacedWaitConditions {
    records: Vec<WaitConditionRecord>,
    cancelled_ids: Vec<String>,
    resolved_after_trigger_ids: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct TimerCancellationOutcome {
    #[serde(flatten)]
    pub timer: TimerRecord,
    pub cancelled_wait_condition_ids: Vec<String>,
    pub blocked_work_item_ids: Vec<String>,
}

/// Preserve independent business blockers in both task-wait registration
/// paths. Replaced task waits only own a scheduling blocker. This bookkeeping
/// does not decide whether an exact wait continuation may enter the model.
fn task_wait_preserves_blocker(
    existing: &WorkItemRecord,
    replaced: &[WaitConditionRecord],
) -> bool {
    existing.blocked_by.is_some()
        && !existing.blocked_by_wait_id.as_deref().is_some_and(|id| {
            replaced
                .iter()
                .any(|wait| wait.id == id && wait.kind == WaitConditionKind::Task)
        })
}

impl PreparedWaitForSettlement {
    pub(crate) fn outcome(&self) -> WaitForRegistrationOutcome {
        if let Some(admission) = self.command.task_result_admission.as_ref() {
            if admission.obligation.canonical_recovery {
                return WaitForRegistrationOutcome::TaskResultRecoveryQueued {
                    task_id: self
                        .expected_task
                        .as_ref()
                        .expect("task admission expectation")
                        .id
                        .clone(),
                    result_message_id: admission.obligation.source_message_id.clone(),
                    recovery_message_id: admission.message.id.clone(),
                    wait_condition_id: self.registration.condition.id.clone(),
                };
            }
            WaitForRegistrationOutcome::TaskResultQueued {
                task_id: self
                    .expected_task
                    .as_ref()
                    .expect("task admission expectation")
                    .id
                    .clone(),
                result_message_id: admission.message.id.clone(),
                wait_condition_id: self.registration.condition.id.clone(),
            }
        } else {
            WaitForRegistrationOutcome::Registered {
                registration: self.registration.clone(),
            }
        }
    }
}

fn replace_wait_conditions(
    existing: Vec<WaitConditionRecord>,
    now: DateTime<Utc>,
) -> ReplacedWaitConditions {
    let mut records = Vec::with_capacity(existing.len());
    let mut cancelled_ids = Vec::new();
    let mut resolved_after_trigger_ids = Vec::new();
    for mut record in existing {
        record.updated_at = now;
        match record.status {
            WaitConditionStatus::Active => {
                record.status = WaitConditionStatus::Cancelled;
                record.cancelled_at = Some(now);
                cancelled_ids.push(record.id.clone());
            }
            WaitConditionStatus::Triggered => {
                record.status = WaitConditionStatus::Resolved;
                record.resolved_at = Some(now);
                resolved_after_trigger_ids.push(record.id.clone());
            }
            _ => continue,
        }
        records.push(record);
    }
    ReplacedWaitConditions {
        records,
        cancelled_ids,
        resolved_after_trigger_ids,
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForScope {
    Agent,
    WorkItem,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForWakeKind {
    OperatorInput,
    TaskResult,
    External,
    Timer,
    System,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WaitForRegistration {
    pub(crate) scope: WaitForScope,
    pub(crate) condition: WaitConditionRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item: Option<WorkItemRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cancelled_wait_condition_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForContinuation {
    ContinueTurn,
    YieldAndWait,
    YieldAndReenter,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(crate) enum WaitForRegistrationOutcome {
    Registered {
        registration: WaitForRegistration,
    },
    TaskResultQueued {
        task_id: String,
        result_message_id: String,
        wait_condition_id: String,
    },
    TaskResultRecoveryQueued {
        task_id: String,
        result_message_id: String,
        recovery_message_id: String,
        wait_condition_id: String,
    },
    TaskResultAlreadyConsumed {
        task_id: String,
        result_message_id: String,
        settlement_disposition: crate::runtime_db::TaskResultSettlementDisposition,
    },
    TaskResultAlreadyAdmitted {
        task_id: String,
        result_message_id: String,
        activation_id: String,
    },
    TaskResultInFlight {
        task_id: String,
        result_message_id: String,
        activation_id: String,
    },
    TaskResultUnavailable {
        task_id: String,
        result_message_id: String,
        reason: String,
    },
    TaskResultClaimedByOtherWaiter {
        task_id: String,
        result_message_id: String,
        wait_condition_id: String,
        claimed_by_work_item_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct WaitForBriefPublicationScope {
    pub(crate) existing_brief_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedWaitForSettlement {
    pub(crate) delivery: crate::tool::tools::wait_for::WaitForDeliveryArg,
    pub(crate) wake: WaitForWakeKind,
    pub(crate) registration: WaitForRegistration,
    pub(crate) command: crate::runtime_db::transitions::WaitTransitionCommand,
    pub(crate) execution_protocol: crate::runtime_db::transitions::ExecutionProtocolTransition,
    pub(crate) expected_task: Option<crate::runtime_db::transitions::TaskExpectation>,
    pub(crate) tool_execution: Option<crate::types::ToolExecutionRecord>,
    pub(crate) brief: Option<crate::types::BriefRecord>,
    pub(crate) brief_publication_scope: Option<WaitForBriefPublicationScope>,
    _scheduling_material: Vec<Arc<super::input_evidence_budget::BudgetViewGuard>>,
}

pub(crate) enum PrepareWaitForOutcome {
    Prepared(Box<PreparedWaitForSettlement>),
    Immediate(WaitForRegistrationOutcome),
}

#[derive(Debug, Clone)]
pub(super) struct WorkItemBlockerClearance {
    pub(super) work_item: WorkItemRecord,
    pub(super) expected_revision: Option<u64>,
    pub(super) wait_conditions: Vec<WaitConditionRecord>,
    pub(super) audit_events: Vec<AuditEvent>,
    pub(super) index_changes: Vec<crate::runtime_db::RuntimeIndexChange>,
    pub(super) blocker_cleared: bool,
    pub(super) cancelled_wait_condition_ids: Vec<String>,
}

impl WorkItemBlockerClearance {
    pub(super) fn unchanged(work_item: WorkItemRecord) -> Self {
        Self {
            work_item,
            expected_revision: None,
            wait_conditions: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            blocker_cleared: false,
            cancelled_wait_condition_ids: Vec::new(),
        }
    }
}

impl RuntimeHandle {
    pub(super) fn wait_resolution_transition_for_message(
        &self,
        message: &MessageEnvelope,
        work: &super::resident_work::CapturedWork,
    ) -> Result<Option<crate::runtime_db::transitions::QueueWaitTransition>> {
        anyhow::ensure!(
            work.scope.agent_id == message.agent_id,
            "Wait resolution Work scope differs"
        );
        let unresolved = work.unresolved_wait_conditions()?;
        let Some(condition) = unresolved.into_iter().find(|condition| {
            condition.status == WaitConditionStatus::Triggered
                && condition.trigger_message_id() == Some(message.id.as_str())
        }) else {
            return Ok(None);
        };
        let now = self.now();
        let mut resolved = (*condition).clone();
        resolved.status = WaitConditionStatus::Resolved;
        resolved.updated_at = now;
        resolved.resolved_at = Some(now);

        let mut work_item = None;
        let mut index_changes = Vec::new();
        if let Some(work_item_id) = resolved.work_item_id.as_deref() {
            if let Some(existing) = work.work_item(work_item_id)? {
                // The blocker is cleared only when this wait derived it
                // (`blocked_by_wait_id`), never by display-text equality: a
                // same-text independent blocker stays authoritative together
                // with its recheck deadline.
                if existing.state == WorkItemState::Open
                    && existing.blocked_by_wait_id.as_deref() == Some(resolved.id.as_str())
                {
                    let mut record = WorkItemRecord {
                        revision: existing.revision + 1,
                        blocked_by: None,
                        blocked_by_wait_id: None,
                        recheck_at: None,
                        recheck_consumed_at: None,
                        updated_at: now,
                        ..existing.clone()
                    };
                    crate::work_item_plan::refresh_plan_artifact_metadata(
                        self.agent_home().as_path(),
                        &mut record,
                    )?;
                    index_changes.extend(self.inner.storage.index_changes_for_work_item(&record)?);
                    work_item = Some(crate::runtime_db::transitions::WorkItemMutation::Update {
                        record,
                        expected_revision: existing.revision,
                    });
                }
            }
        }

        Ok(Some(crate::runtime_db::transitions::QueueWaitTransition {
            expected: crate::runtime_db::transitions::WaitConditionExpectation {
                id: condition.id.clone(),
                agent_id: condition.agent_id.clone(),
                status: condition.status.clone(),
                updated_at: condition.updated_at,
            },
            record: resolved,
            work_item,
            index_changes,
        }))
    }

    pub(crate) async fn register_wait_for(
        &self,
        agent_id: &str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
    ) -> Result<WaitForRegistration> {
        match self
            .register_wait_for_outcome(
                agent_id,
                work_item_id,
                wake,
                resource,
                reason,
                recheck_after_ms,
            )
            .await?
        {
            WaitForRegistrationOutcome::Registered { registration } => Ok(registration),
            WaitForRegistrationOutcome::TaskResultQueued {
                task_id,
                result_message_id,
                wait_condition_id: _,
            }
            | WaitForRegistrationOutcome::TaskResultRecoveryQueued {
                task_id,
                result_message_id,
                ..
            } => Err(RuntimeError::validation(
                "task_result_already_queued",
                format!(
                    "task {task_id} completed before wait registration; result {result_message_id} was queued"
                ),
            )
            .into()),
            WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
                task_id,
                result_message_id,
                settlement_disposition,
            } => Err(RuntimeError::validation(
                "task_result_settled",
                format!(
                    "task {task_id} result {result_message_id} settled: {settlement_disposition:?}"
                ),
            )
            .into()),
            WaitForRegistrationOutcome::TaskResultAlreadyAdmitted { task_id, .. }
            | WaitForRegistrationOutcome::TaskResultInFlight { task_id, .. }
            | WaitForRegistrationOutcome::TaskResultUnavailable { task_id, .. } => {
                Err(RuntimeError::validation(
                    "task_result_wait_not_registered",
                    format!("task {task_id} result does not require a new wait"),
                )
                .into())
            }
            WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter {
                task_id,
                result_message_id,
                wait_condition_id: _,
                claimed_by_work_item_id: _,
            } => Err(RuntimeError::validation(
                "task_result_claimed_by_other_waiter",
                format!(
                    "task {task_id} result {result_message_id} was already claimed by another waiter"
                ),
            )
            .into()),
        }
    }

    pub(crate) async fn register_wait_for_outcome(
        &self,
        agent_id: &str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
    ) -> Result<WaitForRegistrationOutcome> {
        let _admission = self.execution_admission_lease()?;
        for attempt in 0..3 {
            let mut mutation = self.begin_mutation().await?;
            match mutation
                .prepare_wait_for_outcome_with_id(
                    agent_id,
                    work_item_id.clone(),
                    wake,
                    resource.clone(),
                    reason.clone(),
                    recheck_after_ms,
                    None,
                )
                .await?
            {
                PrepareWaitForOutcome::Immediate(outcome) => return Ok(outcome),
                PrepareWaitForOutcome::Prepared(prepared) => {
                    let operation_id = prepared
                        .command
                        .audit_events
                        .first()
                        .map(|event| event.id.clone())
                        .unwrap_or_else(crate::ids::audit_event_id);
                    let commit_result = mutation
                        .commit_wait_transition(
                            operation_id,
                            prepared.command.clone(),
                            prepared.execution_protocol.clone(),
                            prepared.expected_task.clone(),
                        )
                        .await;
                    drop(mutation);
                    match commit_result {
                        Ok(commit) => {
                            self.apply_transition_commit(commit).await;
                            return Ok(prepared.outcome());
                        }
                        Err(error)
                            if attempt + 1 < 3
                                && crate::runtime_db::retryable_state_transition_conflict(
                                    &error,
                                )
                                .is_some() =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        unreachable!("wait registration retry budget is non-empty")
    }

    pub(crate) async fn prepare_wait_for_outcome(
        &self,
        agent_id: &str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
    ) -> Result<PrepareWaitForOutcome> {
        let _admission = self.execution_admission_lease()?;
        self.prepare_wait_for_outcome_with_id(
            agent_id,
            work_item_id,
            wake,
            resource,
            reason,
            recheck_after_ms,
            None,
        )
        .await
    }

    pub(super) fn prepare_wait_for_outcome_with_id<'a>(
        &'a self,
        agent_id: &'a str,
        work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
        condition_id: Option<String>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PrepareWaitForOutcome>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut mutation = self.begin_mutation().await?;
            mutation
                .prepare_wait_for_outcome_with_id(
                    agent_id,
                    work_item_id,
                    wake,
                    resource,
                    reason,
                    recheck_after_ms,
                    condition_id,
                )
                .await
        })
    }

    pub(in crate::runtime) async fn prepare_wait_for_outcome_from_owned_read(
        &self,
        mutation: &mut super::state_mutation::RuntimeMutation,
        agent_id: &str,
        mut work_item_id: Option<String>,
        wake: WaitForWakeKind,
        resource: Option<String>,
        reason: String,
        recheck_after_ms: Option<u64>,
        condition_id: Option<String>,
        scheduling: Option<super::state_mutation::WaitSchedulingRead>,
    ) -> Result<PrepareWaitForOutcome> {
        let base_state = mutation.state.clone();
        let runtime_agent_id = base_state.id.as_str();
        if agent_id != runtime_agent_id {
            return Err(anyhow!("wait_for agent mismatch: {}", agent_id));
        }

        let mut expected_task = None;
        if wake == WaitForWakeKind::TaskResult {
            let task_id = wait_resource_required(wake, resource.clone())?;
            let task = self
                .inner
                .runtime_db
                .tasks()
                .latest(&task_id)?
                .ok_or_else(|| {
                    RuntimeError::not_found(
                        "task_not_found",
                        format!("wait_for task does not exist: {task_id}"),
                    )
                    .with_safe_context("task_id", &task_id)
                })?;
            // #3124: the task's captured owner never changes and never
            // restricts who may wait; an explicit waiter (resolved by the
            // caller, binding-first) wins. With no explicit waiter, keep the
            // legacy task-owner attribution so a conversation-scoped wait
            // still hands off to the WorkItem that owns the task.
            self.validate_wait_for_task_access(agent_id, &task)?;
            if work_item_id.is_none() {
                work_item_id = task.work_item_id.clone();
            }
            if task_state_reducer::is_terminal_task_status(&task.status) {
                return self
                    .prepare_terminal_task_result(
                        mutation,
                        task,
                        work_item_id,
                        reason,
                        condition_id,
                    )
                    .await;
            }
            expected_task = Some(task_expectation(&task));
        }

        let now = self.now();
        anyhow::ensure!(
            scheduling.is_some()
                == matches!(wake, WaitForWakeKind::Timer | WaitForWakeKind::External),
            "Wait Scheduling read differs from its original wake family"
        );
        let (timer_wake_at, pending_timer_wake, external_trigger_id, scheduling_material) =
            if let Some(read) = scheduling {
                let (at, pending, trigger, material) = read.into_parts();
                (at, pending, trigger, Some(material))
            } else {
                (None, None, None, None)
            };
        let (kind, subject_ref, wake_sources) =
            wait_condition_parts(wake, resource.clone(), timer_wake_at, external_trigger_id)?;
        let recheck_at = recheck_after_ms.map(|delay| recheck_at_from(now, delay));
        let mut state = base_state.clone();
        let expected_state = state.clone();
        let current_turn_id = state.current_turn_id.clone();
        let mut work_item = None;
        let active_waits = if let Some(work_item_id) = work_item_id.as_deref() {
            self.inner
                .storage
                .raw_unresolved_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .filter(|record| record.work_item_id.as_deref() == Some(work_item_id))
                .collect::<Vec<_>>()
        } else {
            self.inner
                .storage
                .raw_unresolved_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .filter(|record| record.work_item_id.is_none())
                .collect()
        };
        let replaced = replace_wait_conditions(active_waits, now);
        let mut wait_conditions = replaced.records;
        let cancelled_wait_condition_ids = replaced.cancelled_ids;
        let resolved_after_trigger_wait_condition_ids = replaced.resolved_after_trigger_ids;
        wait_conditions.reserve(1);
        let mut work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        let mut committed_agent_state = None;
        let mut condition = WaitConditionRecord {
            id: condition_id.unwrap_or_else(crate::ids::wait_condition_id),
            agent_id: agent_id.to_string(),
            work_item_id: work_item_id.clone(),
            status: WaitConditionStatus::Active,
            kind,
            source: Some("WaitFor".to_string()),
            subject_ref,
            waiting_for: reason.clone(),
            wake_sources,
            continuation: Some(serde_json::json!({
                "created_by": "WaitFor",
                "wake": wake,
                "resource": resource,
                "recheck_after_ms": recheck_after_ms,
                "recheck_at": recheck_at,
                "recovery": recheck_at.map(|_| serde_json::json!({
                    "kind": "recoverable",
                    "recheck_source": "WaitFor",
                })),
                "clear_blocker_on_task_result": wake == WaitForWakeKind::TaskResult,
            })),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: current_turn_id,
            trigger_message_id: None,
            triggered_at: None,
        };
        if let Some(timer_wake) = pending_timer_wake.as_ref() {
            condition.mark_triggered(&timer_wake.message_id, now);
        }
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": work_item_id,
                    "reason": "wait_for_replaced",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        if !resolved_after_trigger_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_resolved",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": work_item_id,
                    "reason": "wait_for_replaced_after_trigger",
                    "wait_condition_ids": &resolved_after_trigger_wait_condition_ids,
                }),
            ));
        }
        if let Some(work_item_id) = work_item_id.as_deref() {
            let existing = self.validate_owned_work_item(agent_id, work_item_id)?;
            if existing.state != WorkItemState::Open {
                return Err(RuntimeError::validation(
                    "work_item_completed",
                    format!("cannot wait on completed work item {work_item_id}"),
                )
                .with_safe_context("work_item_id", work_item_id)
                .into());
            }
            let preserve_blocker = wake == WaitForWakeKind::TaskResult
                && task_wait_preserves_blocker(&existing, &wait_conditions);
            let mut updated = WorkItemRecord {
                revision: existing.revision + 1,
                blocked_by: if preserve_blocker {
                    existing.blocked_by.clone()
                } else {
                    Some(reason.clone())
                },
                blocked_by_wait_id: if preserve_blocker {
                    existing.blocked_by_wait_id.clone()
                } else {
                    Some(condition.id.clone())
                },
                recheck_at: if preserve_blocker {
                    existing.recheck_at
                } else {
                    recheck_at
                },
                recheck_consumed_at: if preserve_blocker {
                    existing.recheck_consumed_at
                } else {
                    None
                },
                updated_at: now,
                ..existing.clone()
            };
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut updated,
            )?;
            if plan_artifact_changed {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&updated) {
                    audit_events.push(event);
                }
            }
            let mut projection_waits = wait_conditions
                .iter()
                .filter(|record| record.status == WaitConditionStatus::Active)
                .cloned()
                .collect::<Vec<_>>();
            projection_waits.push(condition.clone());
            let readiness = crate::work_item_scheduling::readiness_from_facts(
                &updated,
                false,
                &projection_waits,
            );
            audit_events.push(self.work_item_written_event_with_readiness(
                "wait_for_blocked",
                &updated,
                readiness,
                Value::Null,
            ));
            index_changes.extend(self.inner.storage.index_changes_for_work_item(&updated)?);
            work_items.push(crate::runtime_db::transitions::WorkItemMutation::Update {
                record: updated.clone(),
                expected_revision: existing.revision,
            });
            if state.current_turn_work_item_id.as_deref() == Some(updated.id.as_str()) {
                state.current_turn_work_item_id = None;
                audit_events.push(AuditEvent::legacy(
                    "work_item_turn_binding_released",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": updated.id.as_str(),
                        "reason": "work_item_waiting",
                        "readiness": readiness,
                        "revision": updated.revision,
                    }),
                ));
                committed_agent_state = Some(state.clone());
            }
            if wake == WaitForWakeKind::OperatorInput
                && state.current_work_item_id.as_deref() == Some(updated.id.as_str())
            {
                state.current_work_item_id = None;
                audit_events.push(AuditEvent::legacy(
                    "work_item_focus_released",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "work_item_id": updated.id.as_str(),
                        "reason": "operator_input_wait",
                        "readiness": readiness,
                        "revision": updated.revision,
                    }),
                ));
                committed_agent_state = Some(state.clone());
            }
            work_item = Some(updated);
        }

        wait_conditions.push(condition.clone());
        audit_events.extend(
            self.inner
                .storage
                .wait_condition_auxiliary_events(&condition),
        );
        audit_events.push(AuditEvent::legacy(
            "wait_condition_registered",
            serde_json::json!({
                "agent_id": agent_id,
                "work_item_id": work_item_id,
                "wait_condition_id": condition.id,
                "source": "WaitFor",
                "continuation": WaitForContinuation::YieldAndWait,
                "kind": &condition.kind,
                "subject_ref": &condition.subject_ref,
                "waiting_for": &condition.waiting_for,
                "wake_sources": &condition.wake_sources,
                "cancelled_wait_condition_ids": &cancelled_wait_condition_ids,
            }),
        ));
        let execution_protocol = self
            .execution_wait_settlement_transition(&condition, work_item.as_ref(), now)
            .await?;
        let command = crate::runtime_db::transitions::WaitTransitionCommand {
            task_result_admission: None,
            agent_id: agent_id.to_string(),
            work_items,
            expected_wait_conditions: Vec::new(),
            wait_conditions,
            timer_wake: pending_timer_wake.as_ref().map(|wake| {
                crate::runtime_db::transitions::TimerWakeClaim {
                    timer_id: wake.timer_id.clone(),
                    message_id: wake.message_id.clone(),
                    fire_count: wake.fire_count,
                }
            }),
            agent_state: committed_agent_state.map(|record| {
                crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(expected_state)),
                    record: Box::new(record),
                }
            }),
            audit_events,
            index_changes,
            notify_scheduler: true,
            fault: self.take_transition_fault(),
        };
        Ok(PrepareWaitForOutcome::Prepared(Box::new(
            PreparedWaitForSettlement {
                delivery: crate::tool::tools::wait_for::WaitForDeliveryArg::Silent,
                wake,
                registration: WaitForRegistration {
                    scope: if condition.work_item_id.is_some() {
                        WaitForScope::WorkItem
                    } else {
                        WaitForScope::Agent
                    },
                    condition,
                    recheck_after_ms,
                    recheck_at,
                    work_item,
                    cancelled_wait_condition_ids,
                },
                command,
                execution_protocol,
                expected_task,
                tool_execution: None,
                brief: None,
                brief_publication_scope: None,
                _scheduling_material: scheduling_material.into_iter().collect(),
            },
        )))
    }

    fn validate_wait_for_task_access(&self, agent_id: &str, task: &TaskRecord) -> Result<()> {
        if task.agent_id != agent_id {
            return Err(RuntimeError::validation(
                "task_agent_mismatch",
                format!("wait_for task belongs to another agent: {}", task.id),
            )
            .with_safe_context("task_id", &task.id)
            .into());
        }
        Ok(())
    }

    async fn validate_wait_for_work_item(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<WorkItemRecord> {
        let runtime = self.clone();
        let agent_id = agent_id.to_owned();
        let work_item_id = work_item_id.to_owned();
        tokio::task::spawn_blocking(move || {
            runtime.validate_owned_work_item(&agent_id, &work_item_id)
        })
        .await?
    }

    async fn prepare_terminal_task_result(
        &self,
        mutation: &mut super::state_mutation::RuntimeMutation,
        task: TaskRecord,
        waiter_work_item_id: Option<String>,
        reason: String,
        condition_id: Option<String>,
    ) -> Result<PrepareWaitForOutcome> {
        // The terminal fast path keeps the normal registration's waiter
        // restrictions: an explicit waiter must be an owned, still-open
        // WorkItem (#3124).
        let mut existing_waiter_work_item = None;
        if let Some(work_item_id) = waiter_work_item_id.as_deref() {
            let existing = self
                .validate_wait_for_work_item(&task.agent_id, work_item_id)
                .await?;
            if existing.state != WorkItemState::Open {
                return Err(RuntimeError::validation(
                    "work_item_completed",
                    format!("cannot wait on completed work item {work_item_id}"),
                )
                .with_safe_context("work_item_id", work_item_id)
                .into());
            }
            existing_waiter_work_item = Some(existing);
        }
        let result_message_id = task.parent_message_id.clone().ok_or_else(|| {
            RuntimeError::validation(
                "task_result_evidence_missing",
                format!(
                    "terminal task has no exact result message identity: {}",
                    task.id
                ),
            )
            .with_safe_context("task_id", &task.id)
        })?;
        let storage = self.inner.storage.clone();
        let read_result_message_id = result_message_id.clone();
        let result_message = tokio::task::spawn_blocking(move || {
            storage.read_message_by_id(&read_result_message_id)
        })
        .await??
        .ok_or_else(|| {
            RuntimeError::validation(
                "task_result_evidence_missing",
                format!(
                    "terminal task result message evidence is missing: {}",
                    task.id
                ),
            )
            .with_safe_context("task_id", &task.id)
            .with_safe_context("result_message_id", &result_message_id)
        })?;
        use super::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey, TaskReadError};
        let request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: std::collections::BTreeSet::from([TaskKey::new(
                TaskFamily::Task,
                &task.id,
            )]),
            recovery_source_ids: Default::default(),
        };
        mutation.prepare_owner_task_request(&request).await?;
        let task_read = mutation.capture_task_read(&request)?;
        let result_ids = task_read
            .tasks
            .coverage
            .task_result_ids
            .get(&task.id)
            .ok_or_else(|| {
                TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::Task, &task.id))
            })?;
        let mut settlement_record = None;
        for id in result_ids {
            let metadata = task_read
                .tasks
                .coverage
                .result_metadata
                .get(id)
                .ok_or_else(|| {
                    TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
                })?;
            if metadata.message_id == result_message_id {
                anyhow::ensure!(
                    settlement_record.is_none(),
                    "Task result Message is not unique"
                );
                settlement_record = Some(super::task_reads::result_record(
                    &task_read.tasks,
                    metadata,
                )?);
            }
        }
        let Some(settlement_record) = settlement_record else {
            return Ok(PrepareWaitForOutcome::Immediate(
                WaitForRegistrationOutcome::TaskResultUnavailable {
                    task_id: task.id,
                    result_message_id,
                    reason: "legacy_untracked_result".into(),
                },
            ));
        };
        let settlement_material = self.inner.input_evidence_budget.register_owned_bytes(
            &task_read.tasks.scope,
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: super::input_evidence_budget::serialized_bytes(settlement_record)?
                    .checked_mul(8)
                    .context("Task result admission material overflow")?,
                metadata_estimate_bytes: u64::try_from(std::mem::size_of_val(settlement_record))?
                    .checked_mul(8)
                    .context("Task result admission metadata overflow")?,
            },
        )?;
        let settlement_record = settlement_record.clone();
        let state = task_read.state.as_ref().clone();
        let current_activation_id = state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.activation_id.clone());
        use crate::runtime_db::task_result_settlement::TaskResultObligation;
        let db = self.inner.runtime_db.clone();
        let read_settlement = settlement_record.clone();
        let read_waiter_work_item_id = waiter_work_item_id.clone();
        let read_current_activation_id = current_activation_id.clone();
        let obligation = tokio::task::spawn_blocking(move || {
            db.task_result_settlements().classify_obligation(
                &read_settlement,
                read_current_activation_id.as_deref(),
                read_waiter_work_item_id.as_deref(),
            )
        })
        .await??;
        let immediate = match obligation {
            TaskResultObligation::AlreadyAdmitted { activation_id } => {
                Some(WaitForRegistrationOutcome::TaskResultAlreadyAdmitted {
                    task_id: task.id.clone(),
                    result_message_id: result_message_id.clone(),
                    activation_id,
                })
            }
            TaskResultObligation::InFlight { activation_id } => {
                Some(WaitForRegistrationOutcome::TaskResultInFlight {
                    task_id: task.id.clone(),
                    result_message_id: result_message_id.clone(),
                    activation_id,
                })
            }
            TaskResultObligation::Settled { disposition } => {
                Some(WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
                    task_id: task.id.clone(),
                    result_message_id: result_message_id.clone(),
                    settlement_disposition: disposition,
                })
            }
            TaskResultObligation::ClaimedByOtherWaiter {
                wait_condition_id,
                work_item_id,
            } => Some(WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter {
                task_id: task.id.clone(),
                result_message_id: result_message_id.clone(),
                wait_condition_id,
                claimed_by_work_item_id: work_item_id,
            }),
            TaskResultObligation::Paused | TaskResultObligation::InvalidOrStale => {
                Some(WaitForRegistrationOutcome::TaskResultUnavailable {
                    task_id: task.id.clone(),
                    result_message_id: result_message_id.clone(),
                    reason: if obligation == TaskResultObligation::Paused {
                        "recovery_paused_requires_operator"
                    } else {
                        "invalid_or_stale_result"
                    }
                    .into(),
                })
            }
            TaskResultObligation::Recoverable => None,
        };
        if let Some(outcome) = immediate {
            return Ok(PrepareWaitForOutcome::Immediate(outcome));
        }
        let result_message = if let Some(reply_id) =
            crate::wake_contract::agent_message_reply_reference(&result_message)
        {
            let storage = self.inner.storage.clone();
            let reply_id = reply_id.to_owned();
            tokio::task::spawn_blocking(move || storage.read_message_by_id(&reply_id))
                .await??
                .ok_or_else(|| anyhow!("agent reply message is missing"))?
        } else {
            result_message
        };
        let result_message_id = result_message.id.clone();
        let storage = self.inner.storage.clone();
        let existing_entry = tokio::task::spawn_blocking(move || storage.latest_queue_entries())
            .await??
            .into_iter()
            .find(|entry| entry.message_id == result_message_id);
        let source_message_id = result_message_id.clone();
        let source_queue = existing_entry.clone();
        let canonical_recovery = existing_entry.as_ref().is_some_and(|entry| {
            matches!(
                entry.status,
                QueueEntryStatus::Dequeued
                    | QueueEntryStatus::Interjected
                    | QueueEntryStatus::Processed
            )
        });
        let (result_message, existing_entry) = if canonical_recovery {
            let runtime = self.clone();
            let read_settlement = settlement_record.clone();
            let read_waiter_work_item_id = waiter_work_item_id.clone();
            tokio::task::spawn_blocking(move || {
                runtime.task_result_repair_source(
                    &read_settlement,
                    read_waiter_work_item_id.as_deref(),
                )
            })
            .await??
        } else {
            (result_message, existing_entry)
        };
        let result_message_id = result_message.id.clone();
        let mut work_request = super::work_reads::live_work_request();
        work_request
            .required_trigger_message_ids
            .extend([source_message_id.clone(), result_message_id.clone()]);
        if let Some(id) = condition_id.as_deref() {
            work_request
                .mandatory_roots
                .insert(super::resident_work::WorkKey::new(
                    super::resident_work::WorkFamily::Wait,
                    id,
                ));
        }
        mutation.prepare_owner_work_request(&work_request).await?;
        let work = mutation.capture_work(&work_request)?;
        let (wait_witness, wait_material) = self.capture_task_result_wait_witness(
            mutation,
            &work,
            condition_id.as_deref(),
            &[&source_message_id, &result_message_id],
        )?;
        let agent_wait_conditions: Vec<_> = wait_witness
            .live_waits
            .iter()
            .chain(&wait_witness.exact_waits)
            .cloned()
            .collect();
        let carries_result_trigger = |record: &WaitConditionRecord| {
            matches!(
                record.status,
                WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
            ) && record.kind == WaitConditionKind::Task
                && record.trigger_message_id() == Some(result_message_id.as_str())
                && record.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::TaskResult { task_id } if task_id == &task.id
                    )
                })
        };
        let existing_condition = agent_wait_conditions
            .iter()
            .find(|record| {
                condition_id.as_ref().is_none_or(|id| id == &record.id)
                    && record.work_item_id == waiter_work_item_id
                    && carries_result_trigger(record)
            })
            .cloned();
        // The wake layer admits exactly one wait per result message
        // (UNIQUE(agent_id, trigger_message_id)); when another same-agent
        // waiter already carries this trigger, report a non-fatal
        // disposition instead of writing a duplicate record (#3124).
        if existing_condition.is_none() {
            if let Some(claimed) = agent_wait_conditions.iter().find(|record| {
                record.work_item_id != waiter_work_item_id && carries_result_trigger(record)
            }) {
                return Ok(PrepareWaitForOutcome::Immediate(
                    WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter {
                        task_id: task.id.clone(),
                        result_message_id,
                        wait_condition_id: claimed.id.clone(),
                        claimed_by_work_item_id: claimed.work_item_id.clone(),
                    },
                ));
            }
        }

        let now = self.now();
        // The fast path must leave the same durable wait semantics as a
        // normal WaitFor registration: the re-queued result message is the
        // exact promised wake, so the wait condition starts Triggered and
        // replaced same-scope waits are retired atomically with the queue
        // admission. Without this record the terminal result can be resolved
        // as liveness_only and the agent never re-enters the model.
        let current_turn_id = self.agent_state().await?.current_turn_id.clone();
        let mut condition = existing_condition.unwrap_or_else(|| WaitConditionRecord {
            id: condition_id.unwrap_or_else(crate::ids::wait_condition_id),
            agent_id: task.agent_id.clone(),
            work_item_id: waiter_work_item_id.clone(),
            status: WaitConditionStatus::Triggered,
            kind: WaitConditionKind::Task,
            source: Some("WaitFor".to_string()),
            subject_ref: Some(task.id.clone()),
            waiting_for: reason,
            wake_sources: vec![WakeSource::TaskResult {
                task_id: task.id.clone(),
            }],
            continuation: Some(serde_json::json!({
                "created_by": "WaitFor",
                "wake": WaitForWakeKind::TaskResult,
                "resource": task.id,
                "recheck_after_ms": null,
                "recheck_at": null,
                "clear_blocker_on_task_result": true,
                "late_result_settled": true,
            })),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: current_turn_id,
            trigger_message_id: Some(result_message_id.clone()),
            triggered_at: Some(now),
        });
        let mut result_message = result_message;
        if canonical_recovery {
            condition.status = WaitConditionStatus::Triggered;
            condition.resolved_at = None;
            condition.updated_at = now;
            condition.trigger_message_id = Some(result_message.id.clone());
            condition.triggered_at = Some(now);
            if existing_entry.is_none() {
                result_message
                    .source_refs
                    .insert("wait_id".into(), condition.id.clone());
            }
        }
        let unresolved_waits = wait_witness.live_waits.clone();
        let replaced = replace_wait_conditions(
            unresolved_waits
                .into_iter()
                .filter(|record| record.id != condition.id)
                .filter(|record| match waiter_work_item_id.as_deref() {
                    Some(work_item_id) => record.work_item_id.as_deref() == Some(work_item_id),
                    None => record.work_item_id.is_none(),
                })
                .collect(),
            now,
        );
        let mut wait_conditions = replaced.records;
        let cancelled_wait_condition_ids = replaced.cancelled_ids;
        let resolved_after_trigger_wait_condition_ids = replaced.resolved_after_trigger_ids;
        let mut audit_events = vec![AuditEvent::legacy(
            "wait_condition_registered",
            serde_json::json!({
                "agent_id": task.agent_id,
                "work_item_id": waiter_work_item_id,
                "wait_condition_id": condition.id,
                "source": "WaitFor",
                "kind": &condition.kind,
                "subject_ref": &condition.subject_ref,
                "waiting_for": &condition.waiting_for,
                "wake_sources": &condition.wake_sources,
                "cancelled_wait_condition_ids": &cancelled_wait_condition_ids,
                "initial_status": "triggered",
                "continuation": WaitForContinuation::YieldAndReenter,
                "trigger_message_id": result_message_id,
            }),
        )];
        audit_events.extend(
            self.inner
                .storage
                .wait_condition_auxiliary_events(&condition),
        );
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": task.agent_id,
                    "work_item_id": waiter_work_item_id,
                    "reason": "wait_for_replaced",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        if !resolved_after_trigger_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_resolved",
                serde_json::json!({
                    "agent_id": task.agent_id,
                    "work_item_id": waiter_work_item_id,
                    "reason": "wait_for_replaced_after_trigger",
                    "wait_condition_ids": &resolved_after_trigger_wait_condition_ids,
                }),
            ));
        }
        audit_events.push(AuditEvent::legacy(
            "wait_condition_triggered",
            serde_json::json!({
                "agent_id": task.agent_id,
                "wait_condition_id": condition.id,
                "trigger_message_id": result_message_id,
                "work_item_id": waiter_work_item_id,
            }),
        ));
        wait_conditions.push(condition.clone());
        audit_events.push(AuditEvent::legacy(
            "late_task_result_queued",
            serde_json::json!({
                "continuation": WaitForContinuation::YieldAndReenter,
                "agent_id": task.agent_id,
                "task_id": task.id,
                "result_message_id": result_message_id,
                "wait_condition_id": condition.id,
                "work_item_id": waiter_work_item_id,
            }),
        ));
        // Like ordinary registration, the execution waits on this exact
        // condition while an independent business blocker stays unchanged.
        let mut work_items = Vec::new();
        let mut index_changes = Vec::new();
        let mut work_item = None;
        let mut blocked_readiness = Value::Null;
        if let Some(existing) = existing_waiter_work_item.as_ref() {
            let (blocked_by, blocked_by_wait_id) = match existing.blocked_by.clone() {
                Some(blocker) if task_wait_preserves_blocker(existing, &wait_conditions) => {
                    (Some(blocker), existing.blocked_by_wait_id.clone())
                }
                _ => (
                    Some(condition.waiting_for.clone()),
                    Some(condition.id.clone()),
                ),
            };
            let mut updated = WorkItemRecord {
                revision: existing.revision + 1,
                blocked_by,
                blocked_by_wait_id,
                recheck_at: existing.recheck_at,
                recheck_consumed_at: existing.recheck_consumed_at,
                updated_at: now,
                ..existing.clone()
            };
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut updated,
            )?;
            if plan_artifact_changed {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&updated) {
                    audit_events.push(event);
                }
            }
            let mut projection_waits = wait_conditions
                .iter()
                .filter(|record| record.status == WaitConditionStatus::Active)
                .cloned()
                .collect::<Vec<_>>();
            projection_waits.push(condition.clone());
            let readiness = crate::work_item_scheduling::readiness_from_facts(
                &updated,
                false,
                &projection_waits,
            );
            blocked_readiness = serde_json::to_value(readiness).unwrap_or(Value::Null);
            audit_events.push(self.work_item_written_event_with_readiness(
                "wait_for_blocked",
                &updated,
                readiness,
                Value::Null,
            ));
            index_changes.extend(self.inner.storage.index_changes_for_work_item(&updated)?);
            work_items.push(crate::runtime_db::transitions::WorkItemMutation::Update {
                expected_revision: existing.revision,
                record: updated.clone(),
            });
            work_item = Some(updated);
        }
        let expected_task = task_expectation(&task);
        let execution_protocol = self
            .execution_wait_settlement_transition(&condition, work_item.as_ref(), now)
            .await?;
        let (already_in_memory, queue_len, expected_state, mut committed_state) = self
            .read_committed_agent(|guard| {
                (
                    guard
                        .queue
                        .peek_next_matching(|message| message.id == result_message_id)
                        .is_some(),
                    guard.queue.len(),
                    guard.last_persisted_state.clone(),
                    guard.state.clone(),
                )
            })
            .await?;
        committed_state.pending = queue_len.saturating_add(usize::from(!already_in_memory));
        committed_state.last_wake_reason = Some("TaskResult".into());
        let storage = self.inner.storage.clone();
        committed_state.total_message_count =
            tokio::task::spawn_blocking(move || storage.count_messages()).await??
                + usize::from(canonical_recovery && existing_entry.is_none());
        scheduler::apply_message_wake_projection(&mut committed_state);
        if let (Some(work_item_id), Some(updated)) =
            (waiter_work_item_id.as_deref(), work_item.as_ref())
        {
            if committed_state.current_turn_work_item_id.as_deref() == Some(work_item_id) {
                committed_state.current_turn_work_item_id = None;
                audit_events.push(AuditEvent::legacy(
                    "work_item_turn_binding_released",
                    serde_json::json!({
                        "agent_id": task.agent_id,
                        "work_item_id": work_item_id,
                        "reason": "work_item_waiting",
                        "readiness": blocked_readiness,
                        "revision": updated.revision,
                    }),
                ));
            }
        }
        let queue_entry = QueueEntryRecord {
            message_id: result_message_id,
            agent_id: task.agent_id.clone(),
            priority: result_message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: existing_entry
                .as_ref()
                .map_or(result_message.created_at, |entry| entry.created_at),
            updated_at: now,
        };
        Ok(PrepareWaitForOutcome::Prepared(Box::new(
            PreparedWaitForSettlement {
                delivery: crate::tool::tools::wait_for::WaitForDeliveryArg::Silent,
                wake: WaitForWakeKind::TaskResult,
                registration: WaitForRegistration {
                    scope: if waiter_work_item_id.is_some() {
                        WaitForScope::WorkItem
                    } else {
                        WaitForScope::Agent
                    },
                    condition,
                    recheck_after_ms: None,
                    recheck_at: None,
                    work_item: work_item.clone(),
                    cancelled_wait_condition_ids,
                },
                command: crate::runtime_db::transitions::WaitTransitionCommand {
                    agent_id: task.agent_id,
                    work_items,
                    expected_wait_conditions: Vec::new(),
                    wait_conditions,
                    timer_wake: None,
                    task_result_admission: Some(
                        crate::runtime_db::transitions::WaitTaskResultAdmission {
                            message: result_message,
                            expected: existing_entry,
                            record: queue_entry,
                            obligation: crate::runtime_db::transitions::WaitTaskResultObligation {
                                expected: settlement_record,
                                current_activation_id,
                                waiter_work_item_id,
                                canonical_recovery,
                                source_message_id,
                                source_queue,
                                wait_witness,
                            },
                        },
                    ),
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_state)),
                        record: Box::new(committed_state),
                    }),
                    audit_events,
                    index_changes,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                },
                execution_protocol,
                expected_task: Some(expected_task),
                tool_execution: None,
                brief: None,
                brief_publication_scope: None,
                _scheduling_material: vec![settlement_material, wait_material],
            },
        )))
    }

    async fn execution_wait_settlement_transition(
        &self,
        condition: &WaitConditionRecord,
        work_item: Option<&WorkItemRecord>,
        settled_at: DateTime<Utc>,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            ConversationOutcome, ExecutionBinding, ExecutionOutcome, ExecutionOutcomeRecord,
            ExecutionProtocolCommand, SetWorkItemWaiting, SettleExecution, WaitReference,
            WorkItemExecutionRecord, WorkItemExecutionState, WorkItemOutcome,
        };

        let db = self.inner.runtime_db.clone();
        let agent_id = condition.agent_id.clone();
        let Some(state) = tokio::task::spawn_blocking(move || {
            db.transitions()
                .load_execution_protocol_state_if_initialized(&agent_id)
        })
        .await??
        else {
            return Ok(Default::default());
        };
        let Some(attempt) = state.open_attempt() else {
            return Ok(Default::default());
        };
        let wait = WaitReference {
            wait_id: condition.id.clone(),
        };
        let mut commands = Vec::with_capacity(2);
        let outcome = match &attempt.binding {
            ExecutionBinding::WorkItem { work_item_id }
                if condition.work_item_id.as_deref() == Some(work_item_id.as_str()) =>
            {
                ExecutionOutcome::WorkItem(WorkItemOutcome::Wait { wait })
            }
            ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
                if let Some(work_item_id) = condition.work_item_id.as_deref() {
                    let work_item = work_item
                        .filter(|record| record.id == work_item_id)
                        .ok_or_else(|| {
                            anyhow!("WorkItem wait handoff is missing its updated WorkItem")
                        })?;
                    let expected = state.work_items.get(work_item_id).cloned();
                    let generation = expected
                        .as_ref()
                        .map_or(work_item.revision.max(1), |record| record.generation() + 1);
                    commands.push(ExecutionProtocolCommand::SetWorkItemWaiting(Box::new(
                        SetWorkItemWaiting {
                            command_id: format!(
                                "handoff:{}:work_item_wait:{}",
                                attempt.attempt_id, condition.id
                            ),
                            work_item_id: work_item_id.to_string(),
                            expected,
                            record: WorkItemExecutionRecord {
                                source_revision: work_item.revision,
                                state: WorkItemExecutionState::Waiting {
                                    generation,
                                    wait: wait.clone(),
                                },
                            },
                        },
                    )));
                    ExecutionOutcome::Conversation(ConversationOutcome::HandoffToWorkItemWait {
                        work_item_id: work_item_id.to_string(),
                        wait,
                    })
                } else {
                    ExecutionOutcome::Conversation(ConversationOutcome::Wait { wait })
                }
            }
            ExecutionBinding::Command => {
                return Err(anyhow!("command execution cannot settle through WaitFor"));
            }
            _ => {
                return Err(anyhow!(
                    "WaitFor owner does not match the current execution binding"
                ));
            }
        };
        commands.push(ExecutionProtocolCommand::Settle(SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: format!("outcome:wait-for:{}:{}", attempt.attempt_id, condition.id),
                attempt_id: attempt.attempt_id.clone(),
                outcome,
                created_at: settled_at.to_rfc3339(),
            },
        }));
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: None,
                commands,
            },
        )
    }

    pub(super) async fn clear_work_item_blocker_for_pick(
        &self,
        agent_id: &str,
        existing: WorkItemRecord,
        work: &super::resident_work::CapturedWork,
        reason: &str,
    ) -> Result<WorkItemBlockerClearance> {
        let now = Utc::now();
        let active_waits = work
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|condition| condition.work_item_id.as_deref() == Some(existing.id.as_str()))
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
        let needs_record_write = existing.blocked_by.is_some()
            || existing.recheck_at.is_some()
            || existing.recheck_consumed_at.is_some();
        if !needs_record_write && wait_conditions.is_empty() {
            return Ok(WorkItemBlockerClearance::unchanged(existing));
        }

        let mut audit_events = Vec::new();
        if !cancelled_wait_condition_ids.is_empty() {
            audit_events.push(AuditEvent::legacy(
                "wait_conditions_cancelled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": existing.id,
                    "reason": "pick_work_item_clear_blocker",
                    "wait_condition_ids": &cancelled_wait_condition_ids,
                }),
            ));
        }
        let mut record = existing.clone();
        let mut index_changes = Vec::new();
        if needs_record_write {
            record = WorkItemRecord {
                revision: existing.revision + 1,
                blocked_by: None,
                blocked_by_wait_id: None,
                recheck_at: None,
                recheck_consumed_at: None,
                updated_at: now,
                ..existing.clone()
            };
            let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
                self.agent_home().as_path(),
                &mut record,
            )?;
            if plan_artifact_changed {
                if let Some(event) = self.work_item_plan_artifact_refreshed_event(&record) {
                    audit_events.push(event);
                }
            }
            audit_events.push(self.work_item_written_event(
                "pick_blocker_cleared",
                &record,
                serde_json::json!({
                    "reason": reason,
                    "cancelled_wait_condition_ids": cancelled_wait_condition_ids.clone(),
                }),
            ));
            index_changes = self.inner.storage.index_changes_for_work_item(&record)?;
        }
        Ok(WorkItemBlockerClearance {
            work_item: record,
            expected_revision: needs_record_write.then_some(existing.revision),
            wait_conditions,
            audit_events,
            index_changes,
            blocker_cleared: true,
            cancelled_wait_condition_ids,
        })
    }

    pub async fn submit_wake_hint(&self, hint: WakeHint) -> Result<WakeDisposition> {
        let _admission = self.execution_admission_lease()?;
        let runtime_agent_id = self.agent_id().await?;
        scheduler_executor::SchedulerDecisionExecutor::new(self)
            .reconcile_stale_run_projection(
                scheduler_executor::StaleRunProjectionBoundary::WakeHintSubmission,
            )
            .await?;
        let pending = PendingWakeHint {
            reason: hint.reason.clone(),
            description: hint.description.clone(),
            source: hint.source.clone(),
            scope: hint.scope.clone(),
            external_trigger_id: hint.external_trigger_id.clone(),
            resource: hint.resource.clone(),
            body: hint.body.clone(),
            content_type: hint.content_type.clone(),
            correlation_id: hint.correlation_id.clone(),
            causation_id: hint.causation_id.clone(),
            created_at: Utc::now(),
        };
        let work_item_id = self
            .wake_hint_work_item_id(hint.external_trigger_id.as_deref())
            .await?;

        let mut trigger_now = false;
        let disposition = {
            let mut guard = self.begin_mutation().await?;
            match guard.state.status {
                AgentStatus::Stopped => WakeDisposition::Ignored,
                AgentStatus::AwakeRunning | AgentStatus::AwaitingTask => {
                    guard.state.pending_wake_hint = Some(pending.clone());
                    guard.persist_state(&self.inner.storage).await?;
                    WakeDisposition::Coalesced
                }
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep => {
                    if guard.queue.is_empty() {
                        if guard.state.pending_wake_hint.take().is_some() {
                            guard.persist_state(&self.inner.storage).await?;
                        }
                        trigger_now = true;
                        WakeDisposition::Triggered
                    } else {
                        guard.state.pending_wake_hint = Some(pending.clone());
                        guard.persist_state(&self.inner.storage).await?;
                        WakeDisposition::Coalesced
                    }
                }
            }
        };

        let event_kind = match disposition {
            WakeDisposition::Triggered => "wake_hint_triggered",
            WakeDisposition::Coalesced => "wake_hint_coalesced",
            WakeDisposition::Ignored => "wake_hint_ignored",
        };
        self.append_original_audit_event(AuditEvent::legacy(
            event_kind,
            serde_json::json!({
                "agent_id": runtime_agent_id,
                "reason": hint.reason,
                "description": hint.description,
                "source": hint.source,
                "scope": hint.scope,
                "external_trigger_id": hint.external_trigger_id,
                "work_item_id": work_item_id,
                "resource": hint.resource,
                "body": hint.body,
                "content_type": hint.content_type,
                "correlation_id": hint.correlation_id,
                "causation_id": hint.causation_id,
            }),
        ))
        .await?;

        if trigger_now {
            if let Err(err) = self
                .emit_system_tick_from_wake_hint_with_decision(&pending)
                .await
            {
                let mut guard = self.begin_mutation().await?;
                if guard.state.pending_wake_hint.is_none() {
                    guard.state.pending_wake_hint = Some(pending);
                    guard.persist_state(&self.inner.storage).await?;
                }
                return Err(err);
            }
        }

        Ok(disposition)
    }

    pub(super) async fn emit_recovered_pending_wake_hint(&self) -> Result<()> {
        let pending_wake = {
            let guard = self.inner.agent.lock().await;
            guard.state.pending_wake_hint.clone()
        };
        if let Some(pending) = pending_wake {
            self.emit_system_tick_from_wake_hint_with_decision(&pending)
                .await?;
            let mut guard = self.begin_mutation().await?;
            if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                guard.state.pending_wake_hint = None;
                guard.persist_state(&self.inner.storage).await?;
            }
        }
        Ok(())
    }

    pub async fn schedule_timer(
        &self,
        duration_ms: u64,
        interval_ms: Option<u64>,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let admission = self.execution_admission_lease()?;
        let runtime = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = runtime
                .schedule_timer_owned(duration_ms, interval_ms, summary)
                .await;
            if let Err(error) = &result {
                tracing::warn!(error = %error, "owned timer scheduling failed");
            }
            let _ = result_tx.send(result);
        })?;
        result_rx
            .await
            .map_err(|_| anyhow!("timer scheduling worker stopped"))?
    }

    async fn schedule_timer_owned(
        &self,
        duration_ms: u64,
        interval_ms: Option<u64>,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let mutation = self.begin_mutation().await?;
        let created_at = self.now();
        let timer = TimerRecord {
            id: crate::ids::timer_id(),
            agent_id: mutation.state.id.clone(),
            created_at,
            duration_ms,
            interval_ms,
            repeat: interval_ms.is_some(),
            status: TimerStatus::Active,
            summary,
            next_fire_at: Some(advance_time(created_at, duration_ms)?),
            last_fired_at: None,
            fire_count: 0,
            calendar: None,
        };
        self.commit_timer_creation_owned(mutation, timer).await
    }

    async fn commit_timer_creation_owned(
        &self,
        mut mutation: super::state_mutation::RuntimeMutation,
        timer: TimerRecord,
    ) -> Result<TimerRecord> {
        let fault = self.take_transition_fault();
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::timer(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            timer.created_at,
            crate::runtime_db::transitions::PreparedTimerTransition::create(timer.clone(), fault),
        )?;
        let committed = mutation.commit_prepared_timer_transition(prepared).await;
        match committed {
            Ok((commit, crate::runtime_db::transitions::TimerMutationOutcome::Created { .. })) => {
                drop(mutation);
                self.apply_transition_commit(commit).await;
            }
            Ok(_) => anyhow::bail!("Timer creation returned another operation"),
            Err(error) => {
                if !mutation.reconcile_known_commit().await? {
                    return Err(error);
                }
                drop(mutation);
            }
        }
        let audit = self
            .append_original_audit_event(AuditEvent::legacy("timer_created", to_json_value(&timer)))
            .await;
        // Once creation committed, audit failure must not strand its live timer.
        self.spawn_timer_loop(timer.clone()).await?;
        audit?;
        Ok(timer)
    }

    pub async fn schedule_calendar_timer(
        &self,
        expression: impl Into<String>,
        timezone: impl Into<String>,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let admission = self.execution_admission_lease()?;
        let calendar = crate::calendar::CalendarSchedule::new(expression, timezone)?;
        let runtime = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = runtime
                .schedule_calendar_timer_owned(calendar, summary)
                .await;
            if let Err(error) = &result {
                tracing::warn!(error = %error, "owned calendar timer scheduling failed");
            }
            let _ = result_tx.send(result);
        })?;
        result_rx
            .await
            .map_err(|_| anyhow!("calendar timer scheduling worker stopped"))?
    }

    async fn schedule_calendar_timer_owned(
        &self,
        calendar: crate::calendar::CalendarSchedule,
        summary: Option<String>,
    ) -> Result<TimerRecord> {
        let mutation = self.begin_mutation().await?;
        let created_at = self.now();
        let next_fire_at = calendar.next_after(created_at)?.utc;
        let duration_ms = u64::try_from((next_fire_at - created_at).num_milliseconds())
            .context("calendar occurrence is outside supported timer range")?;
        let timer = TimerRecord {
            id: crate::ids::timer_id(),
            agent_id: mutation.state.id.clone(),
            created_at,
            duration_ms,
            interval_ms: None,
            repeat: true,
            status: TimerStatus::Active,
            summary,
            next_fire_at: Some(next_fire_at),
            last_fired_at: None,
            fire_count: 0,
            calendar: Some(calendar),
        };
        self.commit_timer_creation_owned(mutation, timer).await
    }

    pub async fn cancel_timer(&self, timer_id: &str) -> Result<TimerRecord> {
        Ok(self.cancel_timer_with_evidence(timer_id).await?.timer)
    }

    pub(crate) async fn cancel_timer_with_evidence(
        &self,
        timer_id: &str,
    ) -> Result<TimerCancellationOutcome> {
        let agent_id = self.agent_id().await?;
        for attempt in 0..super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            let mut guard = self.begin_mutation().await?;
            let read = match guard.prepare_timer_mutation_read(timer_id, None, false).await {
                Ok(read) => read,
                Err(error) if matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
                    Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ForeignRecord(key))
                    if key.family == super::resident_scheduling::SchedulingFamily::Timer && key.id == timer_id) => None,
                Err(error) => return Err(error),
            }.ok_or_else(|| {
                RuntimeError::not_found("timer_not_found", format!("timer {timer_id} not found"))
                    .with_safe_context("timer_id", timer_id)
            })?;
            let super::timer_mutation_reads::TimerMutationRead {
                timer: expected,
                pending: pending_wake,
                material: _route_material,
                ..
            } = read;
            if expected.agent_id != agent_id {
                return Err(RuntimeError::not_found(
                    "timer_not_found",
                    format!("timer {timer_id} not found"),
                )
                .with_safe_context("timer_id", timer_id)
                .into());
            }
            match expected.status {
                TimerStatus::Cancelled => {}
                TimerStatus::Completed => {
                    return Err(RuntimeError::validation(
                        "timer_completed",
                        format!("cannot cancel completed timer {timer_id}"),
                    )
                    .with_safe_context("timer_id", timer_id)
                    .into())
                }
                TimerStatus::Active => {}
            }

            let mut timer = expected.clone();
            timer.status = TimerStatus::Cancelled;
            timer.next_fire_at = None;

            let pending_message_in_memory = pending_wake.as_ref().is_some_and(|wake| {
                guard
                    .queue
                    .peek_next_matching(|message| message.id == wake.message_id)
                    .is_some()
            });
            let expected_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            committed_state.pending = guard
                .queue
                .len()
                .saturating_sub(usize::from(pending_message_in_memory));
            let command = crate::runtime_db::TimerCancel {
                expected,
                record: timer.clone(),
                cancelled_at: self.now(),
                agent_state: pending_wake
                    .as_ref()
                    .map(|_| (expected_state, committed_state.clone())),
            };
            let prepared = crate::runtime_db::transitions::PreparedInputTransition::timer(
                guard.input_evidence_scope(),
                crate::ids::audit_event_id(),
                self.now(),
                crate::runtime_db::transitions::PreparedTimerTransition::cancel(
                    command,
                    pending_wake,
                    self.take_transition_fault(),
                ),
            )?;
            let result = guard.commit_prepared_timer_transition(prepared).await;
            let mut timer_result = None;
            let result = result.map(|(commit, outcome)| {
                if let crate::runtime_db::transitions::TimerMutationOutcome::Cancelled { result } =
                    outcome
                {
                    timer_result = Some(result);
                }
                commit
            });
            match result {
                Ok(_) if timer_result.as_ref().is_some_and(|result| result.cancelled) => {
                    let result = timer_result.expect("successful timer cancellation has a result");
                    drop(guard);
                    self.append_original_audit_event(AuditEvent::legacy(
                        "timer_cancelled",
                        serde_json::json!({
                            "timer_id": timer.id,
                            "status": timer.status,
                            "fire_count": timer.fire_count,
                            "dropped_message_id": result.dropped_message_id,
                        }),
                    ))
                    .await?;
                    self.inner.notify.notify_waiters();
                    return Ok(TimerCancellationOutcome {
                        timer,
                        cancelled_wait_condition_ids: result.cancelled_wait_condition_ids,
                        blocked_work_item_ids: result.blocked_work_item_ids,
                    });
                }
                Ok(_) => {
                    drop(guard);
                    continue;
                }
                Err(error) => {
                    drop(guard);
                    let can_retry = attempt + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && super::retryable_enqueue_conflict(&error, &agent_id)
                        && self.refresh_enqueue_agent_state_baseline(&agent_id).await?;
                    if !can_retry {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("timer cancellation attempts always return or retry")
    }

    pub(crate) async fn recover_active_timers(
        &self,
        timers: super::resident_scheduling::CapturedScheduling,
    ) -> Result<()> {
        anyhow::ensure!(
            timers.coverage.active_timers_complete
                && timers.scope.agent_id == self.instance_agent_id()
                && timers.scope.identity_incarnation == self.inner.identity_incarnation,
            "Timer recovery requires the original complete Scheduling scope"
        );
        let count = timers.coverage.active_timers.len();
        if count == 0 {
            return Ok(());
        }
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("Timer recovery byte limit overflow")?
            .min(64 * 1024 * 1024);
        let record_limit = usize::try_from(limits.record_limit)?.min(65536);
        let order_bytes = u64::try_from(count)?
            .checked_mul(std::mem::size_of::<&TimerRecord>() as u64)
            .and_then(|bytes| bytes.checked_add(512))
            .context("Timer recovery order overflow")?;
        let order_records = count
            .checked_add(1)
            .context("Timer recovery order count overflow")?;
        let (retained, retained_records) = timers.selected_material_estimate()?;
        anyhow::ensure!(
            retained
                .checked_add(order_bytes)
                .is_some_and(|bytes| bytes <= byte_limit)
                && retained_records
                    .checked_add(order_records)
                    .is_some_and(|n| n <= record_limit),
            super::resident_scheduling::SchedulingReadError::ResourceExhausted(
                "original Timer recovery order"
            )
        );
        let _order_material = self.inner.input_evidence_budget.register_owned_bytes(
            &timers.scope,
            super::input_evidence_budget::InputMemoryCategory::Pins,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: order_bytes,
            },
        )?;
        let mut active = timers
            .records
            .iter()
            .filter_map(|record| match &record.fact {
                super::resident_scheduling::CanonicalSchedulingFact::Timer(timer)
                    if timers.coverage.active_timers.contains(&record.locator.key) =>
                {
                    Some(timer)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            active.len() == count,
            "Timer recovery coverage missing active body"
        );
        // Keep the original SQL's created_at DESC / timer_id ASC recovery order.
        active.sort_unstable_by(|a, b| {
            b.created_at
                .timestamp_millis()
                .cmp(&a.created_at.timestamp_millis())
                .then_with(|| a.id.cmp(&b.id))
        });
        for timer in active {
            anyhow::ensure!(
                timer.status == TimerStatus::Active,
                "Timer recovery live coverage contains a terminal Timer"
            );
            let owned = super::timer_producer_reads::OwnedTimerProducerRead::from_selected(
                &timers,
                &timer.id,
                None,
                &self.inner.input_evidence_budget,
                record_limit - order_records,
                usize::try_from(byte_limit - order_bytes)?,
            )?;
            self.recover_timer(owned).await?;
        }
        Ok(())
    }

    async fn spawn_timer_loop(&self, timer: TimerRecord) -> Result<()> {
        let _registration = self.begin_mutation().await?;
        let owned = _registration.capture_timer_producer_read(&timer)?;
        let current = owned.timer;
        if !self.accepts_execution_admission()
            || current.status != TimerStatus::Active
            || _registration.state.status == AgentStatus::Stopped
        {
            return Ok(());
        }
        if !self
            .inner
            .control_cancellations
            .timer_for(&timer.id)
            .is_empty()
        {
            return Ok(());
        }
        let timer = current;
        let runtime = self.clone();
        let token = CancellationToken::new();
        let control_lease = self
            .inner
            .control_cancellations
            .timer(&timer, token.clone());
        let handle = tokio::spawn(async move {
            let _producer_material = owned.material;
            let mut timer = timer;
            loop {
                let Some(next_fire_at) = timer.next_fire_at else {
                    break;
                };
                if next_fire_at > runtime.now() {
                    tokio::select! {
                        _ = token.cancelled() => break,
                        _ = runtime.inner.clock.sleep_until(next_fire_at) => {}
                    }
                }
                if token.is_cancelled() {
                    break;
                }
                if let Err(err) = runtime.fire_timer_record(&mut timer).await {
                    let _ = runtime
                        .append_original_audit_event(AuditEvent::legacy(
                            "timer_fire_failed",
                            serde_json::json!({
                                "timer_id": timer.id,
                                "error": err.to_string(),
                            }),
                        ))
                        .await;
                    break;
                }
                if timer.status != TimerStatus::Active {
                    break;
                }
            }
        });
        control_lease.attach(handle);
        Ok(())
    }

    async fn recover_timer(
        &self,
        owned: super::timer_producer_reads::OwnedTimerProducerRead,
    ) -> Result<()> {
        let _producer_material = owned.material;
        let now = self.now();
        let timer = normalize_recovered_timer(owned.timer, now)?;
        if timer
            .next_fire_at
            .is_some_and(|next_fire_at| next_fire_at <= now)
        {
            let mut overdue = timer.clone();
            self.fire_timer_record(&mut overdue).await?;
            if overdue.status == TimerStatus::Active {
                self.spawn_timer_loop(overdue).await?;
            }
        } else {
            self.spawn_timer_loop(timer).await?;
        }
        Ok(())
    }

    pub(super) async fn fire_timer_record(&self, timer: &mut TimerRecord) -> Result<()> {
        let mut first_guard = self.begin_mutation().await?;
        let first_read = first_guard
            .prepare_timer_mutation_read(&timer.id, Some(timer), true)
            .await?
            .context("original Timer is canonically absent")?;
        // Keep the first frozen Timer/source copies covered across State OCC retries.
        let _original_material = first_read.material.clone();
        let expected = first_read.timer.clone();
        if expected.status != TimerStatus::Active {
            *timer = expected;
            return Ok(());
        }
        let scheduled_fire_at = timer.next_fire_at;
        let work_item_id = first_read.work_item_id.clone();
        let mut first = Some((first_guard, first_read));
        let fired_at = self.now();
        let mut record = expected.clone();
        record.last_fired_at = Some(fired_at);
        record.fire_count += 1;
        if let Some(calendar) = record.calendar.as_ref() {
            record.status = TimerStatus::Active;
            record.next_fire_at = Some(calendar.next_after(fired_at)?.utc);
        } else if let Some(interval_ms) = record.interval_ms {
            record.status = TimerStatus::Active;
            record.next_fire_at = Some(next_repeating_fire_at(
                scheduled_fire_at.unwrap_or(fired_at),
                fired_at,
                interval_ms,
            )?);
        } else {
            record.status = TimerStatus::Completed;
            record.next_fire_at = None;
        }
        let mut message = MessageEnvelope {
            metadata: Some(serde_json::json!({
                "timer_id": timer.id,
                "timer_fire_count": record.fire_count,
            })),
            ..MessageEnvelope::new(
                timer.agent_id.clone(),
                MessageKind::TimerTick,
                MessageOrigin::Timer {
                    timer_id: timer.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: timer
                        .summary
                        .clone()
                        .unwrap_or_else(|| format!("timer {} fired", timer.id)),
                },
            )
            .with_admission(
                MessageDeliverySurface::TimerScheduler,
                AdmissionContext::RuntimeOwned,
            )
        };
        message.work_item_id = work_item_id;
        message.turn_id = Some(crate::ids::turn_id());
        message
            .source_refs
            .insert("timer_id".into(), timer.id.clone());
        message.normalize_admission_fields();

        for attempt in 0..super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            let (mut guard, route) = if let Some(first) = first.take() {
                first
            } else {
                let mut guard = self.begin_mutation().await?;
                let route = guard
                    .prepare_timer_mutation_read(&timer.id, Some(timer), false)
                    .await?
                    .context("original Timer disappeared during State OCC retry")?;
                (guard, route)
            };
            let _route_material = route.material;
            let pending = route.pending;

            let expected_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            committed_state.pending = guard.queue.len().saturating_add(1);
            committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
            committed_state.total_message_count =
                self.inner.storage.count_messages()?.saturating_add(1);
            scheduler::apply_message_wake_projection(&mut committed_state);
            let command = crate::runtime_db::TimerFire {
                expected: expected.clone(),
                record: record.clone(),
                message: message.clone(),
                queue_entry: QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: fired_at,
                },
                agent_state: (expected_state, committed_state.clone()),
            };
            let prepared = crate::runtime_db::transitions::PreparedInputTransition::timer(
                guard.input_evidence_scope(),
                crate::ids::audit_event_id(),
                fired_at,
                crate::runtime_db::transitions::PreparedTimerTransition::fire(
                    command,
                    pending,
                    self.take_transition_fault(),
                ),
            )?;
            let mut timer_result = None;
            let result =
                guard
                    .commit_prepared_timer_transition(prepared)
                    .await
                    .map(|(commit, outcome)| {
                        if let crate::runtime_db::transitions::TimerMutationOutcome::Fired {
                            result,
                        } = outcome
                        {
                            timer_result = Some(result);
                        }
                        commit
                    });
            match result {
                Ok(_) if timer_result.as_ref().is_some_and(|result| result.advanced) => {
                    let result = timer_result.expect("successful timer firing has a result");
                    drop(guard);
                    *timer = record.clone();
                    self.append_original_audit_event(AuditEvent::legacy(
                        "timer_fired",
                        serde_json::json!({
                            "timer_id": timer.id,
                            "summary": timer.summary.clone(),
                            "status": timer.status,
                            "fire_count": timer.fire_count,
                            "next_fire_at": timer.next_fire_at,
                            "wake_created": result.wake_created,
                        }),
                    ))
                    .await?;
                    if result.wake_created {
                        self.inner.notify.notify_one();
                    }
                    return Ok(());
                }
                Ok(_) => {
                    let current = guard
                        .capture_timer_mutation_read(&timer.id, Some(timer), false)?
                        .context("original Timer ACK has no current qualified body")?;
                    *timer = current.timer;
                    drop(guard);
                    return Ok(());
                }
                Err(error) => {
                    drop(guard);
                    let can_retry = attempt + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && super::retryable_enqueue_conflict(&error, &record.agent_id);
                    if !can_retry {
                        if super::retryable_enqueue_conflict(&error, &record.agent_id) {
                            tracing::warn!(
                                error = %error,
                                retryable = true,
                                timer_id = %timer.id,
                                attempt = attempt + 1,
                                max_attempts = super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS,
                                "timer fire OCC retries exhausted"
                            );
                        }
                        return Err(error);
                    }
                    if !self
                        .refresh_enqueue_agent_state_baseline(&record.agent_id)
                        .await?
                    {
                        tracing::warn!(
                            error = %error,
                            retryable = true,
                            timer_id = %timer.id,
                            attempt = attempt + 1,
                            max_attempts = super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS,
                            "timer fire OCC baseline refresh failed"
                        );
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("timer fire attempts always return or retry")
    }

    pub async fn latest_external_triggers(&self) -> Result<super::OwnedExternalTriggersRead> {
        Ok(self
            .capture_scheduling_records_read(0)
            .await?
            .into_external_triggers())
    }

    pub(super) async fn wake_hint_work_item_id(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(work_item_id) = self
            .wait_condition_work_item_id_for_external_trigger(external_trigger_id)
            .await?
        {
            return Ok(Some(work_item_id));
        }
        Ok(None)
    }

    async fn wait_condition_work_item_id_for_external_trigger(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(external_trigger_id) = external_trigger_id else {
            return Ok(None);
        };
        let snapshot = self
            .capture_work_read(&super::work_reads::live_work_request())
            .await?;
        let mut work_item_ids = snapshot
            .work
            .context_active_wait_conditions()?
            .into_iter()
            .filter(|condition| {
                condition.wake_sources.iter().any(|source| match source {
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some(id),
                    } => id == external_trigger_id,
                    WakeSource::ExternalIngress {
                        external_trigger_id: None,
                    } => true,
                    _ => false,
                })
            })
            .filter_map(|condition| condition.work_item_id.clone())
            .collect::<Vec<_>>();
        work_item_ids.sort();
        work_item_ids.dedup();
        Ok((work_item_ids.len() == 1).then(|| work_item_ids.remove(0)))
    }

    pub(super) async fn exact_external_wait_correlation(
        &self,
        external_trigger_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(external_trigger_id) = external_trigger_id else {
            return Ok(None);
        };
        let snapshot = self
            .capture_work_read(&super::work_reads::live_work_request())
            .await?;
        let correlations = snapshot
            .work
            .context_active_wait_conditions()?
            .into_iter()
            .filter(|condition| {
                condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::ExternalIngress {
                            external_trigger_id: Some(id),
                        } if id == external_trigger_id
                    )
                })
            })
            .map(|condition| condition.id.clone())
            .collect::<Vec<_>>();
        let [correlation] = correlations.as_slice() else {
            return Ok(None);
        };
        Ok(Some(correlation.clone()))
    }

    pub(super) async fn record_wait_reconciliation_signals(
        &self,
        message: &MessageEnvelope,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let unresolved_conditions = self
            .inner
            .storage
            .raw_unresolved_wait_conditions_for_agent(&agent_id)?;
        if unresolved_conditions.is_empty() {
            return Ok(());
        }
        let unresolved_conditions = self
            .reconciliation_conditions_for_message(&agent_id, message, unresolved_conditions)
            .await?;

        let signals = reconciliation_signals_for_message(message, &unresolved_conditions);
        for signal in &signals {
            let duplicate = self
                .inner
                .storage
                .read_recent_events(500)?
                .iter()
                .any(|event| {
                    event.kind == "wait_reconciliation_requested"
                        && event.data["dedupe_key"] == signal["dedupe_key"]
                });
            if duplicate {
                continue;
            }
            self.append_original_audit_event(AuditEvent::legacy(
                "wait_reconciliation_requested",
                signal.clone(),
            ))
            .await?;
        }

        Ok(())
    }

    async fn reconciliation_conditions_for_message(
        &self,
        agent_id: &str,
        message: &MessageEnvelope,
        active_conditions: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let operator_input = matches!(
            (&message.kind, &message.origin),
            (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. })
        );
        let callback_event = matches!(message.kind, MessageKind::CallbackEvent);
        let external_wake_hint = matches!(message.kind, MessageKind::SystemTick)
            && message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("wake_hint"))
                .and_then(|wake_hint| wake_hint.get("external_trigger_id"))
                .is_some();
        if !operator_input && !callback_event && !external_wake_hint {
            return Ok(active_conditions);
        }

        let exact_condition = self
            .canonical_consumed_wait_condition(agent_id, message, &active_conditions)
            .await?;
        if operator_input || callback_event {
            // Contentful resume messages reconcile only the exact wait
            // generation consumed by their scheduler admission. Broadly
            // matching active waits could resolve unrelated conditions.
            return Ok(exact_condition.into_iter().collect());
        }

        let mut conditions = active_conditions
            .into_iter()
            .filter(|condition| {
                !condition
                    .wake_sources
                    .iter()
                    .any(|source| matches!(source, WakeSource::ExternalIngress { .. }))
            })
            .collect::<Vec<_>>();
        conditions.extend(exact_condition);
        Ok(conditions)
    }

    async fn canonical_consumed_wait_condition(
        &self,
        agent_id: &str,
        message: &MessageEnvelope,
        active_conditions: &[WaitConditionRecord],
    ) -> Result<Option<WaitConditionRecord>> {
        use crate::domain::execution_protocol::{
            ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity,
        };

        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(agent_id)?
        else {
            return Ok(None);
        };
        let Some(attempt) = execution.attempts.values().find(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(message.id.as_str())
        }) else {
            return Ok(None);
        };
        let ExecutionSourceIdentity::TriggeredWait {
            wait_id,
            trigger_message_id,
            ..
        } = &attempt.source.identity
        else {
            return Ok(None);
        };
        if trigger_message_id != &message.id {
            return Ok(None);
        }

        let Some(condition) = active_conditions
            .iter()
            .find(|condition| condition.id == *wait_id)
        else {
            return Ok(None);
        };
        let owner_matches = match (&attempt.binding, condition.work_item_id.as_deref()) {
            (ExecutionBinding::WorkItem { work_item_id }, Some(condition_work_item_id)) => {
                work_item_id == condition_work_item_id
                    && message.work_item_id.as_deref() == Some(condition_work_item_id)
            }
            (
                ExecutionBinding::AgentLifecycle {
                    agent_id: owner_agent_id,
                },
                None,
            ) => owner_agent_id == agent_id && message.work_item_id.is_none(),
            (ExecutionBinding::Conversation { .. }, None) => message.work_item_id.is_none(),
            _ => false,
        };
        Ok(owner_matches.then(|| condition.clone()))
    }
}

fn reconciliation_signals_for_message(
    message: &MessageEnvelope,
    active_conditions: &[WaitConditionRecord],
) -> Vec<serde_json::Value> {
    let message_turn = message.turn_id.as_deref();
    active_conditions
        .iter()
        .filter(|condition| {
            // A wait condition created during the same turn that the current
            // message triggered must not be reconciled by that message.  The
            // wait is meant to be resumed by a *future* event; the triggering
            // message is by definition "before" the wait was registered.
            message_turn
                .zip(condition.turn_id.as_deref())
                .is_none_or(|(mt, ct)| mt != ct)
        })
        .filter_map(|condition| reconciliation_signal_for_condition(message, condition))
        .collect()
}

fn wait_condition_parts(
    wake: WaitForWakeKind,
    resource: Option<String>,
    timer_wake_at: Option<DateTime<Utc>>,
    external_trigger_id: Option<String>,
) -> Result<(WaitConditionKind, Option<String>, Vec<WakeSource>)> {
    match wake {
        WaitForWakeKind::OperatorInput => Ok((
            WaitConditionKind::Operator,
            resource,
            vec![WakeSource::OperatorInput],
        )),
        WaitForWakeKind::TaskResult => {
            let task_id = wait_resource_required(wake, resource)?;
            Ok((
                WaitConditionKind::Task,
                Some(task_id.clone()),
                vec![WakeSource::TaskResult { task_id }],
            ))
        }
        WaitForWakeKind::External => Ok((
            WaitConditionKind::External,
            optional_wait_resource(resource),
            vec![WakeSource::ExternalIngress {
                external_trigger_id,
            }],
        )),
        WaitForWakeKind::Timer => {
            let timer_id = wait_resource_required(wake, resource)?;
            let wake_at =
                timer_wake_at.ok_or_else(|| anyhow!("wait_for timer wake time is unavailable"))?;
            Ok((
                WaitConditionKind::Timer,
                Some(timer_id),
                vec![WakeSource::Timer { wake_at }],
            ))
        }
        WaitForWakeKind::System => Ok((
            WaitConditionKind::System,
            optional_wait_resource(resource),
            vec![WakeSource::SystemTick],
        )),
    }
}

fn optional_wait_resource(resource: Option<String>) -> Option<String> {
    resource
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn wait_resource_required(wake: WaitForWakeKind, resource: Option<String>) -> Result<String> {
    optional_wait_resource(resource)
        .ok_or_else(|| anyhow!("wait_for {:?} requires non-empty resource", wake))
}

#[cfg(test)]
mod wait_condition_parts_tests {
    use super::*;

    #[test]
    fn timer_wait_uses_supplied_timer_identity_and_deadline() {
        let wake_at = Utc::now() + chrono::Duration::seconds(30);
        let (kind, subject_ref, wake_sources) = wait_condition_parts(
            WaitForWakeKind::Timer,
            Some("timer-1".into()),
            Some(wake_at),
            None,
        )
        .unwrap();

        assert_eq!(kind, WaitConditionKind::Timer);
        assert_eq!(subject_ref.as_deref(), Some("timer-1"));
        assert_eq!(wake_sources, vec![WakeSource::Timer { wake_at }]);
    }

    #[test]
    fn system_wait_uses_system_tick_source() {
        let (kind, subject_ref, wake_sources) =
            wait_condition_parts(WaitForWakeKind::System, None, None, None).unwrap();

        assert_eq!(kind, WaitConditionKind::System);
        assert_eq!(subject_ref, None);
        assert_eq!(wake_sources, vec![WakeSource::SystemTick]);
    }
}

fn recheck_at_from(now: DateTime<Utc>, recheck_after_ms: u64) -> DateTime<Utc> {
    let recheck_after_ms = i64::try_from(recheck_after_ms).unwrap_or(i64::MAX);
    let recheck_after =
        chrono::Duration::try_milliseconds(recheck_after_ms).unwrap_or(chrono::Duration::MAX);
    now.checked_add_signed(recheck_after)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn reconciliation_signal_for_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<serde_json::Value> {
    let wake = crate::wake_contract::matching_wake_source(message, condition)?;
    let dedupe_key = format!(
        "wait_reconciliation:{}:{}:{}",
        condition.id, wake.source, message.id
    );
    Some(serde_json::json!({
        "dedupe_key": dedupe_key,
        "message_id": message.id,
        "trigger_kind": message.trigger_kind,
        "wait_condition_id": condition.id,
        "wake_source": wake.source,
        "work_item_id": condition.work_item_id,
        "subject_ref": wake.subject_ref.or(condition.subject_ref.as_deref()),
        "waiting_for": condition.waiting_for,
        "source": condition.source,
    }))
}

fn task_expectation(task: &TaskRecord) -> crate::runtime_db::transitions::TaskExpectation {
    crate::runtime_db::transitions::TaskExpectation {
        id: task.id.clone(),
        agent_id: task.agent_id.clone(),
        work_item_id: task.work_item_id.clone(),
        status: task.status.clone(),
        updated_at: task.updated_at,
        result_message_id: task.parent_message_id.clone(),
    }
}

fn advance_time(base: chrono::DateTime<Utc>, delta_ms: u64) -> Result<chrono::DateTime<Utc>> {
    let delta_ms = i64::try_from(delta_ms).context("duration_ms exceeds supported timer range")?;
    let delta = chrono::Duration::try_milliseconds(delta_ms)
        .ok_or_else(|| anyhow!("duration_ms exceeds supported timer range"))?;
    base.checked_add_signed(delta)
        .ok_or_else(|| anyhow!("timer deadline exceeds supported date range"))
}

fn next_repeating_fire_at(
    scheduled_fire_at: DateTime<Utc>,
    fired_at: DateTime<Utc>,
    interval_ms: u64,
) -> Result<DateTime<Utc>> {
    let interval_ms =
        i64::try_from(interval_ms).context("duration_ms exceeds supported timer range")?;
    anyhow::ensure!(
        interval_ms > 0,
        "repeating timer interval must be greater than zero"
    );

    let first_next_fire_at = advance_time(scheduled_fire_at, interval_ms as u64)?;
    if first_next_fire_at > fired_at {
        return Ok(first_next_fire_at);
    }

    let elapsed_ms = fired_at
        .signed_duration_since(scheduled_fire_at)
        .num_milliseconds();
    let periods = elapsed_ms
        .checked_div(interval_ms)
        .and_then(|periods| periods.checked_add(1))
        .context("repeating timer schedule exceeds supported timer range")?;
    let next_delta_ms = interval_ms
        .checked_mul(periods)
        .context("repeating timer schedule exceeds supported timer range")?;
    advance_time(
        scheduled_fire_at,
        u64::try_from(next_delta_ms)
            .context("repeating timer schedule exceeds supported timer range")?,
    )
}

fn normalize_recovered_timer(mut timer: TimerRecord, now: DateTime<Utc>) -> Result<TimerRecord> {
    if timer.next_fire_at.is_some() {
        return Ok(timer);
    }

    if let Some(calendar) = timer.calendar.as_ref() {
        timer.next_fire_at = Some(calendar.next_after(now)?.utc);
    } else {
        let anchor = timer.last_fired_at.unwrap_or(timer.created_at);
        let fallback_ms = timer.interval_ms.unwrap_or(timer.duration_ms);
        timer.next_fire_at = advance_time(anchor, fallback_ms).ok().or(Some(now));
    }
    Ok(timer)
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use crate::runtime::tests::support::context_config;
    use tempfile::{tempdir, TempDir};

    async fn timer_runtime() -> (TempDir, TempDir, RuntimeHandle, TimerRecord) {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let now = runtime.now();
        let timer = TimerRecord {
            calendar: None,
            id: crate::ids::timer_id(),
            agent_id: "default".into(),
            created_at: now,
            duration_ms: 1_000,
            interval_ms: Some(1_000),
            repeat: true,
            status: TimerStatus::Active,
            summary: None,
            next_fire_at: Some(now),
            last_fired_at: None,
            fire_count: 0,
        };
        crate::runtime::tests::support::persist_timer_creation_for_restart(&runtime, &timer)
            .await
            .unwrap();
        (home, workspace, runtime, timer)
    }

    async fn cancelled_timer_schedule_finishes_owned_installation(
        fault: Option<TransitionFaultPoint>,
    ) {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        if let Some(fault) = fault {
            runtime.inject_next_transition_fault(fault);
        }
        let installation = runtime.inner.installation_barrier.lock().await;
        let caller = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .schedule_timer(0, None, Some("owned timer".into()))
                    .await
            }
        });
        let timer = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(timer) = runtime.storage().latest_timer_records().unwrap().pop() {
                    assert!(runtime.inner.mutation_owner.capture().is_err());
                    break timer;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("creation SQL must finish while installation is blocked");
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(installation);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.join_idle_owned_tasks(),
        )
        .await
        .unwrap()
        .unwrap();
        let latest = runtime
            .storage()
            .latest_timer_record(&timer.id)
            .unwrap()
            .unwrap();
        assert_eq!(latest.status, TimerStatus::Completed);
        assert_eq!(latest.fire_count, 1);
        assert_eq!(runtime.storage().latest_timer_records().unwrap().len(), 1);
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
        assert_eq!(runtime.inner.agent.lock().await.queue.len(), 1);
        assert_eq!(
            runtime
                .storage()
                .read_recent_events(64)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "timer_created")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_timer_schedule_installs_and_starts_committed_timer() {
        cancelled_timer_schedule_finishes_owned_installation(None).await;
    }

    #[tokio::test]
    async fn cancelled_timer_schedule_recovers_known_fault_without_recreating_timer() {
        cancelled_timer_schedule_finishes_owned_installation(Some(
            TransitionFaultPoint::BeforeCacheUpdate,
        ))
        .await;
    }

    #[tokio::test]
    async fn accepted_timer_blocked_on_owner_remains_active_without_spawning_after_retirement() {
        let (_home, _workspace, runtime, _) = timer_runtime().await;
        let owner = runtime.begin_mutation().await.unwrap();
        let mut caller = Box::pin(runtime.schedule_timer(
            3_600_000,
            Some(3_600_000),
            Some("accepted before retirement".into()),
        ));
        assert!(futures_util::poll!(caller.as_mut()).is_pending());
        runtime.retire_instance_locally("runtime_unloaded").await;
        runtime.inner.control_cancellations.cancel_local_producers();
        drop(owner);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.join_owned_mutations(),
        )
        .await
        .unwrap()
        .unwrap();
        runtime.inner.control_cancellations.cancel_local_producers();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runtime.inner.control_cancellations.drain_retiring(),
        )
        .await
        .unwrap()
        .unwrap();
        let timer = caller.await.unwrap();
        let saved = runtime
            .inner
            .runtime_db
            .timers()
            .latest(&timer.id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.status, TimerStatus::Active);
        assert_eq!(saved.fire_count, 0);
        assert!(runtime
            .inner
            .control_cancellations
            .timer_for(&timer.id)
            .is_empty());
        assert_ne!(
            runtime.agent_state().await.unwrap().status,
            AgentStatus::Stopped
        );
    }

    #[tokio::test]
    async fn timer_fire_precommit_failure_keeps_state_and_queue_private() {
        let (_home, _workspace, runtime, mut timer) = timer_runtime().await;
        let previous = runtime.agent_state().await.unwrap();
        runtime.inject_next_transition_fault(TransitionFaultPoint::AfterCanonicalWrites);
        assert!(runtime.fire_timer_record(&mut timer).await.is_err());
        assert_eq!(runtime.agent_state().await.unwrap(), previous);
        assert_eq!(runtime.storage().read_agent().unwrap().unwrap(), previous);
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert_eq!(
            runtime
                .storage()
                .latest_timer_record(&timer.id)
                .unwrap()
                .unwrap()
                .fire_count,
            0
        );
        assert!(runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn timer_known_publication_recovery_preserves_compound_queue_and_cancel() {
        let (_home, _workspace, runtime, mut timer) = timer_runtime().await;
        let work = Box::pin(runtime.create_work_item(
            "recover triggered timer Wait".into(),
            None,
            None,
            Vec::new(),
        ))
        .await
        .unwrap();
        let registration = Box::pin(runtime.register_wait_for(
            "default",
            Some(work.id),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "timer wake".into(),
            None,
        ))
        .await
        .unwrap();
        runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        assert!(runtime.fire_timer_record(&mut timer).await.is_err());
        assert!(runtime.inner.mutation_owner.capture().is_err());
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert_eq!(
            runtime
                .storage()
                .latest_timer_record(&timer.id)
                .unwrap()
                .unwrap()
                .fire_count,
            1
        );
        let mutation = runtime.begin_mutation().await.unwrap();
        assert_eq!(mutation.state.pending, 1);
        assert_eq!(mutation.queue.len(), 1);
        let message_id = mutation.queue.peek().unwrap().id.clone();
        let inputs = mutation.capture_input_evidence().unwrap();
        assert!(inputs
            .lookup(
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &message_id,
                ),
            )
            .unwrap()
            .is_some());
        drop(inputs);
        let work = mutation
            .capture_work(&super::super::resident_work::WorkCaptureRequest::current())
            .unwrap();
        let wait = work
            .lookup(&super::super::resident_work::WorkKey::new(
                super::super::resident_work::WorkFamily::Wait,
                &registration.condition.id,
            ))
            .unwrap()
            .unwrap();
        assert!(matches!(
            &wait.record,
            super::super::resident_work::CanonicalWorkRecord::Wait(wait)
                if wait.status == WaitConditionStatus::Triggered
                    && wait.trigger_message_id() == Some(message_id.as_str())
        ));
        drop(work);
        drop(mutation);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap()
                .unwrap()
                .message_id,
            message_id
        );
        let cancelled = runtime.cancel_timer(&timer.id).await.unwrap();
        assert_eq!(cancelled.status, TimerStatus::Cancelled);
        assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&message_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Dropped
        );
    }
}

#[cfg(test)]
#[path = "timer_producer_reads_tests.rs"]
mod timer_producer_resident_tests;
