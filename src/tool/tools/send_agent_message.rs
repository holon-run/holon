use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    host_registry::validate_agent_id_format,
    runtime::RuntimeHandle,
    runtime_error::describe_runtime_error,
    tool::{error::ToolError, spec::typed_spec},
    types::{AgentMessageSendRequest, AuthorityClass, MessageBody, Priority, ToolCapabilityFamily},
};

use super::{success_from_value, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::SEND_AGENT_MESSAGE;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendAgentMessageArgs {
    pub agent_id: String,
    pub message: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<SendAgentMessageArgs>(
            NAME,
            include_str!("../tool_descriptions/send_agent_message.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    authority_class: &AuthorityClass,
    tool_call_id: &str,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: SendAgentMessageArgs = parse_tool_args(NAME, input)?;
    let agent_id = validate_non_empty(args.agent_id, NAME, "agent_id")?;
    if let Err(error) = validate_agent_id_format(&agent_id) {
        return Err(invalid_tool_input(
            NAME,
            format!("SendAgentMessage requires a valid `agent_id`: {error}"),
            serde_json::json!({
                "field": "agent_id",
                "validation_error": error.to_string(),
            }),
            "provide a valid existing agent id",
        ));
    }
    let message = validate_non_empty(args.message, NAME, "message")?;
    let receipt = runtime
        .agent_messaging_service()
        .send(
            AgentMessageSendRequest {
                target_agent_id: agent_id,
                content: MessageBody::Text { text: message },
                client_idempotency_key: tool_call_id.to_string(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Normal),
            },
            authority_class.clone(),
        )
        .await
        .map_err(map_send_error)?;
    let mut value = serde_json::to_value(&receipt)?;
    value["summary_text"] = Value::String(format!(
        "accepted message delivery {} for agent {}",
        receipt.delivery_id, receipt.target_agent_id
    ));
    Ok(success_from_value(NAME, value))
}

fn map_send_error(error: anyhow::Error) -> anyhow::Error {
    let descriptor = describe_runtime_error(&error);
    if descriptor.code == "agent_target_unavailable" {
        return anyhow::Error::from(
            ToolError::new(
                "not_found",
                "agent target was not found or is not available to this caller",
            )
            .with_domain(crate::runtime_error::RuntimeErrorDomain::NotFound)
            .with_recovery_hint(
                "use an agent id already available through the caller's authorized agent context",
            ),
        );
    }
    error
}
