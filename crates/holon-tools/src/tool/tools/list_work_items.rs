use super::work_item_query::WorkItemQueryContext;
use super::work_item_query::WorkItemView;
use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_WORK_ITEMS;

pub const DEFAULT_LIMIT: usize = 20;

pub const MAX_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ListWorkItemsFilter {
    All,
    Open,
    Completing,
    Completed,
    Current,
    Queued,
    Yielded,
    Blocked,
    WaitingForOperator,
    Runnable,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWorkItemsArgs {
    #[serde(default)]
    pub filter: Option<ListWorkItemsFilter>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_todo_list: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ListWorkItemsResult {
    pub context: WorkItemQueryContext,
    pub filter: ListWorkItemsFilter,
    pub returned: usize,
    pub total_matching: usize,
    pub limit: usize,
    pub work_items: Vec<WorkItemView>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListWorkItemsArgs>(
            NAME,
            include_str!("../tool_descriptions/list_work_items.md"),
        )?,
    })
}
