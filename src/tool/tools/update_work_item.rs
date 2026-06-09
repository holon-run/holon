use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::validate_non_empty,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{
    create_work_item::WorkItemPlanStatusArg,
    serialize_success,
    work_item_action::{
        convert_todo_list, parse_work_item_action_args, TodoItemArgs, WorkItemMutationResult,
    },
    work_item_query::{query_context, view_for_record},
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
    pub(crate) todo_list: Option<Vec<TodoItemArgs>>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/update_work_item.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: UpdateWorkItemArgs = parse_work_item_action_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let objective = args
        .objective
        .map(|value| validate_non_empty(value, NAME, "objective"))
        .transpose()?;
    let todo_list = args
        .todo_list
        .map(|items| convert_todo_list(NAME, items))
        .transpose()?;
    let work_item = runtime
        .update_work_item_fields_with_recheck(
            work_item_id,
            objective,
            args.plan_status.map(Into::into),
            None,
            todo_list,
            None,
            None,
        )
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true, None, None, None).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
