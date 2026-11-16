use super::create_work_item::WorkItemPlanStatusArg;
use super::work_item_action::TodoItemArgs;
use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

pub const NAME: &str = crate::tool::names::UPDATE_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkItemArgs {
    pub work_item_id: String,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub plan_status: Option<WorkItemPlanStatusArg>,
    #[serde(default)]
    pub todo_list: Option<Vec<TodoItemArgs>>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/update_work_item.md"),
        )?,
    })
}
