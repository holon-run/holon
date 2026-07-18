use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::{workspace_control::ExistingWorktreePolicy, RuntimeHandle},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::CREATE_WORKTREE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExistingWorktreePolicyArgs {
    Reuse,
    Error,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateWorktreeArgs {
    pub(crate) workspace_id: String,
    pub(crate) branch: String,
    pub(crate) base_ref: String,
    pub(crate) label: Option<String>,
    #[serde(default = "default_true")]
    pub(crate) activate: bool,
    pub(crate) on_existing: Option<ExistingWorktreePolicyArgs>,
}

fn default_true() -> bool {
    true
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<CreateWorktreeArgs>(
            NAME,
            include_str!("../tool_descriptions/create_worktree.md"),
        )?,
    })
}

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
