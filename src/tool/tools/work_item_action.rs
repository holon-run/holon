use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    tool::helpers::validate_non_empty,
    types::{WorkItemRecord, WorkPlanItem, WorkPlanSnapshot, WorkPlanStepStatus},
};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WorkPlanStepStateArgs {
    Pending,
    Doing,
    Done,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkPlanItemArgs {
    pub(crate) step: String,
    pub(crate) state: WorkPlanStepStateArgs,
}

pub(crate) fn convert_plan(
    tool_name: &str,
    plan: Vec<WorkPlanItemArgs>,
) -> Result<Vec<WorkPlanItem>> {
    plan.into_iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(WorkPlanItem {
                step: validate_non_empty(item.step, tool_name, "step")
                    .with_context(|| format!("invalid work plan item {index}"))?,
                status: match item.state {
                    WorkPlanStepStateArgs::Pending => WorkPlanStepStatus::Pending,
                    WorkPlanStepStateArgs::Doing => WorkPlanStepStatus::InProgress,
                    WorkPlanStepStateArgs::Done => WorkPlanStepStatus::Completed,
                },
            })
        })
        .collect()
}

#[derive(Serialize)]
pub(crate) struct WorkItemMutationResult {
    pub(crate) work_item: WorkItemRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan: Option<WorkPlanSnapshot>,
}
