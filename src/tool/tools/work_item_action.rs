use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    tool::helpers::{parse_tool_args_with_recovery_hint, validate_non_empty},
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

pub(crate) fn parse_work_item_action_args<T>(
    tool_name: &str,
    input: &serde_json::Value,
) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    parse_tool_args_with_recovery_hint(tool_name, input, work_plan_recovery_hint())
}

fn work_plan_recovery_hint() -> &'static str {
    "use plan items like {\"step\":\"inspect current handler\",\"state\":\"done\"}; state must be pending, doing, or done"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkPlanStepStateView {
    Pending,
    Doing,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkPlanItemView {
    pub(crate) step: String,
    pub(crate) state: WorkPlanStepStateView,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkPlanView {
    pub(crate) work_item_id: String,
    pub(crate) agent_id: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) items: Vec<WorkPlanItemView>,
}

impl From<WorkPlanSnapshot> for WorkPlanView {
    fn from(snapshot: WorkPlanSnapshot) -> Self {
        Self {
            work_item_id: snapshot.work_item_id,
            agent_id: snapshot.agent_id,
            created_at: snapshot.created_at,
            items: snapshot
                .items
                .into_iter()
                .map(|item| WorkPlanItemView {
                    step: item.step,
                    state: match item.status {
                        WorkPlanStepStatus::Pending => WorkPlanStepStateView::Pending,
                        WorkPlanStepStatus::InProgress => WorkPlanStepStateView::Doing,
                        WorkPlanStepStatus::Completed => WorkPlanStepStateView::Done,
                    },
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WorkItemMutationResult {
    pub(crate) work_item: WorkItemRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan: Option<WorkPlanView>,
}

impl WorkItemMutationResult {
    pub(crate) fn new(work_item: WorkItemRecord, plan: Option<WorkPlanSnapshot>) -> Self {
        Self {
            work_item,
            plan: plan.map(Into::into),
        }
    }
}
