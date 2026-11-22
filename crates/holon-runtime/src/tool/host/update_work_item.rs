use super::serialize_success;
use super::work_item_action::convert_todo_list;
use super::work_item_action::parse_work_item_action_args;
use super::work_item_action::WorkItemMutationResult;
use super::work_item_query::query_context;
use super::work_item_query::view_for_record;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::update_work_item::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
    context: &crate::tool::spec::ToolExecutionContext,
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
        .update_work_item_fields_in_lane(
            work_item_id,
            objective,
            args.plan_status.map(Into::into),
            todo_list,
            None,
            Some(crate::runtime_db::transitions::WorkCommandLane::from_tool_context(context)),
        )
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true, None, None).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
