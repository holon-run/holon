use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::CREATE_WORKTREE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExistingWorktreePolicyArgs {
    Reuse,
    Error,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorktreeArgs {
    pub workspace_id: String,
    pub branch: String,
    pub base_ref: String,
    pub label: Option<String>,
    #[serde(default = "default_true")]
    pub activate: bool,
    pub on_existing: Option<ExistingWorktreePolicyArgs>,
}

fn default_true() -> bool {
    true
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<CreateWorktreeArgs>(
            NAME,
            include_str!("../tool_descriptions/create_worktree.md"),
        )?,
    })
}
