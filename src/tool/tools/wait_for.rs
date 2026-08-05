use anyhow::Result;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::{RuntimeHandle, WaitForRegistrationOutcome, WaitForScope, WaitForWakeKind},
    tool::{
        helpers::{invalid_tool_input, parse_tool_args, validate_non_empty},
        spec::typed_spec,
        ToolResult,
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitForArgs {
    pub(crate) reason: String,
    pub(crate) wake: WaitForWakeArg,
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
    pub(crate) scope: WaitForScope,
    pub(crate) owner: WaitForOwner,
    pub(crate) reason: String,
    pub(crate) wake: WaitForWakeArg,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item_id: Option<String>,
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
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<ToolResult> {
    let args = parse_wait_for_args(input)?;
    let reason = validate_non_empty(args.reason, NAME, "reason")?;
    let resource = optional_resource(args.resource);
    validate_resource_for_wake(args.wake, resource.as_deref())?;

    let context = query_context(runtime).await?;
    let state = runtime.agent_state().await?;
    let explicit_work_item_id = optional_resource(args.work_item_id);
    let work_item_id = explicit_work_item_id.or_else(|| {
        state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.work_item_id.clone())
            .or_else(|| {
                state
                    .current_turn_work_item_id
                    .clone()
                    .or_else(|| context.current_work_item_id.clone())
            })
    });
    let registration = runtime
        .register_wait_for_outcome(
            agent_id,
            work_item_id.clone(),
            args.wake.into(),
            resource.clone(),
            reason.clone(),
            args.recheck_after_ms,
        )
        .await?;
    let registration = match registration {
        WaitForRegistrationOutcome::TaskResultQueued {
            task_id,
            result_message_id,
        } => {
            let mut result = ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_queued",
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                }),
                Some(format!(
                    "task result already completed; queued exact result message {result_message_id}"
                )),
            );
            result.terminal_transition = true;
            return Ok(result);
        }
        WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
            task_id,
            result_message_id,
        } => {
            return Ok(ToolResult::success(
                NAME,
                json!({
                    "disposition": "task_result_already_consumed",
                    "task_id": task_id,
                    "result_message_id": result_message_id,
                }),
                Some(format!(
                    "task result was already consumed: {result_message_id}"
                )),
            ));
        }
        WaitForRegistrationOutcome::Registered { registration } => registration,
    };
    let updated_context = query_context(runtime).await?;
    let work_item = match registration.work_item {
        Some(record) => {
            Some(view_for_record(runtime, &updated_context, record, true, None, None).await?)
        }
        None => None,
    };
    let owner = work_item_id
        .clone()
        .map(|work_item_id| WaitForOwner::WorkItem { work_item_id })
        .unwrap_or_else(|| WaitForOwner::AgentLifecycle {
            agent_id: agent_id.to_string(),
        });
    let result = WaitForResult {
        scope: registration.scope,
        owner,
        reason: reason.clone(),
        wake: args.wake,
        resource,
        work_item_id,
        recheck_after_ms: registration.recheck_after_ms,
        recheck_at: registration.recheck_at,
        wait_condition: WaitConditionSummary::from(registration.condition),
        work_item,
        cancelled_wait_condition_ids: registration.cancelled_wait_condition_ids,
    };
    let value = serde_json::to_value(&result)?;
    Ok(ToolResult::sleep(
        NAME,
        value,
        Some(match result.scope {
            WaitForScope::WorkItem => format!("waiting on work item: {reason}"),
            WaitForScope::Agent => format!("waiting at agent scope: {reason}"),
        }),
        None,
    ))
}

fn parse_wait_for_args(input: &Value) -> Result<WaitForArgs> {
    parse_tool_args(NAME, input)
}

fn optional_resource(resource: Option<String>) -> Option<String> {
    resource
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
    fn wait_for_parses_recheck_after_ms() {
        let args = parse_wait_for_args(&json!({
            "reason": "wait",
            "wake": "external",
            "recheck_after_ms": 300000,
        }))
        .unwrap();

        assert_eq!(args.recheck_after_ms, Some(300000));
    }
}
