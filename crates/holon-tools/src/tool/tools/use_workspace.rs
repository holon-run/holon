use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::USE_WORKSPACE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum UseWorkspaceModeArgs {
    Direct,
    Isolated,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseWorkspaceArgs {
    pub path: Option<String>,
    pub workspace_id: Option<String>,
    pub mode: Option<UseWorkspaceModeArgs>,
    pub cwd: Option<String>,
    pub isolation_label: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<UseWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/use_workspace.md"),
        )?,
    })
}

pub fn mode_arg_label(mode: UseWorkspaceModeArgs) -> &'static str {
    match mode {
        UseWorkspaceModeArgs::Direct => "direct",
        UseWorkspaceModeArgs::Isolated => "isolated",
    }
}
