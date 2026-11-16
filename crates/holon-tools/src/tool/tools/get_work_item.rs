use super::work_item_query::WorkItemQueryContext;
use super::work_item_query::WorkItemView;
use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::GET_WORK_ITEM;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetWorkItemArgs {
    pub work_item_id: String,
    #[serde(default)]
    pub include_todo_list: bool,
}

#[derive(Serialize, Deserialize)]
pub struct GetWorkItemResult {
    pub context: WorkItemQueryContext,
    pub work_item: WorkItemView,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/get_work_item.md"),
        )?,
    })
}
