use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{invalid_tool_input, normalize_optional_non_empty, validate_non_empty},
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
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
    #[serde(default)]
    pub(crate) blocked_by: Option<Option<String>>,
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub(crate) recheck_after: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            "Update mutable state fields for an existing work item. Use objective to refine the short target, plan_status for planning state, todo_list to replace the full progress checklist snapshot, blocked_by for blockers, and recheck_after as a positive millisecond fallback deadline when setting blocked_by. Edit the plan_artifact.path file directly for plan body changes.",
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
    if args.recheck_after == Some(0) {
        return Err(invalid_tool_input(
            NAME,
            "UpdateWorkItem `recheck_after` must be a positive integer when provided",
            serde_json::json!({
                "field": "recheck_after",
                "validation_error": "must be greater than 0",
            }),
            "omit `recheck_after`, or provide a positive integer millisecond delay when setting `blocked_by`",
        ));
    }
    if args.recheck_after.is_some() && blocked_by.as_ref().is_none_or(Option::is_none) {
        return Err(invalid_tool_input(
            NAME,
            "UpdateWorkItem `recheck_after` only applies when setting `blocked_by`",
            serde_json::json!({
                "field": "recheck_after",
                "validation_error": "requires non-empty blocked_by",
            }),
            "provide `recheck_after` only together with a non-empty `blocked_by` value",
        ));
    }
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
            blocked_by,
            args.recheck_after,
        )
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true, None).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
