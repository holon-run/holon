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
    serialize_success,
    work_item_action::{
        convert_plan, parse_work_item_action_args, WorkItemMutationResult, WorkPlanItemArgs,
    },
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "UpdateWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateWorkItemArgs {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) blocked_by: Option<Option<String>>,
    #[serde(default)]
    pub(crate) plan: Option<Vec<WorkPlanItemArgs>>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            "Update mutable fields for an existing work item. Plan updates replace the full checklist snapshot.",
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
    let blocked_by = args
        .blocked_by
        .map(|value| value.and_then(|inner| normalize_optional_non_empty(Some(inner))));
    let plan = args.plan.map(|plan| convert_plan(NAME, plan)).transpose()?;
    let (work_item, plan) = runtime
        .update_work_item_fields(work_item_id, blocked_by, plan)
        .await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item, plan))
}
