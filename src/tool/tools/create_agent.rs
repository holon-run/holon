use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    host_registry::validate_agent_id_format,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, CreateAgentRequest, SpawnAgentModelRequest, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{
    invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
};

pub(crate) const NAME: &str = crate::tool::names::CREATE_AGENT;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateAgentArgs {
    pub(crate) agent_id: String,
    pub(crate) name: Option<String>,
    pub(crate) initial_message: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) model: Option<SpawnAgentModelRequest>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<CreateAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/create_agent.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _caller_agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateAgentArgs = parse_tool_args(NAME, input)?;
    let agent_id = validate_non_empty(args.agent_id, NAME, "agent_id")?;
    if let Err(error) = validate_agent_id_format(&agent_id) {
        return Err(invalid_tool_input(
            NAME,
            format!("CreateAgent requires a valid `agent_id`: {error}"),
            json!({
                "field": "agent_id",
                "validation_error": error.to_string(),
            }),
            "use a single ASCII agent id containing only letters, digits, '.', '-', or '_'",
        ));
    }
    let model_resolution = runtime
        .resolve_agent_model_request(NAME, args.model)
        .await?;
    let result = runtime
        .agent_creation_service()
        .create(CreateAgentRequest {
            agent_id,
            name: normalize_optional_non_empty(args.name),
            template: normalize_optional_non_empty(args.template),
            initial_message: normalize_optional_non_empty(args.initial_message),
            authority_class: authority_class.clone(),
            model_resolution: Some(model_resolution),
            lineage_parent_agent_id: None,
            inherit_parent_runtime: true,
        })
        .await?;
    serialize_success(NAME, &result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tool::helpers::parse_tool_args;

    use super::{CreateAgentArgs, NAME};

    #[test]
    fn create_agent_rejects_caller_controlled_provenance() {
        let error = parse_tool_args::<CreateAgentArgs>(
            NAME,
            &json!({
                "agent_id": "release-bot",
                "authority_class": "operator",
                "lineage_parent_agent_id": "root-agent"
            }),
        )
        .expect_err("trusted caller context must not be request-controlled");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn create_agent_rejects_unknown_nested_model_fields() {
        let error = parse_tool_args::<CreateAgentArgs>(
            NAME,
            &json!({
                "agent_id": "release-bot",
                "model": {
                    "provider": "anthropic",
                    "model": "claude-haiku-4-5",
                    "max_output_token": 1000
                }
            }),
        )
        .expect_err("nested model typos should be rejected");

        assert!(error.to_string().contains("max_output_token"));
    }
}
