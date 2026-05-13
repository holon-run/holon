use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{normalize_optional_non_empty, validate_non_empty},
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel, WorkItemPlanStatus},
};

use super::{
    serialize_success,
    work_item_action::{
        convert_todo_list, parse_work_item_action_args, TodoItemArgs, WorkItemMutationResult,
    },
    work_item_query::{query_context, view_for_record},
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "CreateWorkItem";

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
            "Create a new open work item for a genuinely separate objective with an independent lifecycle. Optional plan seeds the work item's AgentHome plan.md artifact; edit that file directly for later plan changes. Use todo_list only for progress checklist items. Continuous discussion, planning, candidate screening, and option comparison should usually update the current work item instead. Do not create a new work item just to refine, narrow, or switch candidates inside the current task; use UpdateWorkItem.objective, UpdateWorkItem.plan_status, and UpdateWorkItem.todo_list for state updates.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
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
    let work_item = view_for_record(runtime, &context, work_item, true).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
