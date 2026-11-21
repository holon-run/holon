use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::error::LifecycleRejection;
use crate::tool::helpers::parse_tool_args;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::delete_agent::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: DeleteAgentArgs = parse_tool_args(NAME, input)?;
    let job = runtime
        .delete_supervised_child(&args.agent_id, args.incarnation)
        .await
        .map_err(|error| {
            if error.is::<crate::deletion::UnregisteredHomeArtifact>() {
                LifecycleRejection::UnregisteredHomeArtifact.into_error()
            } else {
                error
            }
        })?;
    serialize_success(NAME, &DeleteAgentResult { deletion: job })
}
