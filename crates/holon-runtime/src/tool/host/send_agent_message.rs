use super::success_from_value;
use crate::host_registry::validate_agent_id_format;
use crate::runtime::RuntimeHandle;
use crate::runtime_error::describe_runtime_error;
use crate::tool::error::ToolError;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AgentMessageSendRequest;
use crate::types::AuthorityClass;
use crate::types::MessageBody;
use crate::types::Priority;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::send_agent_message::*;

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
    let reply_context_id = args
        .reply_context_id
        .map(|id| validate_non_empty(id, NAME, "reply_context_id"))
        .transpose()?;
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
                forward: args.forward,
                reply_context_id,
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
