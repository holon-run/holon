use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::SWITCH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchWorkspaceArgs {
    pub workspace_id: Option<String>,
    pub execution_root_id: Option<String>,
    pub path: Option<String>,
    pub cwd: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<SwitchWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/switch_workspace.md"),
        )?,
    })
}
