use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;

pub(crate) use holon_tools::builtins::attach_workspace::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: AttachWorkspaceArgs = parse_tool_args(NAME, input)?;
    let path = validate_non_empty(args.path, NAME, "path")?;
    let path = PathBuf::from(path);
    if !path.try_exists()? || !path.is_dir() {
        return Err(invalid_tool_input(
            NAME,
            format!(
                "workspace path is not an existing directory: {}",
                path.display()
            ),
            serde_json::json!({ "path": path }),
            "provide an existing repository or directory path",
        ));
    }
    serialize_success(NAME, &runtime.attach_workspace_path(path).await?)
}
