use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{
    runtime::RuntimeHandle,
    system::{
        workspace_access_mode_kind_label, workspace_projection_kind_label, WorkspaceAccessMode,
        WorkspaceProjectionKind,
    },
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel, UseWorkspaceResult, AGENT_HOME_WORKSPACE_ID},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{
    invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
};

pub(crate) const NAME: &str = "UseWorkspace";

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum UseWorkspaceModeArgs {
    Direct,
    Isolated,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum UseWorkspaceAccessModeArgs {
    SharedRead,
    ExclusiveWrite,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UseWorkspaceArgs {
    pub(crate) path: Option<String>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) mode: Option<UseWorkspaceModeArgs>,
    pub(crate) access_mode: Option<UseWorkspaceAccessModeArgs>,
    pub(crate) cwd: Option<String>,
    pub(crate) isolation_label: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<UseWorkspaceArgs>(
            NAME,
            "Make a workspace active. Provide exactly one of `path` or `workspace_id`: use `path` to discover, attach, and activate a project directory; use `workspace_id` to switch to a known workspace, including `agent_home` for the built-in fallback workspace. Shell `cd` does not change this active workspace for ApplyPatch or future commands.",
        )?,
    })
}

fn mode_arg_label(mode: UseWorkspaceModeArgs) -> &'static str {
    match mode {
        UseWorkspaceModeArgs::Direct => "direct",
        UseWorkspaceModeArgs::Isolated => "isolated",
    }
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: UseWorkspaceArgs = parse_tool_args(NAME, input)?;
    let path = normalize_optional_non_empty(args.path);
    let workspace_id = normalize_optional_non_empty(args.workspace_id);
    if path.is_some() == workspace_id.is_some() {
        return Err(invalid_tool_input(
            NAME,
            "UseWorkspace requires exactly one of `path` or `workspace_id`",
            json!({
                "fields": ["path", "workspace_id"],
                "validation_error": "mutually exclusive workspace selector required",
            }),
            "call UseWorkspace with either {\"path\":\"/repo\"} or {\"workspace_id\":\"agent_home\"}, not both",
        ));
    }

    let mode = args.mode.unwrap_or(UseWorkspaceModeArgs::Direct);
    let projection_kind = match mode {
        UseWorkspaceModeArgs::Direct => WorkspaceProjectionKind::CanonicalRoot,
        UseWorkspaceModeArgs::Isolated => WorkspaceProjectionKind::GitWorktreeRoot,
    };
    let access_mode = match args
        .access_mode
        .unwrap_or(UseWorkspaceAccessModeArgs::SharedRead)
    {
        UseWorkspaceAccessModeArgs::SharedRead => WorkspaceAccessMode::SharedRead,
        UseWorkspaceAccessModeArgs::ExclusiveWrite => WorkspaceAccessMode::ExclusiveWrite,
    };
    let cwd = normalize_optional_non_empty(args.cwd).map(PathBuf::from);
    let branch_name = match projection_kind {
        WorkspaceProjectionKind::CanonicalRoot => None,
        WorkspaceProjectionKind::GitWorktreeRoot => Some(
            normalize_optional_non_empty(args.isolation_label)
                .unwrap_or_else(|| "workspace".into()),
        ),
    };

    if let Some(workspace_id) = workspace_id {
        let workspace_id = validate_non_empty(workspace_id, NAME, "workspace_id")?;
        if workspace_id == AGENT_HOME_WORKSPACE_ID {
            if projection_kind != WorkspaceProjectionKind::CanonicalRoot {
                return Err(invalid_tool_input(
                    NAME,
                    "AgentHome can only be activated in direct mode",
                    json!({
                        "workspace_id": workspace_id,
                        "mode": mode_arg_label(mode),
                    }),
                    "call UseWorkspace with {\"workspace_id\":\"agent_home\"} and omit `mode`",
                ));
            }
            runtime.activate_agent_home(access_mode, cwd).await?;
        } else {
            let workspace = runtime
                .workspace_entry_for_use(&workspace_id)
                .await?
                .ok_or_else(|| {
                    invalid_tool_input(
                        NAME,
                        format!("workspace `{workspace_id}` was not found"),
                        json!({
                            "field": "workspace_id",
                            "workspace_id": workspace_id,
                            "validation_error": "workspace not found",
                        }),
                        "inspect the current agent state for attached workspace ids, or call UseWorkspace with a path",
                    )
                })?;
            runtime
                .enter_workspace(&workspace, projection_kind, access_mode, cwd, branch_name)
                .await?;
        }
    } else if let Some(path) = path {
        let path = validate_non_empty(path, NAME, "path")?;
        let workspace = runtime
            .ensure_workspace_entry_for_path(PathBuf::from(&path))
            .await?;
        runtime.attach_workspace(&workspace).await?;
        runtime
            .enter_workspace(&workspace, projection_kind, access_mode, cwd, branch_name)
            .await?;
    }

    let snapshot = runtime.execution_snapshot().await?;
    let mode_label = mode_arg_label(mode);
    let projection_kind_label = workspace_projection_kind_label(projection_kind);
    let access_mode_label = workspace_access_mode_kind_label(access_mode);
    serialize_success(
        NAME,
        &UseWorkspaceResult {
            workspace_id: snapshot
                .workspace_id
                .unwrap_or_else(|| AGENT_HOME_WORKSPACE_ID.to_string()),
            workspace_anchor: snapshot.workspace_anchor,
            execution_root: snapshot.execution_root,
            cwd: snapshot.cwd,
            mode: mode_label.to_string(),
            projection_kind: projection_kind_label.to_string(),
            access_mode: access_mode_label.to_string(),
            summary_text: Some(format!(
                "using workspace with {mode_label} mode, {projection_kind_label} projection, and {access_mode_label}"
            )),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_workspace_rejects_both_selectors() {
        let error = parse_tool_args::<UseWorkspaceArgs>(
            NAME,
            &serde_json::json!({
                "path": "/repo",
                "workspace_id": "agent_home"
            }),
        )
        .and_then(|args| {
            let path = normalize_optional_non_empty(args.path);
            let workspace_id = normalize_optional_non_empty(args.workspace_id);
            if path.is_some() == workspace_id.is_some() {
                return Err(invalid_tool_input(
                    NAME,
                    "UseWorkspace requires exactly one of `path` or `workspace_id`",
                    json!({}),
                    "call UseWorkspace with either path or workspace_id",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        let tool_error = crate::tool::ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
    }

    #[test]
    fn use_workspace_schema_exposes_path_and_workspace_id() {
        let spec = definition().unwrap().spec;
        assert!(spec.input_schema["properties"]["path"].is_object());
        assert!(spec.input_schema["properties"]["workspace_id"].is_object());
    }
}
