use super::work_item_action::TodoItemArgs;
use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use crate::types::WorkItemPlanStatus;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

pub const NAME: &str = crate::tool::names::CREATE_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkItemArgs {
    pub objective: String,
    #[serde(default)]
    pub plan_status: Option<WorkItemPlanStatusArg>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub todo_list: Option<Vec<TodoItemArgs>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum WorkItemPlanStatusArg {
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

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CreateWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/create_work_item.md"),
        )?,
    })
}
