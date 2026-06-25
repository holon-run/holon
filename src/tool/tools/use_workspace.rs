use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{
    runtime::RuntimeHandle,
    system::{workspace_projection_kind_label, WorkspaceAccessMode, WorkspaceProjectionKind},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily, UseWorkspaceResult, AGENT_HOME_WORKSPACE_ID},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{
    invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
};

pub(crate) const NAME: &str = crate::tool::names::USE_WORKSPACE;

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum UseWorkspaceModeArgs {
    Direct,
    Isolated,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UseWorkspaceArgs {
    pub(crate) path: Option<String>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) mode: Option<UseWorkspaceModeArgs>,
    pub(crate) cwd: Option<String>,
    pub(crate) isolation_label: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<UseWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/use_workspace.md"),
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
    _authority_class: &AuthorityClass,
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
    let access_mode = match projection_kind {
        WorkspaceProjectionKind::CanonicalRoot => WorkspaceAccessMode::SharedRead,
        WorkspaceProjectionKind::GitWorktreeRoot => WorkspaceAccessMode::ExclusiveWrite,
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
        let path = PathBuf::from(&path);
        // Normalize and reject nonexistent paths before any state mutation.
        let normalized = crate::tool::helpers::normalize_path(&path)?;
        if !normalized.try_exists()? {
            return Err(invalid_tool_input(
                NAME,
                format!("path does not exist: {}", normalized.display()),
                json!({
                    "field": "path",
                    "path": normalized.display().to_string(),
                    "validation_error": "path does not exist",
                }),
                "call UseWorkspace with an existing directory path",
            ));
        }
        if !normalized.is_dir() {
            return Err(invalid_tool_input(
                NAME,
                format!("path is not a directory: {}", normalized.display()),
                json!({
                    "field": "path",
                    "path": normalized.display().to_string(),
                    "validation_error": "path is not a directory",
                }),
                "call UseWorkspace with an existing directory path",
            ));
        }
        if projection_kind == WorkspaceProjectionKind::CanonicalRoot {
            if let Some(existing_worktree) = runtime
                .attached_workspace_for_existing_git_worktree(&path)
                .await?
            {
                let default_cwd = crate::system::workspace::normalize_path(&path)?;
                runtime
                    .enter_existing_git_worktree(
                        &existing_worktree.workspace,
                        existing_worktree.worktree_root,
                        access_mode,
                        cwd.or(Some(default_cwd)),
                    )
                    .await?;
                let snapshot = runtime.execution_snapshot().await?;
                let projection_kind_label = workspace_projection_kind_label(
                    snapshot
                        .projection_kind
                        .unwrap_or(WorkspaceProjectionKind::GitWorktreeRoot),
                );
                let isolation_label = existing_worktree
                    .suggested_isolation_label
                    .unwrap_or_else(|| "worktree".into());
                return serialize_success(
                    NAME,
                    &UseWorkspaceResult {
                        workspace_id: snapshot
                            .workspace_id
                            .unwrap_or_else(|| AGENT_HOME_WORKSPACE_ID.to_string()),
                        workspace_anchor: snapshot.workspace_anchor,
                        execution_root: snapshot.execution_root,
                        cwd: snapshot.cwd,
                        mode: mode_arg_label(mode).to_string(),
                        projection_kind: projection_kind_label.to_string(),
                        summary_text: Some(format!(
                            "detected an existing git worktree for workspace {}; using it as an external execution root. Prefer UseWorkspace with {{\"workspace_id\":\"{}\",\"mode\":\"isolated\",\"isolation_label\":\"{}\"}} so the runtime manages lifecycle.",
                            existing_worktree.workspace.workspace_id,
                            existing_worktree.workspace.workspace_id,
                            isolation_label
                        )),
                    },
                );
            }
        }
        let workspace = runtime.ensure_workspace_entry_for_path(path).await?;
        runtime.attach_workspace(&workspace).await?;
        runtime
            .enter_workspace(&workspace, projection_kind, access_mode, cwd, branch_name)
            .await?;
    }

    let snapshot = runtime.execution_snapshot().await?;
    let mode_label = mode_arg_label(mode);
    let projection_kind_label = workspace_projection_kind_label(projection_kind);
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
            summary_text: Some(format!(
                "using workspace with {mode_label} mode and {projection_kind_label} projection"
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
        let properties = spec.input_schema["properties"].as_object().unwrap();
        assert!(properties["path"].is_object());
        assert!(properties["workspace_id"].is_object());
        assert!(!properties.contains_key("access_mode"));
    }
}
