use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::DETACH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DetachWorkspaceArgs {
    pub workspace_id: String,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AuthorityExpanding,
        spec: typed_spec::<DetachWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/detach_workspace.md"),
        )?,
    })
}
