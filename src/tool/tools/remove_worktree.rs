use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{
    runtime::{workspace_control::WorktreeBranchPolicy, RuntimeHandle},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, normalize_optional_non_empty, parse_tool_args};

pub(crate) const NAME: &str = crate::tool::names::REMOVE_WORKTREE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeBranchPolicyArgs {
    Keep,
    DeleteIfMerged,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoveWorktreeArgs {
    pub(crate) execution_root_id: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) return_to: Option<String>,
    pub(crate) branch_policy: Option<WorktreeBranchPolicyArgs>,
    pub(crate) merged_into: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AuthorityExpanding,
        spec: typed_spec::<RemoveWorktreeArgs>(
            NAME,
            include_str!("../tool_descriptions/remove_worktree.md"),
        )?,
    })
}

fn validate_path_selector(path: PathBuf) -> Result<PathBuf> {
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

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: RemoveWorktreeArgs = parse_tool_args(NAME, input)?;
    let execution_root_id = normalize_optional_non_empty(args.execution_root_id);
    let path = normalize_optional_non_empty(args.path);
    let selector_count = usize::from(execution_root_id.is_some()) + usize::from(path.is_some());
    if selector_count != 1 {
        return Err(invalid_tool_input(
            NAME,
            "RemoveWorktree requires exactly one selector",
            json!({
                "fields": ["execution_root_id", "path"],
                "selector_count": selector_count,
            }),
            "provide exactly one of `execution_root_id` or `path`",
        ));
    }
    let path = path
        .map(PathBuf::from)
        .map(validate_path_selector)
        .transpose()?;
    let branch_policy = match args.branch_policy.unwrap_or(WorktreeBranchPolicyArgs::Keep) {
        WorktreeBranchPolicyArgs::Keep => WorktreeBranchPolicy::Keep,
        WorktreeBranchPolicyArgs::DeleteIfMerged => WorktreeBranchPolicy::DeleteIfMerged,
    };
    let return_to = normalize_optional_non_empty(args.return_to);
    let merged_into = normalize_optional_non_empty(args.merged_into);
    let removal = match (execution_root_id.as_deref(), path.as_deref()) {
        (Some(execution_root_id), None) => {
            runtime
                .remove_registered_worktree(
                    execution_root_id,
                    return_to.as_deref(),
                    branch_policy,
                    merged_into.as_deref(),
                )
                .await?
        }
        (None, Some(path)) => {
            runtime
                .remove_registered_worktree_selector(
                    None,
                    Some(path),
                    return_to.as_deref(),
                    branch_policy,
                    merged_into.as_deref(),
                )
                .await?
        }
        _ => unreachable!("selector count was validated above"),
    };
    serialize_success(NAME, &removal)
}

#[cfg(test)]
mod tests {
    use super::validate_path_selector;

    #[test]
    fn path_selector_requires_an_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, "content").unwrap();
        let missing = temp.path().join("missing");

        assert!(validate_path_selector(temp.path().to_path_buf()).is_ok());
        assert!(validate_path_selector(file).is_err());
        assert!(validate_path_selector(missing).is_err());
    }
}
