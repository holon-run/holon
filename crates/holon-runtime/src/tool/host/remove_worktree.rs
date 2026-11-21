use super::serialize_success;
use crate::runtime::workspace_control::WorktreeBranchPolicy;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;
use std::path::PathBuf;

pub(crate) use holon_tools::builtins::remove_worktree::*;

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
