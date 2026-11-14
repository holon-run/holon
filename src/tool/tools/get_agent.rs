use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    http_dto::SlimAgentDto,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{GetAgentResult, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::GET_AGENT;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetAgentArgs {
    #[serde(default)]
    pub agent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetAgentArgs>(NAME, include_str!("../tool_descriptions/get_agent.md"))?,
    })
}

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
