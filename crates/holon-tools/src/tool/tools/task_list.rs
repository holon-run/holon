use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::TaskListEntry;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_TASKS;

pub const LEGACY_NAME: &str = crate::tool::names::TASK_LIST;

pub const DEFAULT_LIMIT: usize = 20;

pub const MAX_LIMIT: usize = 100;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTasksArgs {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListTasksResult {
    pub total_active: usize,
    pub returned: usize,
    pub limit: usize,
    pub tasks: Vec<TaskListEntry>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListTasksArgs>(
            NAME,
            include_str!("../tool_descriptions/list_tasks.md"),
        )?,
    })
}

pub fn legacy_definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListTasksArgs>(
            LEGACY_NAME,
            include_str!("../tool_descriptions/task_list_legacy.md"),
        )?,
    })
}
