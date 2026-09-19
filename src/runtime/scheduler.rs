use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::domain::scheduler::{SchedulerOwner, SchedulerScenarioClass};
use crate::runtime::closure::runtime_error_active;
use crate::storage::{AppStorage, WorkQueueReadModel};
use crate::types::{
    AdmissionContext, AgentPostureProjection, AgentSchedulingPosture, AgentStatus, AuthorityClass,
    ExternalWaitRecoverability, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, PendingWakeHint, Priority, TaskRecord, TaskStatus, TimerStatus,
    TurnTerminalKind, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
    WorkItemRecord, WorkItemSchedulingState, WorkReactivationMode, WorkReactivationSignal,
};
use anyhow::bail;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fmt};

#[cfg(test)]
pub(crate) const REDUCER_ONLY_CANDIDATES_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ReducerOnlyCandidates;
pub(crate) const WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::WorkItemAutonomousContinuation;
pub(crate) const EXACT_TASK_REJOIN_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExactTaskRejoin;
pub(crate) const EXACT_WAIT_RESUME_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExactWaitResume;
pub(crate) const EXPLICITLY_BOUND_OPERATOR_INPUT_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::ExplicitlyBoundOperatorInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalActivationScenario {
    WorkItemAutonomousContinuation {
        work_item_id: String,
        expected_work_item_revision: u64,
    },
    ProviderRecovery {
        work_item_id: String,
    },
    InternalFollowup {
        work_item_id: String,
    },
    ExactTaskRejoin {
        task_id: String,
        work_item_id: String,
        wait_id: Option<String>,
    },
    ExactWaitResume {
        owner: SchedulerOwner,
        wait_id: String,
    },
    LifecycleExternalNudge {
        agent_id: String,
    },
    ExplicitlyBoundOperatorInput {
        work_item_id: String,
        wait_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalActivationCandidate {
    UnboundTaskResultWaitOrReduce,
    WorkItemAutonomousContinuation {
        work_item_id: String,
        expected_work_item_revision: u64,
    },
    ProviderRecovery {
        work_item_id: String,
    },
    InternalFollowup {
        work_item_id: String,
    },
    ExactTaskRejoin {
        task_id: String,
        work_item_id: String,
    },
    ExactWaitResume {
        expected_work_item_id: Option<String>,
        correlated_wait: Option<String>,
    },
    LifecycleExternalNudge {
        agent_id: String,
    },
    ExplicitlyBoundOperatorInput {
        work_item_id: String,
    },
}

#[derive(Debug)]
pub(crate) struct AmbiguousCanonicalWaits {
    pub(crate) message_id: String,
    pub(crate) wait_condition_ids: Vec<String>,
}

impl fmt::Display for AmbiguousCanonicalWaits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "canonical activation message {} matches multiple active waits",
            self.message_id
        )
    }
}

impl std::error::Error for AmbiguousCanonicalWaits {}

impl CanonicalActivationScenario {
    pub(crate) fn work_item_id(&self) -> Option<&str> {
        match self {
            Self::WorkItemAutonomousContinuation { work_item_id, .. }
            | Self::ProviderRecovery { work_item_id }
            | Self::InternalFollowup { work_item_id }
            | Self::ExactTaskRejoin { work_item_id, .. }
            | Self::ExplicitlyBoundOperatorInput { work_item_id, .. } => Some(work_item_id),
            Self::ExactWaitResume { owner, .. } => owner.work_item_id(),
            Self::LifecycleExternalNudge { .. } => None,
        }
    }
}

impl CanonicalActivationCandidate {
    pub(crate) fn scenario_class(&self) -> SchedulerScenarioClass {
        match self {
            Self::UnboundTaskResultWaitOrReduce => EXACT_WAIT_RESUME_SCENARIO,
            Self::WorkItemAutonomousContinuation { .. }
            | Self::ProviderRecovery { .. }
            | Self::InternalFollowup { .. } => WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO,
            Self::ExactTaskRejoin { .. } => EXACT_TASK_REJOIN_SCENARIO,
            Self::ExactWaitResume { .. } | Self::LifecycleExternalNudge { .. } => {
                EXACT_WAIT_RESUME_SCENARIO
            }
            Self::ExplicitlyBoundOperatorInput { .. } => EXPLICITLY_BOUND_OPERATOR_INPUT_SCENARIO,
        }
    }

    fn expected_work_item_id(&self) -> Option<&str> {
        match self {
            Self::UnboundTaskResultWaitOrReduce => None,
            Self::WorkItemAutonomousContinuation { work_item_id, .. }
            | Self::ProviderRecovery { work_item_id }
            | Self::InternalFollowup { work_item_id }
            | Self::ExactTaskRejoin { work_item_id, .. }
            | Self::ExplicitlyBoundOperatorInput { work_item_id } => Some(work_item_id),
            Self::ExactWaitResume {
                expected_work_item_id,
                ..
            } => expected_work_item_id.as_deref(),
            Self::LifecycleExternalNudge { .. } => None,
        }
    }
}

mod diagnostics;
mod projection;

pub(crate) use diagnostics::{append_ambiguous_wait_advisory, append_scheduling_advisories};
// Preserve the pre-split scheduler facade even when the current crate has no callers.
#[cfg(test)]
pub(crate) use diagnostics::{
    scheduling_advisories, scheduling_advisories_for_facts, scheduling_advisories_with_queue_len,
};
#[allow(unused_imports)]
pub(crate) use diagnostics::{
    scheduling_advisory_event, SchedulingAdvisory, SchedulingAdvisorySeverity,
};
use projection::CanonicalWorkExecutionState;
pub(crate) use projection::{SchedulerAgentSnapshot, SchedulerProjection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulerDecisionKind {
    StartModelTurn,
    ReduceMessageOnly,
    EmitSystemTick,
    WaitForTask,
    WaitForExternalChange,
    WaitForTimer,
    WaitForOperator,
    Sleep,
    StayIdle,
    Stop,
    Noop,
}

impl SchedulerDecisionKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::StartModelTurn => "StartModelTurn",
            Self::ReduceMessageOnly => "ReduceMessageOnly",
            Self::EmitSystemTick => "EmitSystemTick",
            Self::WaitForTask => "WaitForTask",
            Self::WaitForExternalChange => "WaitForExternalChange",
            Self::WaitForTimer => "WaitForTimer",
            Self::WaitForOperator => "WaitForOperator",
            Self::Sleep => "Sleep",
            Self::StayIdle => "StayIdle",
            Self::Stop => "Stop",
            Self::Noop => "Noop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerDecision {
    pub kind: SchedulerDecisionKind,
    pub reason: String,
    pub model_reentry: bool,
    pub liveness_only: bool,
    pub message_id: Option<String>,
    pub work_item_id: Option<String>,
    pub task_id: Option<String>,
    pub boundary: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerBoundary {
    RunLoop,
    RunLoopIdle,
    LifecycleSleep,
    MessageProcessing,
    IdleTick,
}

impl SchedulerBoundary {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RunLoop => "run_loop",
            Self::RunLoopIdle => "run_loop_idle",
            Self::LifecycleSleep => "lifecycle_sleep",
            Self::MessageProcessing => "message_processing",
            Self::IdleTick => "idle_tick",
        }
    }
}

/// Typed boundary for operator interjection drainage within a turn.
/// Replaces the previous single string-labeled drain path so each boundary
/// gets its own shadow comparison facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterjectionBoundary {
    AfterProviderRound,
    BeforeToolExecution,
    AfterToolResults,
    BeforeProviderContinuation,
}

impl InterjectionBoundary {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AfterProviderRound => "after_provider_round",
            Self::BeforeToolExecution => "before_tool_execution",
            Self::AfterToolResults => "after_tool_results",
            Self::BeforeProviderContinuation => "before_provider_continuation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulerDuplicateEvidence {
    ContinueActiveBrief(String),
    QueuedAvailableMessage(String),
    WakeHintMessage(String),
}

#[derive(Debug, Clone)]
pub(crate) enum SchedulerIdleSignal<'a> {
    ContinueActive {
        work_item: &'a WorkItemRecord,
        suppressed_after_model_reentry_continuation: bool,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
    QueuedAvailable {
        work_item: &'a WorkItemRecord,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
    WakeHint {
        pending: &'a PendingWakeHint,
        duplicate: Option<SchedulerDuplicateEvidence>,
    },
}

pub(crate) enum SchedulerInput<'a> {
    Idle,
    Message {
        message: &'a MessageEnvelope,
        model_turn_allowed: bool,
        continuation_resolution: Option<&'a ContinuationResolution>,
    },
    IdleSignal(SchedulerIdleSignal<'a>),
}

impl SchedulerDecision {
    pub(crate) fn new(kind: SchedulerDecisionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
            model_reentry: false,
            liveness_only: false,
            message_id: None,
            work_item_id: None,
            task_id: None,
            boundary: None,
            evidence: Vec::new(),
        }
    }

    pub(crate) fn model_reentry(mut self, value: bool) -> Self {
        self.model_reentry = value;
        self
    }

    pub(crate) fn liveness_only(mut self, value: bool) -> Self {
        self.liveness_only = value;
        self
    }

    pub(crate) fn message(mut self, message: &MessageEnvelope) -> Self {
        self.message_id = Some(message.id.clone());
        self.work_item_id = message.work_item_id.clone();
        self.task_id = message.task_id.clone();
        self
    }

    pub(crate) fn work_item_id(mut self, work_item_id: impl Into<String>) -> Self {
        self.work_item_id = Some(work_item_id.into());
        self
    }
    pub(crate) fn boundary(mut self, boundary: impl Into<String>) -> Self {
        self.boundary = Some(boundary.into());
        self
    }

    pub(crate) fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }
}

pub(crate) fn decide_next_action(
    projection: &SchedulerProjection,
    boundary: SchedulerBoundary,
    input: SchedulerInput<'_>,
) -> SchedulerDecision {
    let boundary_label = boundary.as_str();
    if matches!(projection.status, AgentStatus::Stopped) {
        return SchedulerDecision::new(SchedulerDecisionKind::Stop, "stopped")
            .boundary(boundary_label)
            .liveness_only(true)
            .evidence(format!("status={:?}", projection.status));
    }

    match input {
        SchedulerInput::Message {
            message,
            model_turn_allowed,
            continuation_resolution,
        } => {
            let matching_wait_work_item_id = matching_wait_conditions(projection, message)
                .into_iter()
                .filter_map(|condition| condition.work_item_id.clone())
                .next();
            let mut decision =
                message_processing_decision(message, model_turn_allowed, continuation_resolution)
                    .boundary(boundary_label)
                    .evidence(format!("queue_len={}", projection.queue_len))
                    .evidence(format!("turn_in_progress={}", projection.turn_in_progress));
            if decision.work_item_id.is_none() {
                decision.work_item_id = matching_wait_work_item_id;
            }
            decision
        }
        SchedulerInput::IdleSignal(signal) => {
            decide_idle_signal_action(projection, boundary_label, signal)
        }
        SchedulerInput::Idle => idle_boundary_decision(projection, boundary_label),
    }
}

fn decide_idle_signal_action(
    projection: &SchedulerProjection,
    boundary: &'static str,
    signal: SchedulerIdleSignal<'_>,
) -> SchedulerDecision {
    if projection.turn_in_progress {
        return SchedulerDecision::new(SchedulerDecisionKind::Noop, "turn_in_progress")
            .boundary(boundary)
            .liveness_only(true)
            .evidence(format!("active_run_id={:?}", projection.active_run_id));
    }

    match signal {
        SchedulerIdleSignal::WakeHint { pending, duplicate } => {
            if let Some(SchedulerDuplicateEvidence::WakeHintMessage(message_id)) = duplicate {
                return SchedulerDecision::new(SchedulerDecisionKind::Noop, "duplicate_wake_hint")
                    .boundary(boundary)
                    .liveness_only(true)
                    .evidence("duplicate_wake_hint_suppressed")
                    .evidence(format!("message_id={message_id}"))
                    .evidence(format!(
                        "idempotency_key={}",
                        wake_hint_idempotency_key(pending)
                    ));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "wake_hint")
                .boundary(boundary)
                .model_reentry(true)
                .evidence("runtime_idle")
                .evidence("pending_wake_hint")
                .evidence(format!(
                    "idempotency_key={}",
                    wake_hint_idempotency_key(pending)
                ))
        }
        SchedulerIdleSignal::ContinueActive {
            work_item,
            suppressed_after_model_reentry_continuation,
            duplicate,
        } => {
            if let Some(decision) = wait_decision_for_projection(projection) {
                return decision
                    .boundary(boundary)
                    .evidence("work_queue_tick_blocked_by_wait_fact");
            }
            if suppressed_after_model_reentry_continuation {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "continue_active_suppressed_after_model_reentry_continuation",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("model_reentry_continuation_suppresses_duplicate_continue_active");
            }
            if let Some(SchedulerDuplicateEvidence::ContinueActiveBrief(result_brief_id)) =
                duplicate
            {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "duplicate_continue_active",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("duplicate_tick_suppressed")
                .evidence(format!("result_brief_id={result_brief_id}"));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "continue_active")
                .boundary(boundary)
                .model_reentry(true)
                .work_item_id(work_item.id.clone())
                .evidence("runtime_idle")
                .evidence("work_item_runnable")
                .evidence(format!(
                    "idempotency_key={}",
                    work_queue_tick_idempotency_key(work_item, "continue_active")
                ))
        }
        SchedulerIdleSignal::QueuedAvailable {
            work_item,
            duplicate,
        } => {
            if let Some(decision) = wait_decision_for_projection(projection) {
                return decision
                    .boundary(boundary)
                    .evidence("work_queue_tick_blocked_by_wait_fact");
            }
            if let Some(SchedulerDuplicateEvidence::QueuedAvailableMessage(message_id)) = duplicate
            {
                return SchedulerDecision::new(
                    SchedulerDecisionKind::Noop,
                    "duplicate_queued_available",
                )
                .boundary(boundary)
                .liveness_only(true)
                .work_item_id(work_item.id.clone())
                .evidence("duplicate_tick_suppressed")
                .evidence(format!("message_id={message_id}"));
            }
            SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "queued_available")
                .boundary(boundary)
                .model_reentry(true)
                .work_item_id(work_item.id.clone())
                .evidence("runtime_idle")
                .evidence("work_item_runnable")
                .evidence(format!(
                    "idempotency_key={}",
                    work_queue_tick_idempotency_key(work_item, "queued_available")
                ))
        }
    }
}

pub(crate) fn scheduler_decision_event(decision: &SchedulerDecision) -> AuditEvent {
    AuditEvent::legacy(
        "scheduler_decision",
        serde_json::json!({
            "decision": decision.kind.as_str(),
            "reason": &decision.reason,
            "model_reentry": decision.model_reentry,
            "liveness_only": decision.liveness_only,
            "message_id": &decision.message_id,
            "work_item_id": &decision.work_item_id,
            "task_id": &decision.task_id,
            "boundary": &decision.boundary,
            "evidence": &decision.evidence,
        }),
    )
}

pub(crate) fn scheduler_diagnostic_event(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<AuditEvent> {
    let payload = scheduler_diagnostic_audit_event(agent_id, decision);
    AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic,
        &payload,
    )
}

pub(crate) fn scheduler_invariant_diagnostic_event(
    agent_id: &str,
    code: &str,
    boundary: &'static str,
    work_item_id: Option<String>,
    message_id: Option<String>,
    evidence: Vec<String>,
) -> Result<AuditEvent> {
    AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic,
        &crate::types::SchedulerDiagnosticAuditEvent {
            agent_id: agent_id.to_string(),
            decision: "InvariantViolation".into(),
            reason: code.to_string(),
            boundary: Some(boundary.into()),
            scenario_class: None,
            work_item_id,
            message_id,
            task_id: None,
            evidence,
        },
    )
}

pub(crate) fn scheduler_decision_events(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<[AuditEvent; 2]> {
    Ok([
        scheduler_diagnostic_event(agent_id, decision)?,
        scheduler_decision_event(decision),
    ])
}

pub(crate) fn append_scheduler_decision(
    storage: &AppStorage,
    agent_id: &str,
    decision: &SchedulerDecision,
) -> Result<bool> {
    let events = scheduler_decision_events(agent_id, decision)?;
    let legacy_event = &events[1];
    let recent_events = storage.read_recent_events(32)?;
    if recent_scheduler_decision_is_duplicate(&recent_events, legacy_event) {
        return Ok(false);
    }
    storage.append_events(&events)?;
    Ok(true)
}

fn recent_scheduler_decision_is_duplicate(
    recent_events: &[AuditEvent],
    legacy_event: &AuditEvent,
) -> bool {
    let signature = scheduler_decision_signature(&legacy_event.data);
    // Suppress only when the most recent same-signature occurrence in the
    // window has no model-reentry decision after it: idle boundary alternation
    // still dedupes, while a genuine work -> idle revert is recorded so the
    // latest recorded decision keeps mirroring the current posture.
    let mut duplicate = false;
    let mut model_reentry_since_last_match = false;
    for event in recent_events {
        if event.kind != legacy_event.kind {
            continue;
        }
        if scheduler_decision_signature(&event.data) == signature {
            duplicate = true;
            model_reentry_since_last_match = false;
        } else if scheduler_decision_model_reentry(&event.data) {
            model_reentry_since_last_match = true;
        }
    }
    duplicate && !model_reentry_since_last_match
}

fn scheduler_decision_model_reentry(data: &serde_json::Value) -> bool {
    data.get("model_reentry")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

type SchedulerDecisionSignature<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    bool,
    bool,
);

/// Stable identity of a scheduler decision for duplicate suppression.
///
/// Idle run loops alternate boundaries (`run_loop_idle` / `idle_tick`) with the
/// same wait decision, so comparing only the latest same-kind event never
/// matches: each decision must be compared against its own most recent
/// occurrence. Volatile evidence (per-tick idempotency keys, active counts) is
/// excluded so an unchanged scheduler state is recorded once. The
/// `model_reentry` / `liveness_only` posture flags are part of the identity so
/// a flag flip can never be merged away.
fn scheduler_decision_signature(data: &serde_json::Value) -> SchedulerDecisionSignature<'_> {
    fn field<'a>(data: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        data.get(key).and_then(serde_json::Value::as_str)
    }
    (
        field(data, "decision"),
        field(data, "reason"),
        field(data, "boundary"),
        field(data, "message_id"),
        field(data, "work_item_id"),
        field(data, "task_id"),
        scheduler_decision_model_reentry(data),
        data.get("liveness_only")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
    )
}

pub(crate) fn scheduler_diagnostic_audit_event(
    agent_id: &str,
    decision: &SchedulerDecision,
) -> crate::types::SchedulerDiagnosticAuditEvent {
    crate::types::SchedulerDiagnosticAuditEvent {
        agent_id: agent_id.to_string(),
        decision: decision.kind.as_str().to_string(),
        reason: decision.reason.clone(),
        boundary: decision.boundary.clone(),
        scenario_class: None,
        work_item_id: decision.work_item_id.clone(),
        message_id: decision.message_id.clone(),
        task_id: decision.task_id.clone(),
        evidence: decision.evidence.clone(),
    }
}

pub(crate) fn message_processing_decision(
    message: &MessageEnvelope,
    model_turn_allowed: bool,
    continuation_resolution: Option<&ContinuationResolution>,
) -> SchedulerDecision {
    let model_reentry = model_turn_allowed
        && continuation_resolution.is_some_and(|resolution| resolution.model_reentry);
    let kind = if model_reentry {
        SchedulerDecisionKind::StartModelTurn
    } else {
        SchedulerDecisionKind::ReduceMessageOnly
    };
    let mut decision = SchedulerDecision::new(kind, format!("{:?}", message.kind))
        .message(message)
        .model_reentry(model_reentry)
        .liveness_only(!model_reentry)
        .evidence(format!("message_kind={:?}", message.kind))
        .evidence(format!("trigger_kind={:?}", message.trigger_kind));
    if !model_turn_allowed {
        decision = decision.evidence("model_turn_blocked_by_control_posture");
    }
    decision
}

#[cfg(test)]
pub(crate) fn authority_scenarios_for_message_claim(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    continuation_resolution: Option<&ContinuationResolution>,
) -> Vec<SchedulerScenarioClass> {
    let mut scenarios = Vec::with_capacity(1);
    if message_admission_scenario_applies(message, continuation_resolution) {
        scenarios.push(REDUCER_ONLY_CANDIDATES_SCENARIO);
    }
    if wait_resume_scenario_applies(projection, message) {
        scenarios.push(
            wait_resume_scenario_class(message)
                .expect("applicable wait resume has a registered scenario class"),
        );
    }
    scenarios
}

pub(crate) fn canonical_activation_candidate(
    message: &MessageEnvelope,
    _continuation_resolution: Option<&ContinuationResolution>,
    task: Option<&TaskRecord>,
) -> Result<Option<CanonicalActivationCandidate>> {
    if matches!(
        (&message.kind, &message.origin),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem })
            if subsystem == "work_queue"
    ) {
        let metadata = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("work_queue"));
        let metadata_work_item_id = metadata
            .and_then(|metadata| metadata.get("work_item_id"))
            .and_then(serde_json::Value::as_str);
        let expected_work_item_revision = metadata
            .and_then(|metadata| metadata.get("work_item_revision"))
            .and_then(serde_json::Value::as_u64)
            .filter(|revision| *revision > 0);
        let reason = metadata
            .and_then(|metadata| metadata.get("reason"))
            .and_then(serde_json::Value::as_str);
        return Ok(
            match (
                message.work_item_id.as_deref(),
                metadata_work_item_id,
                expected_work_item_revision,
                reason,
            ) {
                (
                    Some(bound_work_item_id),
                    Some(metadata_work_item_id),
                    Some(expected_work_item_revision),
                    Some("continue_active" | "queued_available"),
                ) if bound_work_item_id == metadata_work_item_id => Some(
                    CanonicalActivationCandidate::WorkItemAutonomousContinuation {
                        work_item_id: bound_work_item_id.to_string(),
                        expected_work_item_revision,
                    },
                ),
                _ => None,
            },
        );
    }
    if message.kind == MessageKind::InternalFollowup {
        if let Some(work_item_id) = message.work_item_id.clone() {
            if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
                return Ok(Some(CanonicalActivationCandidate::ProviderRecovery {
                    work_item_id,
                }));
            }
        }
        return Ok(if let Some(work_item_id) = message.work_item_id.clone() {
            Some(CanonicalActivationCandidate::InternalFollowup { work_item_id })
        } else if runtime_owned_internal_followup(message) {
            Some(CanonicalActivationCandidate::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            })
        } else {
            None
        });
    }
    if message.kind == MessageKind::TaskResult {
        let MessageOrigin::Task { task_id } = &message.origin else {
            bail!("canonical task rejoin requires task message origin");
        };
        if message.task_id.as_deref() != Some(task_id.as_str()) {
            bail!("canonical task rejoin has inconsistent task identity");
        }
        let task = task.ok_or_else(|| anyhow!("canonical task rejoin is missing task record"))?;
        if task.id != *task_id || task.agent_id != message.agent_id {
            bail!("canonical task rejoin requires a same-agent task identity");
        }
        if !matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            return Ok(None);
        }
        if let Some(work_item_id) = task.effective_work_item_id() {
            if message.work_item_id.as_deref() != Some(work_item_id) {
                bail!("canonical task rejoin has inconsistent WorkItem binding");
            }
            return Ok(Some(CanonicalActivationCandidate::ExactTaskRejoin {
                task_id: task_id.clone(),
                work_item_id: work_item_id.to_string(),
            }));
        }
        if task.terminal_reentry() {
            return Ok(Some(CanonicalActivationCandidate::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            }));
        }
        return Ok(Some(
            CanonicalActivationCandidate::UnboundTaskResultWaitOrReduce,
        ));
    }

    if message.kind == MessageKind::OperatorPrompt {
        let operator_ingress =
            trusted_operator_prompt(message) || authenticated_operator_ingress(message);
        if operator_ingress && message.work_item_id.is_some() {
            let work_item_id = message
                .work_item_id
                .clone()
                .ok_or_else(|| anyhow!("explicit operator input requires a WorkItem binding"))?;
            return Ok(Some(
                CanonicalActivationCandidate::ExplicitlyBoundOperatorInput { work_item_id },
            ));
        }
        if operator_ingress {
            return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
                expected_work_item_id: None,
                correlated_wait: None,
            }));
        }
        return Ok(None);
    }

    if matches!(
        (&message.kind, &message.origin),
        (
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent,
            _
        ) | (MessageKind::SystemTick, MessageOrigin::System { .. })
    ) {
        if let Some(correlated_wait) = authoritative_wait_correlation(message) {
            return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
                expected_work_item_id: message.work_item_id.clone(),
                correlated_wait: Some(correlated_wait),
            }));
        }
        return Ok(Some(CanonicalActivationCandidate::LifecycleExternalNudge {
            agent_id: message.agent_id.clone(),
        }));
    }

    if matches!(
        (&message.kind, &message.origin),
        (MessageKind::TimerTick, MessageOrigin::Timer { .. })
    ) {
        return Ok(Some(CanonicalActivationCandidate::ExactWaitResume {
            expected_work_item_id: message.work_item_id.clone(),
            correlated_wait: None,
        }));
    }

    Ok(None)
}

fn trusted_operator_prompt(message: &MessageEnvelope) -> bool {
    message.authority_class == AuthorityClass::OperatorInstruction
        && matches!(message.origin, MessageOrigin::Operator { .. })
}

pub(crate) fn runtime_owned_internal_followup(message: &MessageEnvelope) -> bool {
    message.kind == MessageKind::InternalFollowup
        && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && message.admission_context == Some(AdmissionContext::RuntimeOwned)
        && matches!(
            message.origin,
            MessageOrigin::System { .. } | MessageOrigin::Task { .. }
        )
}

pub(crate) fn resolve_canonical_activation_scenario(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
    candidate: CanonicalActivationCandidate,
) -> Result<Option<CanonicalActivationScenario>> {
    if let CanonicalActivationCandidate::WorkItemAutonomousContinuation {
        work_item_id,
        expected_work_item_revision,
    } = candidate
    {
        return Ok(Some(
            CanonicalActivationScenario::WorkItemAutonomousContinuation {
                work_item_id,
                expected_work_item_revision,
            },
        ));
    }
    if let CanonicalActivationCandidate::ProviderRecovery { work_item_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::ProviderRecovery {
            work_item_id,
        }));
    }
    if let CanonicalActivationCandidate::InternalFollowup { work_item_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::InternalFollowup {
            work_item_id,
        }));
    }
    if let CanonicalActivationCandidate::LifecycleExternalNudge { agent_id } = candidate {
        return Ok(Some(CanonicalActivationScenario::LifecycleExternalNudge {
            agent_id,
        }));
    }

    let mut matching_waits = match &candidate {
        CanonicalActivationCandidate::ExactWaitResume {
            correlated_wait: Some(wait_id),
            ..
        } => projection
            .activation_waits
            .iter()
            .filter(|condition| {
                condition.id == *wait_id
                    && (condition.status == WaitConditionStatus::Active
                        || (condition.status == WaitConditionStatus::Triggered
                            && condition.trigger_message_id() == Some(message.id.as_str())))
                    && condition.work_item_id.as_deref() == candidate.expected_work_item_id()
            })
            .collect(),
        _ => {
            let mut waits = matching_wait_conditions_for_work_item(
                projection,
                message,
                candidate.expected_work_item_id(),
            );
            if message.work_item_id.is_none()
                && matches!(
                    message.kind,
                    MessageKind::OperatorPrompt | MessageKind::TaskResult
                )
            {
                waits.retain(|wait| {
                    wait.work_item_id.is_none()
                        || (message.kind == MessageKind::OperatorPrompt
                            && message.priority != Priority::Interject
                            && wait.kind == WaitConditionKind::Operator)
                });
            }
            waits
        }
    };
    if matching_waits.len() > 1
        && matches!(
            candidate,
            CanonicalActivationCandidate::ExactTaskRejoin { .. }
        )
        && projection.canonical_work_states.is_none()
    {
        // The durable task rejoin fence is authoritative. Before the canonical
        // scheduler partition exists, duplicate legacy wait rows are mirrors
        // and must not make the exact task identity ambiguous.
        matching_waits.clear();
    } else if matching_waits.len() > 1 {
        return Err(anyhow::Error::new(AmbiguousCanonicalWaits {
            message_id: message.id.clone(),
            wait_condition_ids: matching_waits.iter().map(|wait| wait.id.clone()).collect(),
        }));
    }
    let matching_wait = matching_waits.first().copied();

    if let CanonicalActivationCandidate::ExactTaskRejoin {
        task_id,
        work_item_id,
    } = candidate
    {
        if matching_wait.is_none() {
            match projection
                .canonical_work_states
                .as_ref()
                .and_then(|states| states.get(&work_item_id))
            {
                Some(
                    CanonicalWorkExecutionState::Runnable { .. }
                    | CanonicalWorkExecutionState::Other,
                ) => {}
                Some(CanonicalWorkExecutionState::Waiting { .. }) => {
                    return Ok(None);
                }
                None if projection.canonical_work_states.is_some() => return Ok(None),
                None => {}
            }
        }
        return Ok(Some(CanonicalActivationScenario::ExactTaskRejoin {
            task_id,
            work_item_id,
            wait_id: matching_wait.map(|wait| wait.id.clone()),
        }));
    }

    if let CanonicalActivationCandidate::ExplicitlyBoundOperatorInput { work_item_id } = candidate {
        return Ok(Some(
            CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                work_item_id,
                wait_id: matching_wait.map(|wait| wait.id.clone()),
            },
        ));
    }

    let Some(wait) = matching_wait else {
        if matches!(
            message.kind,
            MessageKind::OperatorPrompt | MessageKind::TimerTick
        ) {
            return Ok(Some(CanonicalActivationScenario::LifecycleExternalNudge {
                agent_id: message.agent_id.clone(),
            }));
        }
        return Ok(None);
    };
    Ok(Some(CanonicalActivationScenario::ExactWaitResume {
        owner: wait
            .work_item_id
            .clone()
            .map(|work_item_id| SchedulerOwner::WorkItem { work_item_id })
            .unwrap_or_else(|| SchedulerOwner::AgentLifecycle {
                agent_id: wait.agent_id.clone(),
            }),
        wait_id: wait.id.clone(),
    }))
}

fn authoritative_wait_correlation(message: &MessageEnvelope) -> Option<String> {
    let trusted = matches!(
        (message.delivery_surface, message.admission_context),
        (
            Some(MessageDeliverySurface::RuntimeSystem),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::TaskRejoin),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::HttpCallbackWake),
            Some(AdmissionContext::ExternalTriggerCapability)
        )
    ) && matches!(
        message.authority_class,
        AuthorityClass::RuntimeInstruction | AuthorityClass::IntegrationSignal
    );
    if !trusted {
        return None;
    }
    message.source_refs.get("wait_id").cloned()
}

fn matching_wait_conditions<'a>(
    projection: &'a SchedulerProjection,
    message: &MessageEnvelope,
) -> Vec<&'a WaitConditionRecord> {
    matching_wait_conditions_for_work_item(projection, message, None)
}

fn matching_wait_conditions_for_work_item<'a>(
    projection: &'a SchedulerProjection,
    message: &MessageEnvelope,
    expected_work_item_id: Option<&str>,
) -> Vec<&'a WaitConditionRecord> {
    projection
        .activation_waits
        .iter()
        .filter(|condition| {
            expected_work_item_id
                .is_none_or(|work_item_id| condition.work_item_id.as_deref() == Some(work_item_id))
                && (condition.status == WaitConditionStatus::Active
                    || (condition.status == WaitConditionStatus::Triggered
                        && condition.trigger_message_id() == Some(message.id.as_str()))
                    || (message.kind == MessageKind::TaskResult
                        && condition.status == WaitConditionStatus::Resolved
                        && condition.kind == WaitConditionKind::Task
                        && condition.work_item_id == message.work_item_id
                        && condition.trigger_message_id() == Some(message.id.as_str())
                        && resolved_task_wait_is_current(projection, condition)))
                && message_matches_wait_condition(message, condition)
                && (!(message.kind == MessageKind::TaskResult
                    && condition.kind == WaitConditionKind::Task)
                    || resolved_task_wait_is_current(projection, condition))
        })
        .collect()
}

fn resolved_task_wait_is_current(
    projection: &SchedulerProjection,
    condition: &WaitConditionRecord,
) -> bool {
    let Some(states) = &projection.canonical_work_states else {
        return true;
    };
    let Some(work_item_id) = condition.work_item_id.as_deref() else {
        return condition.trigger_message_id().is_some();
    };
    matches!(
        states.get(work_item_id),
        Some(CanonicalWorkExecutionState::Waiting { wait_id }) if wait_id == &condition.id
    )
}

pub(crate) fn authenticated_operator_ingress(message: &MessageEnvelope) -> bool {
    message
        .message_seq
        .is_some_and(|message_seq| message_seq > 0)
        && matches!(message.origin, MessageOrigin::Operator { .. })
        && matches!(
            (message.delivery_surface, message.admission_context),
            (
                Some(MessageDeliverySurface::CliPrompt | MessageDeliverySurface::RunOnce),
                Some(AdmissionContext::LocalProcess)
            ) | (
                Some(MessageDeliverySurface::HttpControlPrompt),
                Some(AdmissionContext::ControlAuthenticated)
            ) | (
                Some(MessageDeliverySurface::RemoteOperatorTransport),
                Some(AdmissionContext::OperatorTransportAuthenticated)
            )
        )
}

#[cfg(test)]
fn message_admission_scenario_applies(
    message: &MessageEnvelope,
    continuation_resolution: Option<&ContinuationResolution>,
) -> bool {
    matches!(
        continuation_resolution.map(|resolution| resolution.class),
        None | Some(
            crate::types::ContinuationClass::LocalContinuation
                | crate::types::ContinuationClass::LivenessOnly
        )
    ) && !matches!(
        message.kind,
        MessageKind::OperatorPrompt | MessageKind::TaskResult | MessageKind::SystemTick
    )
}

#[cfg(test)]
fn wait_resume_scenario_class(message: &MessageEnvelope) -> Option<SchedulerScenarioClass> {
    match message.kind {
        MessageKind::TaskResult => Some(EXACT_TASK_REJOIN_SCENARIO),
        MessageKind::CallbackEvent
        | MessageKind::WebhookEvent
        | MessageKind::ChannelEvent
        | MessageKind::TimerTick
        | MessageKind::SystemTick => Some(EXACT_WAIT_RESUME_SCENARIO),
        _ => None,
    }
}

#[cfg(test)]
fn wait_resume_scenario_applies(
    projection: &SchedulerProjection,
    message: &MessageEnvelope,
) -> bool {
    matches!(
        message.kind,
        MessageKind::TaskResult
            | MessageKind::CallbackEvent
            | MessageKind::WebhookEvent
            | MessageKind::ChannelEvent
            | MessageKind::TimerTick
            | MessageKind::SystemTick
    ) && !matching_wait_conditions(projection, message).is_empty()
}

pub(super) fn message_matches_wait_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> bool {
    if matches!(
        (&message.kind, &message.origin),
        (
            MessageKind::SystemTick,
            MessageOrigin::System { subsystem }
        ) if subsystem == "wait_condition_recheck"
    ) && message.authority_class == AuthorityClass::RuntimeInstruction
        && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && message.admission_context == Some(AdmissionContext::RuntimeOwned)
        && condition.work_item_id.is_none()
        && message.source_refs.get("wait_id") == Some(&condition.id)
    {
        return true;
    }
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => {
            condition.wake_sources.iter().any(
                |source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id),
            )
        }
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput)),
        (MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            condition.wake_sources.iter().any(|source| {
                matches!(
                    source,
                    WakeSource::ExternalIngress {
                        external_trigger_id: expected,
                    } if expected.as_ref().is_none_or(|expected| {
                        external_trigger_id.is_some_and(|actual| actual == expected)
                    })
                )
            })
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => {
            condition
                .subject_ref
                .as_deref()
                .is_none_or(|subject_ref| subject_ref == timer_id)
                && condition.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::Timer { .. })
                        && message
                            .source_refs
                            .get("timer_id")
                            .is_none_or(|source_timer_id| source_timer_id == timer_id)
                })
        }
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if subsystem == "work_queue" {
                return false;
            }
            if let Some(external_trigger_id) = message.source_refs.get("external_trigger_id") {
                return condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::ExternalIngress {
                            external_trigger_id: expected,
                        } if expected.as_ref().is_none_or(|expected| {
                            expected == external_trigger_id
                        })
                    )
                });
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
        }
        _ => false,
    }
}

pub(crate) fn idle_noop_decision(projection: &SchedulerProjection) -> SchedulerDecision {
    let (kind, reason) = if matches!(projection.status, AgentStatus::Stopped) {
        (SchedulerDecisionKind::Stop, "stopped")
    } else if projection.queue_len > 0 {
        (SchedulerDecisionKind::Noop, "queue_not_empty")
    } else if projection.turn_in_progress {
        (SchedulerDecisionKind::Noop, "turn_in_progress")
    } else if matches!(projection.status, AgentStatus::Asleep) {
        (SchedulerDecisionKind::StayIdle, "already_asleep")
    } else {
        (SchedulerDecisionKind::Sleep, "no_pending_scheduler_facts")
    };
    SchedulerDecision::new(kind, reason)
        .liveness_only(true)
        .evidence(format!("status={:?}", projection.status))
        .evidence(format!("queue_len={}", projection.queue_len))
}

pub(crate) fn wait_decision_for_projection(
    projection: &SchedulerProjection,
) -> Option<SchedulerDecision> {
    if projection.has_interrupted_replay {
        return None;
    }
    if projection.work_reactivation_signal().is_some() {
        return None;
    }
    if projection.active_agent_waiting_intents > 0 {
        return Some(
            SchedulerDecision::new(
                SchedulerDecisionKind::WaitForExternalChange,
                "active_agent_waiting_intents",
            )
            .liveness_only(true)
            .evidence(format!(
                "active_waiting_intents={}",
                projection.active_waiting_intents
            ))
            .evidence(format!(
                "active_agent_waiting_intents={}",
                projection.active_agent_waiting_intents
            )),
        );
    }
    if projection.active_timers > 0 {
        return Some(
            SchedulerDecision::new(SchedulerDecisionKind::WaitForTimer, "active_timers")
                .liveness_only(true)
                .evidence(format!("active_timers={}", projection.active_timers)),
        );
    }
    projection.waiting_work_item.as_ref().and_then(|item| {
        match projection.waiting_work_item_scheduling_state {
            Some(WorkItemSchedulingState::WaitingOperator) => {
                // If recheck_at has expired and not been consumed, do not block on
                // WaitForOperator — let the agent wake up to re-evaluate. This
                // prevents permanent stalls when wake=operator_input is used with
                // a recheck_after_ms fallback. (#1989)
                if item
                    .recheck_at
                    .is_some_and(|recheck_at| recheck_at <= projection.now)
                    && item
                        .recheck_consumed_at
                        .zip(item.recheck_at)
                        .is_none_or(|(consumed, recheck_at)| consumed < recheck_at)
                {
                    return None;
                }
                Some(
                    SchedulerDecision::new(
                        SchedulerDecisionKind::WaitForOperator,
                        "work_item_needs_input",
                    )
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingOperator"),
                )
            }
            Some(WorkItemSchedulingState::WaitingTask) => Some(
                SchedulerDecision::new(SchedulerDecisionKind::WaitForTask, "work_item_task_wait")
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingTask"),
            ),
            Some(WorkItemSchedulingState::WaitingExternal) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::WaitForExternalChange,
                    "work_item_external_wait",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("work_item_scheduling_state=WaitingExternal"),
            ),
            Some(WorkItemSchedulingState::WaitingTimer) => Some(
                SchedulerDecision::new(SchedulerDecisionKind::WaitForTimer, "work_item_timer_wait")
                    .liveness_only(true)
                    .work_item_id(item.id.clone())
                    .evidence("work_item_scheduling_state=WaitingTimer"),
            ),
            Some(WorkItemSchedulingState::WaitingSystem) => Some(
                SchedulerDecision::new(
                    SchedulerDecisionKind::EmitSystemTick,
                    "work_item_system_wait",
                )
                .liveness_only(true)
                .work_item_id(item.id.clone())
                .evidence("work_item_scheduling_state=WaitingSystem"),
            ),
            _ => None,
        }
    })
}

pub(crate) fn idle_boundary_decision(
    projection: &SchedulerProjection,
    boundary: impl Into<String>,
) -> SchedulerDecision {
    let boundary = boundary.into();
    if matches!(projection.status, AgentStatus::Stopped) {
        return idle_noop_decision(projection).boundary(boundary);
    }
    if let Some(decision) = wait_decision_for_projection(projection) {
        return decision.boundary(boundary);
    }
    if let Some(signal) = projection.work_reactivation_signal() {
        return SchedulerDecision::new(SchedulerDecisionKind::EmitSystemTick, "runnable_work")
            .boundary(boundary)
            .model_reentry(true)
            .work_item_id(signal.work_item_id)
            .evidence("runtime_idle")
            .evidence("work_item_runnable");
    }
    idle_noop_decision(projection).boundary(boundary)
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(crate) fn projected_status_for_idle(
    state: &AgentState,
    _storage: &AppStorage,
) -> Result<AgentStatus> {
    if matches!(state.status, AgentStatus::Asleep | AgentStatus::Stopped) {
        return Ok(state.status.clone());
    }
    Ok(AgentStatus::AwakeIdle)
}

pub(crate) fn apply_idle_projection(state: &mut AgentState, storage: &AppStorage) -> Result<()> {
    state.status = projected_status_for_idle(state, storage)?;
    state.current_run_id = None;
    Ok(())
}

pub(crate) fn apply_running_projection(state: &mut AgentState, run_id: String) {
    state.status = AgentStatus::AwakeRunning;
    state.current_run_id = Some(run_id);
}

pub(crate) fn apply_message_wake_projection(state: &mut AgentState) -> bool {
    if matches!(state.status, AgentStatus::Asleep | AgentStatus::Booting) {
        state.status = AgentStatus::AwakeIdle;
        state.sleeping_until = None;
        return true;
    }
    false
}

pub(crate) fn apply_start_projection(state: &mut AgentState) {
    state.status = AgentStatus::AwakeIdle;
    state.current_run_id = None;
}

pub(crate) fn apply_stop_projection(state: &mut AgentState) {
    state.status = AgentStatus::Stopped;
    state.current_run_id = None;
    state.sleeping_until = None;
    state.pending_wake_hint = None;
}

pub(crate) fn apply_sleep_projection(
    state: &mut AgentState,
    sleeping_until: Option<DateTime<Utc>>,
) {
    state.status = AgentStatus::Asleep;
    state.current_run_id = None;
    state.sleeping_until = sleeping_until;
}

pub(crate) fn is_operator_interjection_message(message: &MessageEnvelope) -> bool {
    matches!(
        (
            &message.kind,
            &message.origin,
            &message.authority_class,
            &message.priority,
        ),
        (
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { .. },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
        )
    )
}

pub(crate) fn work_queue_tick_idempotency_key(work_item: &WorkItemRecord, reason: &str) -> String {
    format!(
        "work_queue:{}:{}:{}",
        reason, work_item.id, work_item.revision
    )
}

pub(crate) fn wake_hint_idempotency_key(pending: &PendingWakeHint) -> String {
    let scope = pending
        .external_trigger_id
        .as_deref()
        .or(pending.source.as_deref())
        .unwrap_or("unknown");
    format!(
        "wake_hint:{}:{}",
        scope,
        pending.created_at.timestamp_micros()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AdmissionContext, AgentState, MessageBody, MessageDeliverySurface};

    // --- apply_start_projection ---

    #[test]
    fn apply_start_sets_awake_idle_and_clears_run() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Stopped;
        state.current_run_id = Some("stale".into());
        apply_start_projection(&mut state);
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    // --- apply_stop_projection ---

    #[test]
    fn apply_stop_clears_all_runtime_state() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        state.current_run_id = Some("run-1".into());
        state.sleeping_until = Some(Utc::now());
        state.pending_wake_hint = Some(PendingWakeHint {
            reason: "test".into(),
            description: None,
            source: None,
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        apply_stop_projection(&mut state);
        assert_eq!(state.status, AgentStatus::Stopped);
        assert_eq!(state.current_run_id, None);
        assert_eq!(state.sleeping_until, None);
        assert_eq!(state.pending_wake_hint, None);
    }

    // --- apply_sleep_projection ---

    #[test]
    fn apply_sleep_sets_status_and_clears_run() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        state.current_run_id = Some("run-1".into());
        let until = Utc::now() + chrono::Duration::hours(1);
        apply_sleep_projection(&mut state, Some(until));
        assert_eq!(state.status, AgentStatus::Asleep);
        assert_eq!(state.current_run_id, None);
        assert_eq!(state.sleeping_until, Some(until));
    }

    #[test]
    fn apply_sleep_indefinite_clears_sleeping_until() {
        let mut state = AgentState::new("test");
        state.sleeping_until = Some(Utc::now());
        apply_sleep_projection(&mut state, None);
        assert_eq!(state.status, AgentStatus::Asleep);
        assert_eq!(state.sleeping_until, None);
    }

    // --- apply_running_projection ---

    #[test]
    fn apply_running_sets_awake_running_with_run_id() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeIdle;
        apply_running_projection(&mut state, "run-42".into());
        assert_eq!(state.status, AgentStatus::AwakeRunning);
        assert_eq!(state.current_run_id.as_deref(), Some("run-42"));
    }

    // --- apply_message_wake_projection ---

    #[test]
    fn apply_message_wake_from_asleep_returns_true() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Asleep;
        state.sleeping_until = Some(Utc::now());
        assert!(apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.sleeping_until, None);
    }

    #[test]
    fn apply_message_wake_from_booting_returns_true() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::Booting;
        assert!(apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[test]
    fn apply_message_wake_from_running_returns_false() {
        let mut state = AgentState::new("test");
        state.status = AgentStatus::AwakeRunning;
        assert!(!apply_message_wake_projection(&mut state));
        assert_eq!(state.status, AgentStatus::AwakeRunning);
    }

    // --- is_operator_interjection_message ---

    #[test]
    fn operator_interjection_detected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
            MessageBody::Text {
                text: "urgent".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(is_operator_interjection_message(&msg));
    }

    #[test]
    fn non_interjection_priority_rejected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "normal".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(!is_operator_interjection_message(&msg));
    }

    #[test]
    fn non_operator_kind_rejected() {
        let msg = MessageEnvelope::new(
            "agent-1",
            MessageKind::SystemTick,
            MessageOrigin::Operator {
                actor_id: Some("user".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Interject,
            MessageBody::Text {
                text: "tick".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(!is_operator_interjection_message(&msg));
    }
}
