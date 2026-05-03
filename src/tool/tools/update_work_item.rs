use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{normalize_optional_non_empty, validate_non_empty},
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{
    create_work_item::WorkItemPlanStatusArg,
    serialize_success,
    work_item_action::{
        convert_todo_list, parse_work_item_action_args, TodoItemArgs, WorkItemMutationResult,
    },
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "UpdateWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateWorkItemArgs {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) objective: Option<String>,
    #[serde(default)]
    pub(crate) plan_status: Option<WorkItemPlanStatusArg>,
    #[serde(default)]
    pub(crate) plan: Option<Option<String>>,
    #[serde(default)]
    pub(crate) todo_list: Option<Vec<TodoItemArgs>>,
    #[serde(default)]
    pub(crate) blocked_by: Option<Option<String>>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            "Update mutable fields for an existing work item. Use objective to refine the short target, plan to replace durable task understanding, and todo_list to replace the full progress checklist snapshot.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: UpdateWorkItemArgs = parse_work_item_action_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let objective = args
        .objective
        .map(|value| validate_non_empty(value, NAME, "objective"))
        .transpose()?;
    let blocked_by = args
        .blocked_by
        .map(|value| value.and_then(|inner| normalize_optional_non_empty(Some(inner))));
    let plan = args
        .plan
        .map(|value| {
            value
                .map(|inner| validate_non_empty(inner, NAME, "plan"))
                .transpose()
        })
        .transpose()?;
    let todo_list = args
        .todo_list
        .map(|items| convert_todo_list(NAME, items))
        .transpose()?;
    let work_item = runtime
        .update_work_item_fields(
            work_item_id,
            objective,
            args.plan_status.map(Into::into),
            plan,
            todo_list,
            blocked_by,
        )
        .await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
