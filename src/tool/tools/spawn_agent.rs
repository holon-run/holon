use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    host_registry::validate_agent_id_format,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AgentProfilePreset, AuthorityClass, SpawnAgentModelRequest, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, normalize_optional_non_empty, parse_tool_args};

pub(crate) const NAME: &str = "SpawnAgent";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum SpawnAgentPreset {
    PrivateChild,
    PublicNamed,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum SpawnAgentWorkspaceMode {
    Inherit,
    Worktree,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpawnAgentArgs {
    pub(crate) initial_message: Option<String>,
    pub(crate) preset: Option<SpawnAgentPreset>,
    pub(crate) agent_id: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) workspace_mode: Option<SpawnAgentWorkspaceMode>,
    pub(crate) model: Option<SpawnAgentModelRequest>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<SpawnAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/spawn_agent.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: SpawnAgentArgs = parse_tool_args(NAME, input)?;
    let initial_message = normalize_optional_non_empty(args.initial_message);
    let worktree = matches!(
        args.workspace_mode
            .unwrap_or(SpawnAgentWorkspaceMode::Inherit),
        SpawnAgentWorkspaceMode::Worktree
    );
    let preset = match args.preset.unwrap_or(SpawnAgentPreset::PrivateChild) {
        SpawnAgentPreset::PrivateChild => AgentProfilePreset::PrivateChild,
        SpawnAgentPreset::PublicNamed => AgentProfilePreset::PublicNamed,
    };
    let agent_id = normalize_optional_non_empty(args.agent_id);
    let template = normalize_optional_non_empty(args.template);

    match preset {
        AgentProfilePreset::PrivateChild => {
            if initial_message.is_none() {
                return Err(invalid_tool_input(
                    NAME,
                    "SpawnAgent `private_child` requires a non-empty `initial_message`",
                    json!({
                        "field": "initial_message",
                        "preset": "private_child",
                        "validation_error": "must not be empty",
                    }),
                    "provide the delegation message in `initial_message` when using `private_child`",
                ));
            }
            if agent_id.is_some() {
                return Err(invalid_tool_input(
                    NAME,
                    "SpawnAgent `private_child` does not accept `agent_id`",
                    json!({
                        "field": "agent_id",
                        "preset": "private_child",
                        "validation_error": "unexpected field for preset",
                    }),
                    "omit `agent_id` when using the default `private_child` preset",
                ));
            }
        }
        AgentProfilePreset::PublicNamed => {
            let Some(agent_id) = agent_id.as_deref() else {
                return Err(invalid_tool_input(
                    NAME,
                    "SpawnAgent `public_named` requires a non-empty `agent_id`",
                    json!({
                        "field": "agent_id",
                        "preset": "public_named",
                        "validation_error": "must not be empty",
                    }),
                    "provide a stable public agent id when using the `public_named` preset",
                ));
            };
            if let Err(error) = validate_agent_id_format(agent_id) {
                return Err(invalid_tool_input(
                    NAME,
                    format!("SpawnAgent `public_named` requires a valid `agent_id`: {error}"),
                    json!({
                        "field": "agent_id",
                        "preset": "public_named",
                        "validation_error": error.to_string(),
                    }),
                    "use a single ASCII agent id like `release-bot` containing only letters, digits, '.', '-', or '_'",
                ));
            }
            if worktree {
                return Err(invalid_tool_input(
                    NAME,
                    "SpawnAgent `public_named` does not support `workspace_mode=worktree`",
                    json!({
                        "field": "workspace_mode",
                        "preset": "public_named",
                        "validation_error": "unsupported value for preset",
                    }),
                    "use inherited workspace mode for `public_named`, or use the default `private_child` preset for worktree-isolated delegation",
                ));
            }
        }
    }

    let result = runtime
        .managed_tasks()
        .spawn_agent(
            initial_message,
            authority_class.clone(),
            preset,
            agent_id,
            worktree,
            template,
            args.model,
        )
        .await?;
    serialize_success(NAME, &result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tool::helpers::parse_tool_args;

    use super::{SpawnAgentArgs, NAME};

    #[test]
    fn spawn_agent_rejects_unknown_nested_model_fields() {
        let result = parse_tool_args::<SpawnAgentArgs>(
            NAME,
            &json!({
                "initial_message": "compare implementation",
                "model": {
                    "provider": "anthropic",
                    "model": "claude-haiku-4-5",
                    "max_output_token": 1000
                }
            }),
        );
        let error = result.err().expect("nested model typos should be rejected");

        assert!(error.to_string().contains("max_output_token"));
    }
}
