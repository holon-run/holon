use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    host_registry::validate_agent_id_format,
    runtime::RuntimeHandle,
    runtime_error::describe_runtime_error,
    tool::{error::ToolError, spec::typed_spec},
    types::{
        AuthorityClass, ChildAgentWorkspaceMode, InvokeAgentRequest, InvokeAgentTarget,
        SpawnAgentModelRequest, ToolCapabilityFamily,
    },
};

use super::{success_from_value, BuiltinToolDefinition};
use crate::tool::helpers::{
    invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
};

pub(crate) const NAME: &str = crate::tool::names::INVOKE_AGENT;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InvokeAgentToolTarget {
    ExistingAgent {
        agent_id: String,
    },
    NewSubagent {
        template: Option<String>,
        #[serde(default)]
        workspace_mode: ChildAgentWorkspaceMode,
        model: Option<SpawnAgentModelRequest>,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvokeAgentArgs {
    pub(crate) target: InvokeAgentToolTarget,
    pub(crate) initial_message: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<InvokeAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/invoke_agent.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _caller_agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: InvokeAgentArgs = parse_tool_args(NAME, input)?;
    let initial_message = validate_non_empty(args.initial_message, NAME, "initial_message")?;
    let target = match args.target {
        InvokeAgentToolTarget::ExistingAgent { agent_id } => {
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
            InvokeAgentTarget::ExistingAgent { agent_id }
        }
        InvokeAgentToolTarget::NewSubagent {
            template,
            workspace_mode,
            model,
        } => InvokeAgentTarget::NewSubagent {
            template: normalize_optional_non_empty(template),
            workspace_mode,
            model_resolution: Some(runtime.resolve_agent_model_request(NAME, model).await?),
        },
    };
    let result = runtime
        .agent_invocation_service()
        .invoke(InvokeAgentRequest {
            target,
            message: initial_message,
            authority_class: authority_class.clone(),
        })
        .await
        .map_err(map_invocation_error)?;
    let mut value = serde_json::to_value(&result)?;
    value["summary_text"] = Value::String(format!(
        "invoked agent {}; task_id={}",
        result.agent_id, result.task_handle.task_id
    ));
    Ok(success_from_value(NAME, value))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tool::helpers::parse_tool_args;

    use super::{InvokeAgentArgs, NAME};

    #[test]
    fn invoke_agent_contract_accepts_exactly_the_two_target_variants() {
        parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot"
                },
                "initial_message": "prepare the release"
            }),
        )
        .expect("existing agent target should deserialize");
        parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "new_subagent",
                    "workspace_mode": "worktree"
                },
                "initial_message": "review the implementation"
            }),
        )
        .expect("new subagent target should deserialize");
    }

    #[test]
    fn invoke_agent_rejects_target_field_mixing() {
        let error = parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot",
                    "workspace_mode": "worktree"
                },
                "initial_message": "prepare the release"
            }),
        )
        .expect_err("existing targets must not accept new-subagent options");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn invoke_agent_rejects_caller_controlled_provenance() {
        let error = parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot"
                },
                "initial_message": "prepare the release",
                "authority_class": "operator",
                "origin": "operator"
            }),
        )
        .expect_err("trusted caller context must not be request-controlled");

        assert!(error.to_string().contains("unknown field"));
    }
}
