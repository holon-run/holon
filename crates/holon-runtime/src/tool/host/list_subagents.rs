use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::list_subagents::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args = parse_tool_args::<ListSubagentsArgs>(NAME, input)?;
    let page = runtime
        .list_subagents(
            Some(args.limit),
            args.cursor.as_deref(),
            args.include_cleanup,
        )
        .await?;
    serialize_success(NAME, &page)
}
