use super::serialize_success;
use super::work_item_action::convert_todo_list;
use super::work_item_action::parse_work_item_action_args;
use super::work_item_action::WorkItemMutationResult;
use super::work_item_query::query_context;
use super::work_item_query::view_for_record;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::create_work_item::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateWorkItemArgs = parse_work_item_action_args(NAME, input)?;
    let objective = validate_non_empty(args.objective, NAME, "objective")?;
    let plan = args
        .plan
        .map(|value| validate_non_empty(value, NAME, "plan"))
        .transpose()?;
    let todo_list = args
        .todo_list
        .map(|items| convert_todo_list(NAME, items))
        .transpose()?
        .unwrap_or_default();
    let work_item = runtime
        .create_work_item(
            objective,
            args.plan_status.map(Into::into),
            normalize_optional_non_empty(plan),
            todo_list,
        )
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true, None, None).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
