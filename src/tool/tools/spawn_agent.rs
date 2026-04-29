use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    host_registry::validate_agent_id_format,
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AgentProfilePreset, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{
    invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
};

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
    pub(crate) summary: String,
    pub(crate) prompt: String,
    pub(crate) preset: Option<SpawnAgentPreset>,
    pub(crate) agent_id: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) workspace_mode: Option<SpawnAgentWorkspaceMode>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<SpawnAgentArgs>(
            NAME,
            "Spawn a delegated agent from a small preset surface. The default `private_child` preset returns `agent_id` plus a supervising `task_handle`; `public_named` requires `agent_id` and returns only `agent_id`.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: SpawnAgentArgs = parse_tool_args(NAME, input)?;
    let summary = validate_non_empty(args.summary, NAME, "summary")?;
    let prompt = validate_non_empty(args.prompt, NAME, "prompt")?;
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
        .spawn_agent(
            summary,
            prompt,
            trust.clone(),
            preset,
            agent_id,
            worktree,
            template,
        )
        .await?;
    serialize_success(NAME, &result)
}
