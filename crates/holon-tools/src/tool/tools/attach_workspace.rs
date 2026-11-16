use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::ATTACH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachWorkspaceArgs {
    pub path: String,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AuthorityExpanding,
        spec: typed_spec::<AttachWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/attach_workspace.md"),
        )?,
    })
}
