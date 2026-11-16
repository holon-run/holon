use super::serialize_success;
use crate::runtime::workspace_control::ExistingWorktreePolicy;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::create_worktree::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateWorktreeArgs = parse_tool_args(NAME, input)?;
    let workspace_id = validate_non_empty(args.workspace_id, NAME, "workspace_id")?;
    let branch = validate_non_empty(args.branch, NAME, "branch")?;
    let base_ref = validate_non_empty(args.base_ref, NAME, "base_ref")?;
    let on_existing = match args
        .on_existing
        .unwrap_or(ExistingWorktreePolicyArgs::Reuse)
    {
        ExistingWorktreePolicyArgs::Reuse => ExistingWorktreePolicy::Reuse,
        ExistingWorktreePolicyArgs::Error => ExistingWorktreePolicy::Error,
    };
    serialize_success(
        NAME,
        &runtime
            .create_worktree_for_workspace(
                &workspace_id,
                &branch,
                &base_ref,
                normalize_optional_non_empty(args.label).as_deref(),
                args.activate,
                on_existing,
            )
            .await?,
    )
}
