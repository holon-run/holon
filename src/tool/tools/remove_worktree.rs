use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::{workspace_control::WorktreeBranchPolicy, RuntimeHandle},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty};

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
    pub(crate) execution_root_id: String,
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

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: RemoveWorktreeArgs = parse_tool_args(NAME, input)?;
    let execution_root_id = validate_non_empty(args.execution_root_id, NAME, "execution_root_id")?;
    let branch_policy = match args.branch_policy.unwrap_or(WorktreeBranchPolicyArgs::Keep) {
        WorktreeBranchPolicyArgs::Keep => WorktreeBranchPolicy::Keep,
        WorktreeBranchPolicyArgs::DeleteIfMerged => WorktreeBranchPolicy::DeleteIfMerged,
    };
    let return_to = normalize_optional_non_empty(args.return_to);
    let merged_into = normalize_optional_non_empty(args.merged_into);
    serialize_success(
        NAME,
        &runtime
            .remove_registered_worktree(
                &execution_root_id,
                return_to.as_deref(),
                branch_policy,
                merged_into.as_deref(),
            )
            .await?,
    )
}
