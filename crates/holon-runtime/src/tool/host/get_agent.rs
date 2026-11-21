use super::serialize_success;
use crate::http_dto::SlimAgentDto;
use crate::runtime::RuntimeHandle;
use crate::tool::error::LifecycleRejection;
use crate::tool::helpers::parse_tool_args;
use crate::types::GetAgentResult;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::get_agent::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: GetAgentArgs = parse_tool_args(NAME, input)?;
    let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let projection = loop {
        let read = runtime
            .agent_status_projection_for(args.agent_id.as_deref())
            .await;
        match read {
            Ok(summary) => break summary,
            Err(error)
                if error.is::<crate::runtime_db::RuntimeDbRetryableError>()
                    && crate::runtime_error::describe_runtime_error(&error).retryable
                    && tokio::time::Instant::now() < retry_deadline =>
            {
                tracing::debug!(
                    target: "holon::observer_read",
                    tool = NAME,
                    "retrying transient agent summary read"
                );
                tokio::time::sleep_until(std::cmp::min(
                    retry_deadline,
                    tokio::time::Instant::now() + std::time::Duration::from_millis(10),
                ))
                .await;
                if tokio::time::Instant::now() >= retry_deadline {
                    return Err(error);
                }
            }
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::host::PublicAgentError>(),
                    Some(crate::host::PublicAgentError::Deleted { .. })
                ) =>
            {
                return Err(LifecycleRejection::AgentDeleted.into_error());
            }
            Err(error) => return Err(error),
        }
    };
    serialize_success(
        NAME,
        &GetAgentResult {
            agent: SlimAgentDto::from(&projection),
        },
    )
}
