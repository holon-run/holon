use super::success_from_value;
use crate::host_registry::validate_agent_id_format;
use crate::runtime::RuntimeHandle;
use crate::runtime_error::describe_runtime_error;
use crate::tool::error::ToolError;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::InvokeAgentRequest;
use crate::types::InvokeAgentTarget;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::invoke_agent::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _caller_agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    reject_legacy_agent_invocation_input(input)?;
    let args: InvokeAgentArgs = parse_tool_args(NAME, input)?;
    let initial_message = validate_non_empty(args.initial_message, NAME, "initial_message")?;
    let (target, execution_driver) = match args.target {
        InvokeAgentToolTarget::ExistingAgent {
            agent_id,
            reply_timeout_ms,
        } => {
            let agent_id = validate_non_empty(agent_id, NAME, "target.agent_id")?;
            if let Err(error) = validate_agent_id_format(&agent_id) {
                return Err(invalid_tool_input(
                    NAME,
                    format!("InvokeAgent requires a valid target `agent_id`: {error}"),
                    serde_json::json!({
                        "field": "target.agent_id",
                        "validation_error": error.to_string(),
                    }),
                    "provide a valid existing agent id",
                ));
            }
            (
                InvokeAgentTarget::ExistingAgent {
                    agent_id,
                    reply_timeout_ms,
                },
                None,
            )
        }
        InvokeAgentToolTarget::NewSubagent {
            template,
            workspace_mode,
            model,
            execution_driver,
        } => (
            InvokeAgentTarget::NewSubagent {
                template: normalize_optional_non_empty(template),
                workspace_mode,
                model_resolution: Some(runtime.resolve_agent_model_request(NAME, model).await?),
            },
            execution_driver
                .map(execution_driver_from_tool)
                .transpose()?,
        ),
    };
    let result = runtime
        .agent_invocation_service()
        .invoke_with_execution_driver(
            InvokeAgentRequest {
                target,
                message: initial_message,
                authority_class: authority_class.clone(),
            },
            execution_driver,
        )
        .await
        .map_err(map_invocation_error)?;
    let mut value = serde_json::to_value(&result)?;
    value["summary_text"] = Value::String(format!(
        "invoked agent {}; task_id={}",
        result.agent_id, result.task_handle.task_id
    ));
    Ok(success_from_value(NAME, value))
}

fn execution_driver_from_tool(
    driver: InvokeAgentExecutionDriver,
) -> Result<crate::types::AgentExecutionDriver> {
    let kind = crate::types::AgentExecutionDriverKind::from_id(&driver.kind).ok_or_else(|| {
        invalid_tool_input(
            NAME,
            format!(
                "InvokeAgent execution driver `{}` is not supported",
                driver.kind
            ),
            serde_json::json!({ "field": "target.execution_driver.kind" }),
            "use kind=native or kind=acp",
        )
    })?;
    Ok(crate::types::AgentExecutionDriver {
        kind,
        harness_profile: normalize_optional_non_empty(driver.harness_profile),
        role: normalize_optional_non_empty(driver.role),
    })
}

fn map_invocation_error(error: anyhow::Error) -> anyhow::Error {
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
