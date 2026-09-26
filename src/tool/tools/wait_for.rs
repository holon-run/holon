use anyhow::{Error, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::{
        RuntimeHandle, WaitForContinuation, WaitForRegistrationOutcome, WaitForScope,
        WaitForWakeKind,
    },
    tool::{
        helpers::{invalid_tool_input, parse_tool_args, validate_non_empty},
        spec::{typed_spec, AwaitWaitReportDirective, ToolExecutionContext, ToolLoopDirective},
        ToolError, ToolResult,
    },
    types::{AuthorityClass, ToolCapabilityFamily, WaitConditionSummary},
};

use super::{
    work_item_query::{query_context, view_for_record, WorkItemView},
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = crate::tool::names::WAIT_FOR;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForWakeArg {
    OperatorInput,
    TaskResult,
    External,
    Timer,
    System,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitForDeliveryArg {
    Final,
    Silent,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitForArgs {
    pub(crate) reason: String,
    pub(crate) wake: WaitForWakeArg,
    pub(crate) delivery: WaitForDeliveryArg,
    #[serde(default)]
    pub(crate) work_item_id: Option<String>,
    #[serde(default)]
    pub(crate) resource: Option<String>,
    #[serde(default)]
    pub(crate) recheck_after_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WaitForOwner {
    WorkItem { work_item_id: String },
    AgentLifecycle { agent_id: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct WaitForResult {
    pub(crate) continuation: WaitForContinuation,
    pub(crate) scope: WaitForScope,
    pub(crate) owner: WaitForOwner,
    pub(crate) reason: String,
    pub(crate) wake: WaitForWakeArg,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested_work_item_id: Option<String>,
    /// How the actual waiter was selected: execution_binding (a conflicting
    /// explicit work_item_id is ignored), explicit_request, turn_bound,
    /// current_focus, or agent_lifecycle (#3124).
    pub(crate) owner_selection: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_at: Option<DateTime<Utc>>,
    pub(crate) wait_condition: WaitConditionSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item: Option<WorkItemView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cancelled_wait_condition_ids: Vec<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<WaitForArgs>(NAME, include_str!("../tool_descriptions/wait_for.md"))?,
    })
}

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
                "expected_output": "final_text_only",
            }),
            Some("Awaiting the final operator-facing report before committing the wait.".into()),
            ToolLoopDirective::AwaitWaitReport(AwaitWaitReportDirective {
                input: input.clone(),
            }),
        ));
    }
    Ok(result)
}

pub(crate) async fn prepare_settlement(
    runtime: &RuntimeHandle,
    agent_id: &str,
    _authority_class: &AuthorityClass,
    args: WaitForArgs,
) -> Result<ToolResult> {
    settle_impl(runtime, agent_id, args, true).await
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
                        result.prepared_wait_for = Some(prepared);
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
    result.prepared_wait_for = prepared_wait_for;
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
    match outcome {
        WaitForRegistrationOutcome::TaskResultQueued {
            task_id,
            result_message_id,
            wait_condition_id,
        } => {
            let mut summary = format!(
                "yield and reenter for exact task result message {result_message_id}; the triggered wait preserves result admission"
            );
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
        } => {
            let mut summary = format!(
                "continue the current turn; task result was already consumed: {result_message_id}; no wait registered"
            );
            if let Some(note) = disclosure.ignore_note() {
                summary = format!("{summary}; {note}");
            }
            Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_already_consumed",
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
    }
}

struct WaitForOwnerDisclosure {
    waiter_work_item_id: Option<String>,
    requested_work_item_id: Option<String>,
    owner_selection: WaitForOwnerSelection,
    ignored_request: bool,
}

impl WaitForOwnerDisclosure {
    /// Explains that an explicit work_item_id was ignored because the
    /// execution binding takes priority (#3124).
    fn ignore_note(&self) -> Option<String> {
        if !self.ignored_request {
            return None;
        }
        Some(format!(
            "ignored work_item_id {} (owner_selection={}; waiter: {})",
            self.requested_work_item_id.as_deref().unwrap_or_default(),
            self.owner_selection.as_str(),
            self.waiter_work_item_id
                .as_deref()
                .unwrap_or("agent lifecycle"),
        ))
    }
}

pub(crate) fn parse_wait_for_args(input: &Value) -> Result<WaitForArgs> {
    parse_tool_args(NAME, input)
}

fn optional_resource(resource: Option<String>) -> Option<String> {
    resource
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_wait_for_args(args: &WaitForArgs) -> Result<()> {
    validate_non_empty(args.reason.clone(), NAME, "reason")?;
    let resource = optional_resource(args.resource.clone());
    validate_resource_for_wake(args.wake, resource.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitForOwnerSelection {
    ExecutionBinding,
    ExplicitRequest,
    TurnBound,
    CurrentFocus,
    AgentLifecycle,
}

impl WaitForOwnerSelection {
    fn as_str(self) -> &'static str {
        match self {
            Self::ExecutionBinding => "execution_binding",
            Self::ExplicitRequest => "explicit_request",
            Self::TurnBound => "turn_bound",
            Self::CurrentFocus => "current_focus",
            Self::AgentLifecycle => "agent_lifecycle",
        }
    }
}

/// Resolves who waits (#3124): the current execution binding first (a
/// conflicting explicit work_item_id is ignored, not fatal), then the
/// explicit request, the turn-bound WorkItem, and current focus.
fn resolve_wait_work_item_id(
    explicit_work_item_id: Option<String>,
    execution_work_item_id: Option<String>,
    turn_work_item_id: Option<String>,
    current_work_item_id: Option<String>,
) -> (Option<String>, WaitForOwnerSelection) {
    if let Some(work_item_id) = execution_work_item_id {
        return (Some(work_item_id), WaitForOwnerSelection::ExecutionBinding);
    }
    if let Some(work_item_id) = explicit_work_item_id {
        return (Some(work_item_id), WaitForOwnerSelection::ExplicitRequest);
    }
    if let Some(work_item_id) = turn_work_item_id {
        return (Some(work_item_id), WaitForOwnerSelection::TurnBound);
    }
    if let Some(work_item_id) = current_work_item_id {
        return (Some(work_item_id), WaitForOwnerSelection::CurrentFocus);
    }
    (None, WaitForOwnerSelection::AgentLifecycle)
}

fn validate_resource_for_wake(wake: WaitForWakeArg, resource: Option<&str>) -> Result<()> {
    match wake {
        WaitForWakeArg::TaskResult | WaitForWakeArg::Timer if resource.is_none() => {
            Err(invalid_tool_input(
                NAME,
                format!(
                    "WaitFor wake `{}` requires non-empty `resource`",
                    wake.as_str()
                ),
                json!({
                    "field": "resource",
                    "wake": wake,
                    "validation_error": "required",
                }),
                "provide `resource`; use the task id for task_result or timer id for timer",
            ))
        }
        _ => Ok(()),
    }
}

impl WaitForWakeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::OperatorInput => "operator_input",
            Self::TaskResult => "task_result",
            Self::External => "external",
            Self::Timer => "timer",
            Self::System => "system",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;
    use serde_json::json;

    #[test]
    fn wait_for_rejects_unknown_top_level_fields() {
        let error = parse_wait_for_args(&json!({
            "reason": "wait",
            "wake": "operator_input",
            "summary": "not allowed",
        }))
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("parse_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|error| error.contains("unknown field `summary`")));
    }

    #[test]
    fn wait_for_requires_resource_for_task_and_timer_waits() {
        for wake in [WaitForWakeArg::TaskResult, WaitForWakeArg::Timer] {
            let error = validate_resource_for_wake(wake, None).unwrap_err();
            let tool_error = ToolError::from_anyhow(&error);
            assert_eq!(tool_error.kind, "invalid_tool_input");
            assert_eq!(
                tool_error
                    .details
                    .as_ref()
                    .and_then(|value| value.get("field"))
                    .and_then(|value| value.as_str()),
                Some("resource")
            );
        }
    }

    #[test]
    fn wait_for_validates_final_wait_before_deferring() {
        let args = WaitForArgs {
            reason: "wait for timer".into(),
            wake: WaitForWakeArg::Timer,
            delivery: WaitForDeliveryArg::Final,
            work_item_id: None,
            resource: None,
            recheck_after_ms: None,
        };

        let error = validate_wait_for_args(&args).unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert_eq!(
            tool_error
                .details
                .as_ref()
                .and_then(|value| value.get("field"))
                .and_then(|value| value.as_str()),
            Some("resource")
        );
    }

    #[test]
    fn wait_for_allows_operator_and_external_without_resource() {
        validate_resource_for_wake(WaitForWakeArg::OperatorInput, None).unwrap();
        validate_resource_for_wake(WaitForWakeArg::External, None).unwrap();
        validate_resource_for_wake(WaitForWakeArg::System, None).unwrap();
    }

    #[test]
    fn wait_for_treats_empty_resource_as_absent() {
        assert_eq!(optional_resource(Some("  ".into())), None);
        assert_eq!(
            optional_resource(Some("  github:repo#1  ".into())),
            Some("github:repo#1".into())
        );
    }

    #[test]
    fn wait_owner_prefers_execution_binding_over_explicit_request() {
        assert_eq!(
            resolve_wait_work_item_id(
                None,
                Some("execution-work".into()),
                Some("turn-work".into()),
                Some("current-work".into()),
            ),
            (
                Some("execution-work".into()),
                WaitForOwnerSelection::ExecutionBinding,
            )
        );
        // A conflicting explicit request is ignored, not fatal (#3124):
        // the execution binding keeps deciding who waits.
        assert_eq!(
            resolve_wait_work_item_id(
                Some("explicit-work".into()),
                Some("execution-work".into()),
                Some("turn-work".into()),
                Some("current-work".into()),
            ),
            (
                Some("execution-work".into()),
                WaitForOwnerSelection::ExecutionBinding,
            )
        );
    }

    #[test]
    fn wait_owner_falls_back_to_explicit_turn_current_and_lifecycle() {
        assert_eq!(
            resolve_wait_work_item_id(
                Some("explicit-work".into()),
                Some("execution-work".into()),
                Some("turn-work".into()),
                Some("current-work".into()),
            ),
            (
                Some("execution-work".into()),
                WaitForOwnerSelection::ExecutionBinding,
            )
        );
        assert_eq!(
            resolve_wait_work_item_id(
                None,
                None,
                Some("turn-work".into()),
                Some("current-work".into()),
            ),
            (Some("turn-work".into()), WaitForOwnerSelection::TurnBound)
        );
        assert_eq!(
            resolve_wait_work_item_id(None, None, None, Some("current-work".into())),
            (
                Some("current-work".into()),
                WaitForOwnerSelection::CurrentFocus,
            )
        );
        assert_eq!(
            resolve_wait_work_item_id(None, None, None, None),
            (None, WaitForOwnerSelection::AgentLifecycle)
        );
    }

    #[test]
    fn wait_for_parses_recheck_after_ms() {
        let args = parse_wait_for_args(&json!({
            "reason": "wait",
            "wake": "external",
            "delivery": "silent",
            "recheck_after_ms": 300000,
        }))
        .unwrap();

        assert_eq!(args.recheck_after_ms, Some(300000));
    }

    #[test]
    fn wait_for_parses_integral_decimal_string_recheck_after_ms() {
        let args = parse_wait_for_args(&json!({
            "reason": "wait",
            "wake": "external",
            "delivery": "silent",
            "recheck_after_ms": "900000.0",
        }))
        .unwrap();

        assert_eq!(args.recheck_after_ms, Some(900000));
    }
}
