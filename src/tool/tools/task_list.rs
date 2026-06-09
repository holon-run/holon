use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, TaskListEntry, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "ListTasks";
pub(crate) const LEGACY_NAME: &str = "TaskList";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTasksArgs {
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ListTasksResult {
    pub(crate) total_active: usize,
    pub(crate) returned: usize,
    pub(crate) limit: usize,
    pub(crate) tasks: Vec<TaskListEntry>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListTasksArgs>(
            NAME,
            include_str!("../tool_descriptions/list_tasks.md"),
        )?,
    })
}

pub(crate) fn legacy_definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListTasksArgs>(
            LEGACY_NAME,
            include_str!("../tool_descriptions/task_list_legacy.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    execute_with_name(NAME, runtime, input).await
}

pub(crate) async fn execute_legacy(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    execute_with_name(LEGACY_NAME, runtime, input).await
}

async fn execute_with_name(
    tool_name: &str,
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListTasksArgs = parse_tool_args(tool_name, input)?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let current_agent = runtime.agent_state().await?;
    let agent_id = current_agent.id;
    let total_active = runtime.storage().active_task_count_for_agent(&agent_id)?;
    let tasks = runtime
        .latest_task_list_entries_for_agent(&agent_id, limit)
        .await?;
    serialize_success(
        tool_name,
        &ListTasksResult {
            total_active,
            returned: tasks.len(),
            limit,
            tasks,
        },
    )
}
