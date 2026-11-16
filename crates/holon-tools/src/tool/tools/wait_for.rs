use super::work_item_query::WorkItemView;
use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use crate::types::WaitConditionSummary;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use holon_domain::WaitForContinuation;
use holon_domain::WaitForScope;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::WAIT_FOR;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitForWakeArg {
    OperatorInput,
    TaskResult,
    External,
    Timer,
    System,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitForDeliveryArg {
    Final,
    Silent,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitForArgs {
    pub reason: String,
    pub wake: WaitForWakeArg,
    pub delivery: WaitForDeliveryArg,
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub recheck_after_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitForOwner {
    WorkItem { work_item_id: String },
    AgentLifecycle { agent_id: String },
}

#[derive(Debug, Serialize)]
pub struct WaitForResult {
    pub continuation: WaitForContinuation,
    pub scope: WaitForScope,
    pub owner: WaitForOwner,
    pub reason: String,
    pub wake: WaitForWakeArg,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_work_item_id: Option<String>,
    /// How the actual waiter was selected: execution_binding (a conflicting
    /// explicit work_item_id is ignored), explicit_request, turn_bound,
    /// current_focus, or agent_lifecycle (#3124).
    pub owner_selection: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_at: Option<DateTime<Utc>>,
    pub wait_condition: WaitConditionSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<WorkItemView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_wait_condition_ids: Vec<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<WaitForArgs>(NAME, include_str!("../tool_descriptions/wait_for.md"))?,
    })
}

pub struct WaitForOwnerDisclosure {
    pub waiter_work_item_id: Option<String>,
    pub requested_work_item_id: Option<String>,
    pub owner_selection: WaitForOwnerSelection,
    pub ignored_request: bool,
}

impl WaitForOwnerDisclosure {
    /// Explains that an explicit work_item_id was ignored because the
    /// execution binding takes priority (#3124).
    pub fn ignore_note(&self) -> Option<String> {
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

pub fn parse_wait_for_args(input: &Value) -> Result<WaitForArgs> {
    parse_tool_args(NAME, input)
}

pub fn optional_resource(resource: Option<String>) -> Option<String> {
    resource
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn validate_wait_for_args(args: &WaitForArgs) -> Result<()> {
    validate_non_empty(args.reason.clone(), NAME, "reason")?;
    let resource = optional_resource(args.resource.clone());
    validate_resource_for_wake(args.wake, resource.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitForOwnerSelection {
    ExecutionBinding,
    ExplicitRequest,
    TurnBound,
    CurrentFocus,
    AgentLifecycle,
}

impl WaitForOwnerSelection {
    pub fn as_str(self) -> &'static str {
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

pub fn resolve_wait_work_item_id(
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

pub fn validate_resource_for_wake(wake: WaitForWakeArg, resource: Option<&str>) -> Result<()> {
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OperatorInput => "operator_input",
            Self::TaskResult => "task_result",
            Self::External => "external",
            Self::Timer => "timer",
            Self::System => "system",
        }
    }
}
