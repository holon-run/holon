use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

pub const NAME: &str = crate::tool::names::REMOVE_WORKTREE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeBranchPolicyArgs {
    Keep,
    DeleteIfMerged,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveWorktreeArgs {
    pub execution_root_id: Option<String>,
    pub path: Option<String>,
    pub return_to: Option<String>,
    pub branch_policy: Option<WorktreeBranchPolicyArgs>,
    pub merged_into: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<RemoveWorktreeArgs>(
            NAME,
            include_str!("../tool_descriptions/remove_worktree.md"),
        )?,
    })
}

pub fn validate_path_selector(path: PathBuf) -> Result<PathBuf> {
    if !path.try_exists()? || !path.is_dir() {
        return Err(invalid_tool_input(
            NAME,
            format!(
                "worktree path is not an existing directory: {}",
                path.display()
            ),
            json!({ "path": path }),
            "provide an existing directory inside a registered worktree",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
