use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    host::PublicAgentError,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    tool::ToolError,
    types::{AgentGetResult, AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "AgentGet";

/// Arguments for the `AgentGet` tool.
///
/// `agent_id` is optional. When omitted (the default), the tool returns the
/// current agent's summary — the original behavior. When supplied, the tool
/// resolves the requested agent through the host bridge under the local
/// operator control-API trust boundary and returns that agent's summary.
/// Private child agents are reachable when an explicit `agent_id` is
/// provided, because the local control surface is treated as a trusted
/// operator boundary (see `docs/runtime-spec.md` and issue #1742).
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentGetArgs {
    #[serde(default)]
    pub(crate) agent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<AgentGetArgs>(NAME, include_str!("../tool_descriptions/agent_get.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: AgentGetArgs = parse_tool_args(NAME, input)?;
    let summary = match args.agent_id.as_deref() {
        None => runtime.agent_summary().await?,
        Some(target) => resolve_sibling_agent_summary(runtime, target).await?,
    };
    serialize_success(NAME, &AgentGetResult { agent: summary })
}

async fn resolve_sibling_agent_summary(
    runtime: &RuntimeHandle,
    target_agent_id: &str,
) -> Result<crate::types::AgentSummary> {
    let bridge = runtime.host_bridge().ok_or_else(|| {
        anyhow::Error::from(
            ToolError::new(
                "host_unavailable",
                "AgentGet(agent_id) requires a hosted runtime",
            )
            .with_recovery_hint(
                "invoke AgentGet without agent_id from a `run once` runtime, or attach a host to resolve sibling agents",
            ),
        )
    })?;
    let target_runtime = bridge
        .get_active_agent_for_local_operator(target_agent_id)
        .await
        .map_err(map_public_agent_error_to_tool_error)?;
    target_runtime.agent_summary().await
}

fn map_public_agent_error_to_tool_error(err: PublicAgentError) -> anyhow::Error {
    let tool_error = match err {
        PublicAgentError::NotFound { agent_id } => ToolError::new(
            "agent_not_found",
            format!("agent {agent_id} not found"),
        )
        .with_recovery_hint(
            "create it first with `holon agent create <id>`, or pass a known agent_id",
        ),
        PublicAgentError::Archived { agent_id } => {
            ToolError::new("agent_archived", format!("agent {agent_id} is archived"))
        }
        PublicAgentError::Stopped { agent_id } => ToolError::new(
            "agent_stopped",
            format!("agent {agent_id} is stopped; start first"),
        )
        .with_recovery_hint(
            "call /agents/{id}/start on the host, or pass an active agent_id",
        ),
        PublicAgentError::Private { agent_id } => ToolError::new(
            "host_unexpected_private",
            format!(
                "agent {agent_id} is private (visibility gate should not have triggered from the local operator path)"
            ),
        )
        .with_recovery_hint("this indicates a host bug; report it"),
        PublicAgentError::Runtime(error) => ToolError::from_anyhow(&error),
    };
    anyhow::Error::from(tool_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_get_args_default_to_no_agent_id() {
        // The no-arg `{}` shape continues to parse cleanly with
        // agent_id == None, preserving the original default behavior.
        let args: AgentGetArgs = serde_json::from_value(json!({})).expect("empty input");
        assert!(args.agent_id.is_none());

        // Explicit `agent_id` parses cleanly.
        let args_explicit: AgentGetArgs =
            serde_json::from_value(json!({"agent_id": "sibling"})).expect("explicit input");
        assert_eq!(args_explicit.agent_id.as_deref(), Some("sibling"));
    }
}
