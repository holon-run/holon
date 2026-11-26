use super::work_item_query::query_context;
use super::work_item_query::view_for_record;
use crate::runtime::RuntimeHandle;
use crate::runtime::WaitForContinuation;
use crate::runtime::WaitForRegistrationOutcome;
use crate::runtime::WaitForScope;
use crate::runtime::WaitForWakeKind;
use crate::tool::helpers::validate_non_empty;
use crate::tool::spec::AwaitWaitReportDirective;
use crate::tool::spec::ToolExecutionContext;
use crate::tool::spec::ToolLoopDirective;
use crate::tool::ToolError;
use crate::tool::ToolResult;
use crate::types::AuthorityClass;
use crate::types::WaitConditionSummary;
use anyhow::Error;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::wait_for::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<ToolResult> {
    let args = parse_wait_for_args(input)?;
    validate_wait_for_args(&args)?;
    let needs_report =
        args.delivery == WaitForDeliveryArg::Final && context.completion_report_candidate.is_none();
    // Classification is side-effect free. Only a yielding outcome needs a report;
    // the report continuation re-prepares and the terminal transaction revalidates.
    let result = prepare_settlement(runtime, agent_id, authority_class, args).await?;
    if needs_report && result.should_sleep {
        return Ok(ToolResult::deferred(
            NAME,
            json!({
                "disposition": "awaiting_final_report",
                "wait_registered": false,
                "expected_output": "non_empty_final_text",
            }),
            Some(
                "Awaiting the final operator-facing report before committing the wait: no wait is registered yet. Finish any remaining authorized cleanup or verification with ordinary tools, then provide non-empty operator-facing final text; do not call WaitFor, CompleteWorkItem, PickWorkItem, or Sleep while this report is pending (changes to task state, WorkItem revision, execution binding, or wake state before settlement invalidate or clear this pending wait).".into(),
            ),
            ToolLoopDirective::AwaitWaitReport(AwaitWaitReportDirective {
                input: input.clone(),
            }),
        ));
    }
    Ok(result)
}

#[inline(never)]
pub(crate) fn prepare_settlement<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    _authority_class: &AuthorityClass,
    args: WaitForArgs,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    Box::pin(settle_impl(runtime, agent_id, args, true))
}

async fn settle_impl(
    runtime: &RuntimeHandle,
    agent_id: &str,
    args: WaitForArgs,
    prepare_only: bool,
) -> Result<ToolResult> {
    validate_wait_for_args(&args)?;
    let reason = validate_non_empty(args.reason, NAME, "reason")?;
    let resource = optional_resource(args.resource);

    let context = query_context(runtime).await?;
    let state = runtime.agent_state().await?;
    let requested_work_item_id = optional_resource(args.work_item_id.clone());
    let (work_item_id, owner_selection) = resolve_wait_work_item_id(
        requested_work_item_id.clone(),
        state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.work_item_id.clone()),
        state.current_turn_work_item_id.clone(),
        context.current_work_item_id.clone(),
    );
    let disclosure = WaitForOwnerDisclosure {
        waiter_work_item_id: work_item_id.clone(),
        requested_work_item_id: requested_work_item_id.clone(),
        owner_selection,
        ignored_request: requested_work_item_id
            .as_deref()
            .is_some_and(|requested| work_item_id.as_deref() != Some(requested)),
    };
    let (registration, prepared_wait_for) = if prepare_only {
        match runtime
            .prepare_wait_for_outcome(
                agent_id,
                work_item_id.clone(),
                args.wake.into(),
                resource.clone(),
                reason.clone(),
                args.recheck_after_ms,
            )
            .await
        {
            Ok(outcome) => match outcome {
                crate::runtime::PrepareWaitForOutcome::Prepared(mut prepared) => {
                    prepared.delivery = args.delivery;
                    if prepared.command.task_result_admission.is_some() {
                        let mut result = immediate_result(prepared.outcome(), &disclosure)?;
                        result.runtime_state.prepared_wait_for = Some(prepared);
                        return Ok(result);
                    }
                    (prepared.registration.clone(), Some(prepared))
                }
                crate::runtime::PrepareWaitForOutcome::Immediate(outcome) => {
                    return immediate_result(outcome, &disclosure);
                }
            },
            Err(error) => return timer_wait_error_result(error),
        }
    } else {
        let outcome = match runtime
            .register_wait_for_outcome(
                agent_id,
                work_item_id.clone(),
                args.wake.into(),
                resource.clone(),
                reason.clone(),
                args.recheck_after_ms,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => return timer_wait_error_result(error),
        };
        match outcome {
            WaitForRegistrationOutcome::Registered { registration } => (registration, None),
            outcome => return immediate_result(outcome, &disclosure),
        }
    };
    let updated_context = query_context(runtime).await?;
    let pending_condition = registration.condition.clone();
    let work_item_id = registration.condition.work_item_id.clone();
    let work_item = match registration.work_item {
        Some(record) => Some(
            view_for_record(
                runtime,
                &updated_context,
                record.clone(),
                true,
                None,
                Some(crate::work_item_scheduling::derive_work_item_scheduling(
                    crate::work_item_scheduling::WorkItemSchedulingFacts {
                        work_item: &record,
                        is_current: updated_context.current_work_item_id.as_deref()
                            == Some(record.id.as_str()),
                        is_yielded: false,
                        active_wait_conditions: std::slice::from_ref(&pending_condition),
                        trigger_delivery_by_id: &std::collections::BTreeMap::new(),
                    },
                )),
            )
            .await?,
        ),
        None => None,
    };
    let owner = work_item_id
        .clone()
        .map(|work_item_id| WaitForOwner::WorkItem { work_item_id })
        .unwrap_or_else(|| WaitForOwner::AgentLifecycle {
            agent_id: agent_id.to_string(),
        });
    let result = WaitForResult {
        continuation: WaitForContinuation::YieldAndWait,
        scope: registration.scope,
        owner,
        reason: reason.clone(),
        wake: args.wake,
        resource,
        work_item_id,
        requested_work_item_id: disclosure.requested_work_item_id.clone(),
        owner_selection: disclosure.owner_selection.as_str(),
        recheck_after_ms: registration.recheck_after_ms,
        recheck_at: registration.recheck_at,
        wait_condition: WaitConditionSummary::from(registration.condition),
        work_item,
        cancelled_wait_condition_ids: registration.cancelled_wait_condition_ids,
    };
    let value = serde_json::to_value(&result)?;
    let mut summary = match result.scope {
        WaitForScope::WorkItem => format!("yield and wait on work item: {reason}"),
        WaitForScope::Agent => format!("yield and wait at agent scope: {reason}"),
    };
    if let Some(note) = disclosure.ignore_note() {
        summary = format!("{summary}; {note}");
    }
    let mut result = ToolResult::sleep(NAME, value, Some(summary), None);
    result.terminal_transition = true;
    result.runtime_state.prepared_wait_for = prepared_wait_for;
    Ok(result)
}

fn timer_wait_error_result(error: Error) -> Result<ToolResult> {
    let Some(runtime_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<crate::runtime_error::RuntimeError>())
    else {
        return Err(error);
    };
    let code = runtime_error.descriptor().code.as_str();
    if !matches!(
        code,
        "timer_not_found"
            | "timer_agent_mismatch"
            | "timer_invalid_state"
            | "timer_wake_unavailable"
            | "timer_cancelled"
    ) {
        return Err(error);
    }

    let recovery_hint = match code {
        "timer_not_found" | "timer_agent_mismatch" | "timer_cancelled" => {
            "call CreateTimer or ListTimers, then wait using an active timer_id owned by this agent"
        }
        "timer_wake_unavailable" => {
            "create a new timer or use a timer_id whose completed wake has not been consumed"
        }
        "timer_invalid_state" => "create a new timer and wait using its returned timer_id",
        _ => unreachable!("timer error code was checked above"),
    };
    Ok(ToolResult::error(
        NAME,
        ToolError::new("invalid_tool_input", runtime_error.to_string())
            .with_domain(crate::runtime_error::RuntimeErrorDomain::Validation)
            .with_details(json!({
                "field": "resource",
                "wake": "timer",
                "code": code,
            }))
            .with_recovery_hint(recovery_hint),
    ))
}

fn immediate_result(
    outcome: WaitForRegistrationOutcome,
    disclosure: &WaitForOwnerDisclosure,
) -> Result<ToolResult> {
    let (outcome, recovery_message_id) = match outcome {
        WaitForRegistrationOutcome::TaskResultRecoveryQueued {
            task_id,
            result_message_id,
            recovery_message_id,
            wait_condition_id,
        } => (
            WaitForRegistrationOutcome::TaskResultQueued {
                task_id,
                result_message_id,
                wait_condition_id,
            },
            Some(recovery_message_id),
        ),
        outcome => (outcome, None),
    };
    let already_admitted = matches!(
        &outcome,
        WaitForRegistrationOutcome::TaskResultAlreadyAdmitted { .. }
    );
    match outcome {
        WaitForRegistrationOutcome::TaskResultQueued {
            task_id,
            result_message_id,
            wait_condition_id,
        } => {
            let mut summary = format!(
                "yield and reenter for exact task result message {result_message_id}; the triggered wait preserves result admission"
            );
            if let Some(recovery_id) = recovery_message_id.as_deref() {
                summary = format!(
                    "yield and reenter through canonical recovery wake {recovery_id} for pending task result {result_message_id}; the original message was not requeued"
                );
            }
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            let mut result = ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_queued",
                    "continuation": WaitForContinuation::YieldAndReenter,
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                    "recovery_message_id": recovery_message_id,
                    "wait_condition_id": wait_condition_id,
                    "waiter_work_item_id": disclosure.waiter_work_item_id,
                    "requested_work_item_id": disclosure.requested_work_item_id,
                    "owner_selection": disclosure.owner_selection.as_str(),
                }),
                Some(summary),
            );
            result.should_sleep = true;
            result.terminal_transition = true;
            Ok(result)
        }
        WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
            task_id,
            result_message_id,
            settlement_disposition,
        } => {
            let mut summary = format!(
                "continue the current turn; task result {result_message_id} settled as {settlement_disposition:?}; no wait registered"
            );
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_settled",
                    "settlement_disposition": settlement_disposition,
                    "continuation": WaitForContinuation::ContinueTurn,
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                    "waiter_work_item_id": disclosure.waiter_work_item_id,
                    "requested_work_item_id": disclosure.requested_work_item_id,
                    "owner_selection": disclosure.owner_selection.as_str(),
                }),
                Some(summary),
            ))
        }
        WaitForRegistrationOutcome::TaskResultAlreadyAdmitted {
            task_id,
            result_message_id,
            activation_id,
        }
        | WaitForRegistrationOutcome::TaskResultInFlight {
            task_id,
            result_message_id,
            activation_id,
        } => {
            let disposition = if already_admitted {
                "task_result_already_admitted"
            } else {
                "task_result_in_flight"
            };
            let mut summary = if already_admitted {
                format!("continue the current turn; task result {result_message_id} is admitted to this activation, not yet settled; no wait registered")
            } else {
                format!("continue the current turn; task result {result_message_id} is claimed by another active attempt; no duplicate wait registered")
            };
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": disposition,
                    "continuation": WaitForContinuation::ContinueTurn,
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                    "activation_id": activation_id,
                    "waiter_work_item_id": disclosure.waiter_work_item_id,
                    "requested_work_item_id": disclosure.requested_work_item_id,
                    "owner_selection": disclosure.owner_selection.as_str(),
                }),
                Some(summary),
            ))
        }
        WaitForRegistrationOutcome::TaskResultUnavailable {
            task_id,
            result_message_id,
            reason,
        } => {
            let mut summary = format!(
                "continue the current turn; task result {result_message_id}: {reason}; no wait registered or recovery budget reset"
            );
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_unavailable",
                    "continuation": WaitForContinuation::ContinueTurn,
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                    "requires_operator": reason == "recovery_paused_requires_operator",
                    "reason": reason,
                    "waiter_work_item_id": disclosure.waiter_work_item_id,
                    "requested_work_item_id": disclosure.requested_work_item_id,
                    "owner_selection": disclosure.owner_selection.as_str(),
                }),
                Some(summary),
            ))
        }
        WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter {
            task_id,
            result_message_id,
            wait_condition_id,
            claimed_by_work_item_id,
        } => {
            let claimed_by = match claimed_by_work_item_id.as_deref() {
                Some(work_item_id) => format!("work item {work_item_id}"),
                None => "agent lifecycle".to_string(),
            };
            let mut summary = format!(
                "continue the current turn; task result {result_message_id} was already claimed by another waiter ({claimed_by}); no duplicate wake registered"
            );
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_claimed_by_other_waiter",
                    "continuation": WaitForContinuation::ContinueTurn,
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                    "wait_condition_id": wait_condition_id,
                    "claimed_by_work_item_id": claimed_by_work_item_id,
                    "waiter_work_item_id": disclosure.waiter_work_item_id,
                    "requested_work_item_id": disclosure.requested_work_item_id,
                    "owner_selection": disclosure.owner_selection.as_str(),
                }),
                Some(summary),
            ))
        }
        WaitForRegistrationOutcome::Registered { .. } => {
            unreachable!("registered wait is handled by settle_impl")
        }
        WaitForRegistrationOutcome::TaskResultRecoveryQueued { .. } => {
            unreachable!("recovery outcome was normalized above")
        }
    }
}

impl From<WaitForWakeArg> for WaitForWakeKind {
    fn from(value: WaitForWakeArg) -> Self {
        match value {
            WaitForWakeArg::OperatorInput => WaitForWakeKind::OperatorInput,
            WaitForWakeArg::TaskResult => WaitForWakeKind::TaskResult,
            WaitForWakeArg::External => WaitForWakeKind::External,
            WaitForWakeArg::Timer => WaitForWakeKind::Timer,
            WaitForWakeArg::System => WaitForWakeKind::System,
        }
    }
}
