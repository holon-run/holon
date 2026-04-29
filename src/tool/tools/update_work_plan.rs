use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel, WorkPlanItem, WorkPlanStepStatus},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "UpdateWorkPlan";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WorkPlanStepStatusArgs {
    Pending,
    InProgress,
    Completed,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkPlanItemArgs {
    pub(crate) step: String,
    pub(crate) status: WorkPlanStepStatusArgs,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateWorkPlanArgs {
    pub(crate) work_item_id: String,
    pub(crate) plan: Vec<WorkPlanItemArgs>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkPlanArgs>(
            NAME,
            "Replace the current full work-plan snapshot for one work item.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: UpdateWorkPlanArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let plan = args
        .plan
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(WorkPlanItem {
                step: validate_non_empty(item.step, NAME, "step")
                    .map_err(|error| error.context(format!("invalid work plan item {index}")))?,
                status: match item.status {
                    WorkPlanStepStatusArgs::Pending => WorkPlanStepStatus::Pending,
                    WorkPlanStepStatusArgs::InProgress => WorkPlanStepStatus::InProgress,
                    WorkPlanStepStatusArgs::Completed => WorkPlanStepStatus::Completed,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = runtime.update_work_plan(work_item_id, plan).await?;
    serialize_success(NAME, &snapshot)
}
