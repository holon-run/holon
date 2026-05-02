use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::validate_non_empty,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{
    serialize_success,
    work_item_action::{
        convert_plan, parse_work_item_action_args, WorkItemMutationResult, WorkPlanItemArgs,
    },
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "CreateWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateWorkItemArgs {
    pub(crate) delivery_target: String,
    #[serde(default)]
    pub(crate) plan: Option<Vec<WorkPlanItemArgs>>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CreateWorkItemArgs>(
            NAME,
            "Create a new open work item for a genuinely separate delivery target. Do not create a new work item just to refine the current task; use UpdateWorkItem.delivery_target for that. Use PickWorkItem separately to make a different existing item current.",
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
    let delivery_target = validate_non_empty(args.delivery_target, NAME, "delivery_target")?;
    let plan = args.plan.map(|plan| convert_plan(NAME, plan)).transpose()?;
    let (work_item, plan) = runtime.create_work_item(delivery_target, plan).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item, plan))
}
