use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::list_subagent_cleanup::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    parse_tool_args::<ListSubagentCleanupArgs>(NAME, input)?;
    serialize_success(
        NAME,
        &ListSubagentCleanupResult {
            subagent_cleanup: runtime.subagent_cleanup_candidates().await?,
        },
    )
}
