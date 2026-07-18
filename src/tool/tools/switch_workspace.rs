use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{
    runtime::{workspace_control::WorkspaceSwitchTarget, RuntimeHandle},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, normalize_optional_non_empty, parse_tool_args};

pub(crate) const NAME: &str = crate::tool::names::SWITCH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SwitchWorkspaceArgs {
    pub(crate) workspace_id: Option<String>,
    pub(crate) execution_root_id: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) cwd: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<SwitchWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/switch_workspace.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: SwitchWorkspaceArgs = parse_tool_args(NAME, input)?;
    let workspace_id = normalize_optional_non_empty(args.workspace_id);
    let execution_root_id = normalize_optional_non_empty(args.execution_root_id);
    let path = normalize_optional_non_empty(args.path);
    let selector_count = usize::from(workspace_id.is_some())
        + usize::from(execution_root_id.is_some())
        + usize::from(path.is_some());
    if selector_count != 1 {
        return Err(invalid_tool_input(
            NAME,
            "SwitchWorkspace requires exactly one selector",
            json!({
                "fields": ["workspace_id", "execution_root_id", "path"],
                "selector_count": selector_count,
            }),
            "provide exactly one of `workspace_id`, `execution_root_id`, or `path`",
        ));
    }
    let target = if let Some(workspace_id) = workspace_id {
        WorkspaceSwitchTarget::WorkspaceId(workspace_id)
    } else if let Some(execution_root_id) = execution_root_id {
        WorkspaceSwitchTarget::ExecutionRootId(execution_root_id)
    } else {
        let path = PathBuf::from(path.expect("selector count checked"));
        if !path.try_exists()? || !path.is_dir() {
            return Err(invalid_tool_input(
                NAME,
                format!(
                    "workspace path is not an existing directory: {}",
                    path.display()
                ),
                json!({ "path": path }),
                "provide an existing path inside an attached workspace or registered worktree",
            ));
        }
        WorkspaceSwitchTarget::Path(path)
    };
    let cwd = normalize_optional_non_empty(args.cwd).map(PathBuf::from);
    serialize_success(NAME, &runtime.switch_workspace_target(target, cwd).await?)
}
