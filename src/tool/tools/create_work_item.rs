use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{normalize_optional_non_empty, validate_non_empty},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily, WorkItemPlanStatus},
};

use super::{
    serialize_success,
    work_item_action::{
        convert_todo_list, parse_work_item_action_args, TodoItemArgs, WorkItemMutationResult,
    },
    work_item_query::view_for_created_work_item,
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = crate::tool::names::CREATE_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateWorkItemArgs {
    pub(crate) objective: String,
    #[serde(default)]
    pub(crate) plan_status: Option<WorkItemPlanStatusArg>,
    #[serde(default)]
    pub(crate) plan: Option<String>,
    #[serde(default)]
    pub(crate) todo_list: Option<Vec<TodoItemArgs>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WorkItemPlanStatusArg {
    Draft,
    Ready,
    NeedsInput,
}

impl From<WorkItemPlanStatusArg> for WorkItemPlanStatus {
    fn from(value: WorkItemPlanStatusArg) -> Self {
        match value {
            WorkItemPlanStatusArg::Draft => Self::Draft,
            WorkItemPlanStatusArg::Ready => Self::Ready,
            WorkItemPlanStatusArg::NeedsInput => Self::NeedsInput,
        }
    }
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CreateWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/create_work_item.md"),
        )?,
    })
}

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
        .create_work_item_owned_result(
            objective,
            args.plan_status.map(Into::into),
            normalize_optional_non_empty(plan),
            todo_list,
        )
        .await?;
    let (work_item, material) = view_for_created_work_item(runtime, work_item).await?;
    let result = serialize_success(NAME, &WorkItemMutationResult::new(work_item));
    drop(material);
    result
}
