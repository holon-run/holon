use super::serialize_success;
use crate::runtime::workspace_control::ExistingWorktreePolicy;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::{json, Value};

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
    let path = normalize_optional_non_empty(args.path);
    if let Some(value) = &path {
        if value.starts_with('~') || !std::path::Path::new(value).is_absolute() {
            return Err(invalid_tool_input(
                NAME,
                format!("{NAME} `path` must be an absolute filesystem path"),
                json!({
                    "tool_name": NAME,
                    "field": "path",
                    "validation_error": if value.starts_with('~') {
                        "`~` and environment variables are not expanded"
                    } else {
                        "relative paths are not resolved"
                    },
                }),
                "provide an absolute filesystem path for `path`; `~`, environment variables, and relative paths are not resolved".to_string(),
            ));
        }
    }
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
                path.as_deref(),
            )
            .await?,
    )
}
