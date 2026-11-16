use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::AgentModelRequest;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::CREATE_AGENT;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentArgs {
    pub agent_id: String,
    pub name: Option<String>,
    pub initial_message: Option<String>,
    pub template: Option<String>,
    pub model: Option<AgentModelRequest>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<CreateAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/create_agent.md"),
        )?,
    })
}

pub fn reject_legacy_agent_creation_input(input: &Value) -> Result<()> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    let Some(field) = [
        "kind",
        "visibility",
        "ownership",
        "profile_preset",
        "preset",
    ]
    .into_iter()
    .find(|field| object.contains_key(*field)) else {
        return Ok(());
    };
    Err(anyhow::Error::from(
        crate::tool::ToolError::new(
            "legacy_agent_creation_input",
            format!("CreateAgent no longer accepts the legacy `{field}` field"),
        )
        .with_details(json!({
            "tool_name": NAME,
            "field": field,
        }))
        .with_recovery_hint(
            "use CreateAgent for an independent identity or InvokeAgent with target.kind=new_subagent for supervised delegation",
        ),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tool::helpers::parse_tool_args;

    use super::{reject_legacy_agent_creation_input, CreateAgentArgs, NAME};

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

    #[test]
    fn create_agent_rejects_legacy_identity_fields_with_stable_error() {
        let error = reject_legacy_agent_creation_input(&json!({
            "agent_id": "release-bot",
            "profile_preset": "public_named"
        }))
        .expect_err("legacy identity fields should have a stable typed error");

        let tool_error = error
            .downcast_ref::<crate::tool::ToolError>()
            .expect("typed tool error");
        assert_eq!(tool_error.kind, "legacy_agent_creation_input");
    }
}
