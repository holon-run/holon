//! Result closure and runtime error status from supplied records.

use crate::{
    AuditEvent, BriefRecord, ClosureDecision, ClosureOutcome, RuntimePosture, TurnTerminalKind,
    WaitingReason, WorkItemSchedulingState, WorkReactivationSignal,
};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Default)]
pub struct ClosureFacts {
    pub runtime_error: bool,
    pub awaiting_operator_input: bool,
    pub active_blocking_tasks: usize,
    pub active_waiting_intents: usize,
    pub active_agent_waiting_intents: usize,
    pub active_timers: usize,
    pub waiting_work_item_scheduling_state: Option<WorkItemSchedulingState>,
    pub work_signal: Option<WorkReactivationSignal>,
    pub turn_started: bool,
    pub turn_in_progress: bool,
    pub turn_terminal_kind: Option<TurnTerminalKind>,
    pub runtime_posture: Option<RuntimePosture>,
}

/// Derive the closure outcome and emit each evidence entry in order.
/// The caller formats each entry, applying any materialization budget first.
pub fn derive_closure_decision(
    facts: ClosureFacts,
    push_evidence: fn(&mut Vec<String>, std::fmt::Arguments<'_>) -> anyhow::Result<()>,
) -> anyhow::Result<ClosureDecision> {
    let runtime_posture = facts.runtime_posture.unwrap_or(RuntimePosture::Awake);
    let mut evidence = Vec::new();
    if facts.runtime_error {
        push_evidence(&mut evidence, format_args!("{}", "runtime_error"))?;
    }
    if facts.awaiting_operator_input {
        push_evidence(
            &mut evidence,
            format_args!("{}", "awaiting_operator_input_signal"),
        )?;
    }
    if facts.active_blocking_tasks > 0 {
        push_evidence(
            &mut evidence,
            format_args!("active_blocking_tasks={}", facts.active_blocking_tasks),
        )?;
    }
    if facts.active_waiting_intents > 0 {
        push_evidence(
            &mut evidence,
            format_args!("active_waiting_intents={}", facts.active_waiting_intents),
        )?;
    }
    if facts.active_agent_waiting_intents > 0 {
        push_evidence(
            &mut evidence,
            format_args!(
                "active_agent_waiting_intents={}",
                facts.active_agent_waiting_intents
            ),
        )?;
    }
    if facts.active_timers > 0 {
        push_evidence(
            &mut evidence,
            format_args!("active_timers={}", facts.active_timers),
        )?;
    }
    if let Some(state) = facts.waiting_work_item_scheduling_state {
        push_evidence(
            &mut evidence,
            format_args!("work_item_scheduling_state={state:?}"),
        )?;
    }
    if let Some(work_signal) = facts.work_signal.as_ref() {
        push_evidence(
            &mut evidence,
            format_args!("work_reactivation_mode={:?}", work_signal.reactivation_mode),
        )?;
        push_evidence(
            &mut evidence,
            format_args!("work_item_state={:?}", work_signal.state),
        )?;
        push_evidence(
            &mut evidence,
            format_args!("work_item_id={}", work_signal.work_item_id),
        )?;
    }
    if runtime_posture == RuntimePosture::Sleeping {
        push_evidence(&mut evidence, format_args!("{}", "runtime_sleeping"))?;
    }

    if facts.runtime_error {
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Failed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    if facts.turn_in_progress {
        push_evidence(&mut evidence, format_args!("{}", "turn_in_progress"))?;
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    if let Some(kind) = facts.turn_terminal_kind {
        if kind.is_failure() {
            let marker = match kind {
                TurnTerminalKind::Aborted => "turn_terminal_aborted",
                TurnTerminalKind::Interrupted => "turn_terminal_interrupted",
                TurnTerminalKind::BaselineOverBudget => "turn_terminal_baseline_over_budget",
                TurnTerminalKind::DeferredToFallback => "turn_terminal_deferred_to_fallback",
                TurnTerminalKind::ProviderFailedNeedsRecovery => {
                    "turn_terminal_provider_failed_needs_recovery"
                }
                TurnTerminalKind::Completed => unreachable!("completed is not a failure"),
            };
            push_evidence(&mut evidence, format_args!("{}", marker))?;
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Failed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
    }

    if facts.turn_started && facts.turn_terminal_kind.is_none() {
        push_evidence(&mut evidence, format_args!("{}", "turn_terminal_missing"))?;
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    if let Some(work_signal) = facts.work_signal {
        push_evidence(&mut evidence, format_args!("{}", "runnable_work_present"))?;
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Continuable,
            waiting_reason: None,
            work_signal: Some(work_signal),
            runtime_posture,
            evidence,
        });
    }

    if facts.awaiting_operator_input {
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingOperatorInput),
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    if facts.active_agent_waiting_intents > 0 {
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    if facts.active_timers > 0 {
        return Ok(ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingTimer),
            work_signal: None,
            runtime_posture,
            evidence,
        });
    }

    match facts.waiting_work_item_scheduling_state {
        Some(WorkItemSchedulingState::WaitingTask) => {
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Waiting,
                waiting_reason: Some(WaitingReason::AwaitingTaskResult),
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
        Some(WorkItemSchedulingState::WaitingExternal) => {
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Waiting,
                waiting_reason: Some(WaitingReason::AwaitingExternalChange),
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
        Some(WorkItemSchedulingState::WaitingTimer) => {
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Waiting,
                waiting_reason: Some(WaitingReason::AwaitingTimer),
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
        Some(WorkItemSchedulingState::WaitingOperator) => {
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Waiting,
                waiting_reason: Some(WaitingReason::AwaitingOperatorInput),
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
        Some(WorkItemSchedulingState::WaitingSystem) => {
            return Ok(ClosureDecision {
                outcome: ClosureOutcome::Waiting,
                waiting_reason: Some(WaitingReason::AwaitingExternalChange),
                work_signal: None,
                runtime_posture,
                evidence,
            });
        }
        _ => {}
    }

    if matches!(facts.turn_terminal_kind, Some(TurnTerminalKind::Completed)) {
        push_evidence(&mut evidence, format_args!("{}", "turn_terminal_completed"))?;
    }
    push_evidence(&mut evidence, format_args!("{}", "no_waiting_condition"))?;
    Ok(ClosureDecision {
        outcome: ClosureOutcome::Completed,
        waiting_reason: None,
        work_signal: None,
        runtime_posture,
        evidence,
    })
}

pub fn runtime_error_active(events: &[AuditEvent], briefs: &[BriefRecord]) -> bool {
    let latest_runtime_error = events
        .iter()
        .filter(|event| event.kind == "runtime_error")
        .map(|event| event.created_at)
        .max();
    runtime_error_active_at(latest_runtime_error, briefs)
}

pub fn runtime_error_active_at(
    latest_runtime_error: Option<DateTime<Utc>>,
    briefs: &[BriefRecord],
) -> bool {
    let latest_success_result_brief = briefs
        .iter()
        .filter(|brief| brief.kind.is_success())
        .map(|brief| brief.created_at)
        .max();

    match (latest_runtime_error, latest_success_result_brief) {
        (Some(error_at), Some(result_at)) => error_at > result_at,
        (Some(_), None) => true,
        _ => false,
    }
}
