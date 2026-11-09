use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    http_dto::SlimAgentDto,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, GetAgentCompactResult, GetAgentResult, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::GET_AGENT;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetAgentArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<GetAgentDetail>,
}

#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GetAgentDetail {
    #[default]
    Compact,
    Full,
}

#[derive(Serialize)]
#[serde(untagged)]
enum GetAgentResponse {
    Compact(GetAgentCompactResult),
    Full(GetAgentResult),
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetAgentArgs>(NAME, include_str!("../tool_descriptions/get_agent.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: GetAgentArgs = parse_tool_args(NAME, input)?;
    let show_cleanup = args.agent_id.as_deref().is_none_or(|id| id == _agent_id);
    let subagent_cleanup = if show_cleanup {
        runtime.subagent_cleanup_candidates().await?
    } else {
        Vec::new()
    };
    let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let summary = loop {
        let read = match args.agent_id.as_deref() {
            None => runtime.agent_summary().await,
            // Canonical inspection does not start an unloaded target runtime.
            Some(requested_id) => runtime.agent_summary_for(requested_id).await,
        };
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
    let (summary, _material) = summary.into_parts();
    let response = match args.detail.unwrap_or_default() {
        GetAgentDetail::Compact => GetAgentResponse::Compact(GetAgentCompactResult {
            agent: SlimAgentDto::from(&summary),
            subagent_cleanup,
        }),
        GetAgentDetail::Full => GetAgentResponse::Full(GetAgentResult {
            agent: summary,
            subagent_cleanup,
        }),
    };
    let serialized = serialize_success(NAME, &response);
    drop(_material);
    serialized
}
