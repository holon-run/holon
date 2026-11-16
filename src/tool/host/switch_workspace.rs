use super::serialize_success;
use crate::runtime::workspace_control::WorkspaceSwitchTarget;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;
use std::path::PathBuf;

pub(crate) use holon_tools::builtins::switch_workspace::*;

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
