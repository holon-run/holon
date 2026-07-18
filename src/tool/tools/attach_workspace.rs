use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::ATTACH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachWorkspaceArgs {
    pub(crate) path: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AuthorityExpanding,
        spec: typed_spec::<AttachWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/attach_workspace.md"),
        )?,
    })
}

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
