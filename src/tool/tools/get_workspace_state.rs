use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{
        AuthorityClass, ExecutionRootEntry, ToolCapabilityFamily, WorkspaceOccupancyRecord,
        WorkspaceStateResult,
    },
};

use super::{serialize_success, truncate_chars, BuiltinToolDefinition, ToolModelRenderContext};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::GET_WORKSPACE_STATE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetWorkspaceStateArgs {}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<GetWorkspaceStateArgs>(
            NAME,
            include_str!("../tool_descriptions/get_workspace_state.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let _: GetWorkspaceStateArgs = parse_tool_args(NAME, input)?;
    serialize_success(NAME, &runtime.workspace_state_result().await?)
}

const DEFAULT_EXECUTION_ROOT_LIMIT: usize = 10;
const DEFAULT_OCCUPANCY_LIMIT: usize = 20;
const DEFAULT_WORKSPACE_LIMIT: usize = 20;
const STRING_LIMIT: usize = 512;

pub(crate) fn render_for_model(
    result: &crate::tool::ToolResult,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow::anyhow!("GetWorkspaceState result missing payload"))?;
    let state: WorkspaceStateResult = serde_json::from_value(value)?;
    let output_ref = format!("tool_execution:{}:output", context.tool_execution_id);

    let mut root_limit = DEFAULT_EXECUTION_ROOT_LIMIT;
    let mut occupancy_limit = DEFAULT_OCCUPANCY_LIMIT;
    let mut workspace_limit = DEFAULT_WORKSPACE_LIMIT;
    loop {
        let receipt = workspace_state_receipt(
            &state,
            &output_ref,
            root_limit,
            occupancy_limit,
            workspace_limit,
        );
        let rendered = serde_json::to_string(&receipt)?;
        if estimated_tokens(&rendered) <= context.tool_output_budget_estimated_tokens
            || (root_limit == 0 && occupancy_limit == 0 && workspace_limit == 0)
        {
            return Ok(rendered);
        }
        if occupancy_limit > 0 {
            occupancy_limit /= 2;
        } else if root_limit > 1 {
            root_limit /= 2;
        } else if root_limit > 0 {
            root_limit = 0;
        } else {
            workspace_limit /= 2;
        }
    }
}

fn workspace_state_receipt(
    state: &WorkspaceStateResult,
    output_ref: &str,
    root_limit: usize,
    occupancy_limit: usize,
    workspace_limit: usize,
) -> Value {
    let selected_roots = select_execution_roots(state, root_limit);
    let selected_occupancies = select_occupancies(&state.occupancies, occupancy_limit);
    let selected_workspaces = state
        .workspaces
        .iter()
        .take(workspace_limit)
        .collect::<Vec<_>>();
    let attached_limit = DEFAULT_WORKSPACE_LIMIT.min(state.attached_workspace_ids.len());
    serde_json::json!({
        "agent_id": truncate_chars(&state.agent_id, STRING_LIMIT),
        "attached_workspace_ids": state.attached_workspace_ids
            .iter()
            .take(attached_limit)
            .map(|value| truncate_chars(value, STRING_LIMIT))
            .collect::<Vec<_>>(),
        "attached_workspace_ids_total": state.attached_workspace_ids.len(),
        "attached_workspace_ids_truncated": attached_limit < state.attached_workspace_ids.len(),
        "workspaces": {
            "total": state.workspaces.len(),
            "returned": selected_workspaces.len(),
            "truncated": selected_workspaces.len() < state.workspaces.len(),
            "items": selected_workspaces.into_iter().map(workspace_summary).collect::<Vec<_>>(),
        },
        "active": state.active.as_ref().map(active_summary),
        "execution_roots": {
            "total": state.execution_roots.len(),
            "returned": selected_roots.len(),
            "truncated": selected_roots.len() < state.execution_roots.len(),
            "selection": "active_then_occupied_then_recent",
            "items": selected_roots.into_iter().map(execution_root_summary).collect::<Vec<_>>(),
        },
        "occupancies": {
            "total": state.occupancies.len(),
            "returned": selected_occupancies.len(),
            "truncated": selected_occupancies.len() < state.occupancies.len(),
            "items": selected_occupancies.into_iter().map(occupancy_summary).collect::<Vec<_>>(),
        },
        "output_ref": output_ref,
        "provider_projection_truncated": true,
    })
}

fn select_execution_roots(state: &WorkspaceStateResult, limit: usize) -> Vec<&ExecutionRootEntry> {
    let mut selected = Vec::with_capacity(limit);
    let mut seen = HashSet::new();
    let active_root_id = state
        .active
        .as_ref()
        .map(|active| active.execution_root_id.as_str());
    let occupied_root_ids = state
        .occupancies
        .iter()
        .filter(|occupancy| occupancy.released_at.is_none())
        .map(|occupancy| occupancy.execution_root_id.as_str())
        .collect::<HashSet<_>>();

    if let Some(active_root_id) = active_root_id {
        if let Some(root) = state
            .execution_roots
            .iter()
            .find(|root| root.execution_root_id == active_root_id)
        {
            push_root(&mut selected, &mut seen, root, limit);
        }
    }
    let mut occupied = state
        .execution_roots
        .iter()
        .filter(|root| occupied_root_ids.contains(root.execution_root_id.as_str()))
        .collect::<Vec<_>>();
    occupied.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.execution_root_id.cmp(&right.execution_root_id))
    });
    for root in occupied {
        push_root(&mut selected, &mut seen, root, limit);
    }
    let mut recent = state.execution_roots.iter().collect::<Vec<_>>();
    recent.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.execution_root_id.cmp(&right.execution_root_id))
    });
    for root in recent {
        push_root(&mut selected, &mut seen, root, limit);
    }
    selected
}

fn push_root<'a>(
    selected: &mut Vec<&'a ExecutionRootEntry>,
    seen: &mut HashSet<&'a str>,
    root: &'a ExecutionRootEntry,
    limit: usize,
) {
    if selected.len() < limit && seen.insert(root.execution_root_id.as_str()) {
        selected.push(root);
    }
}

fn select_occupancies(
    occupancies: &[WorkspaceOccupancyRecord],
    limit: usize,
) -> Vec<&WorkspaceOccupancyRecord> {
    let mut selected = occupancies.iter().collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.released_at
            .is_some()
            .cmp(&right.released_at.is_some())
            .then_with(|| right.acquired_at.cmp(&left.acquired_at))
            .then_with(|| left.occupancy_id.cmp(&right.occupancy_id))
    });
    selected.truncate(limit);
    selected
}

fn workspace_summary(workspace: &crate::types::WorkspaceEntry) -> Value {
    serde_json::json!({
        "workspace_id": truncate_chars(&workspace.workspace_id, STRING_LIMIT),
        "workspace_alias": workspace.workspace_alias.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
        "workspace_anchor": truncate_chars(&workspace.workspace_anchor.display().to_string(), STRING_LIMIT),
        "repo_name": workspace.repo_name.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
        "updated_at": workspace.updated_at,
    })
}

fn active_summary(active: &crate::types::ActiveWorkspaceEntry) -> Value {
    serde_json::json!({
        "workspace_id": truncate_chars(&active.workspace_id, STRING_LIMIT),
        "workspace_anchor": truncate_chars(&active.workspace_anchor.display().to_string(), STRING_LIMIT),
        "execution_root_id": truncate_chars(&active.execution_root_id, STRING_LIMIT),
        "execution_root": truncate_chars(&active.execution_root.display().to_string(), STRING_LIMIT),
        "projection_kind": active.projection_kind,
        "access_mode": active.access_mode,
        "cwd": truncate_chars(&active.cwd.display().to_string(), STRING_LIMIT),
        "occupancy_id": active.occupancy_id.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
    })
}

fn execution_root_summary(root: &ExecutionRootEntry) -> Value {
    serde_json::json!({
        "execution_root_id": truncate_chars(&root.execution_root_id, STRING_LIMIT),
        "workspace_id": truncate_chars(&root.workspace_id, STRING_LIMIT),
        "filesystem_path": truncate_chars(&root.filesystem_path.display().to_string(), STRING_LIMIT),
        "root_kind": root.root_kind,
        "created_at": root.created_at,
        "removed_at": root.removed_at,
        "worktree": root.worktree.as_ref().map(|worktree| serde_json::json!({
            "branch": worktree.branch.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
            "branch_ref": worktree.branch_ref.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
            "head_commit": worktree.head_commit.as_deref().map(|value| truncate_chars(value, STRING_LIMIT)),
            "detached": worktree.detached,
            "cleanup_status": worktree.last_cleanup.as_ref().map(|cleanup| truncate_chars(&cleanup.status, STRING_LIMIT)),
            "cleanup_error": worktree.last_cleanup.as_ref().and_then(|cleanup| cleanup.error.as_deref()).map(|value| truncate_chars(value, STRING_LIMIT)),
        })),
    })
}

fn occupancy_summary(occupancy: &WorkspaceOccupancyRecord) -> Value {
    serde_json::json!({
        "occupancy_id": truncate_chars(&occupancy.occupancy_id, STRING_LIMIT),
        "execution_root_id": truncate_chars(&occupancy.execution_root_id, STRING_LIMIT),
        "workspace_id": truncate_chars(&occupancy.workspace_id, STRING_LIMIT),
        "holder_agent_id": truncate_chars(&occupancy.holder_agent_id, STRING_LIMIT),
        "access_mode": occupancy.access_mode,
        "acquired_at": occupancy.acquired_at,
        "released_at": occupancy.released_at,
    })
}

fn estimated_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::Value;

    use super::*;
    use crate::{
        system::{WorkspaceAccessMode, WorkspaceProjectionKind},
        tool::{spec::ToolResultStatus, tools::serialize_success},
        types::{ActiveWorkspaceEntry, WorkspaceStateResult},
    };

    #[test]
    fn renderer_bounds_large_execution_root_history_and_preserves_selection_order() {
        let now = Utc::now();
        let active_root_id = "root-active".to_string();
        let occupied_root_id = "root-occupied".to_string();
        let mut execution_roots = (0..249)
            .map(|index| ExecutionRootEntry {
                execution_root_id: format!("root-{index:03}"),
                workspace_id: "workspace-1".into(),
                filesystem_path: format!("/tmp/{}", "long-path/".repeat(80)).into(),
                root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                worktree: None,
                created_at: now - Duration::seconds(index),
                removed_at: None,
            })
            .collect::<Vec<_>>();
        execution_roots.push(ExecutionRootEntry {
            execution_root_id: active_root_id.clone(),
            workspace_id: "workspace-1".into(),
            filesystem_path: "/tmp/active".into(),
            root_kind: WorkspaceProjectionKind::CanonicalRoot,
            worktree: None,
            created_at: now - Duration::days(10),
            removed_at: None,
        });
        execution_roots.push(ExecutionRootEntry {
            execution_root_id: occupied_root_id.clone(),
            workspace_id: "workspace-1".into(),
            filesystem_path: "/tmp/occupied".into(),
            root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: now - Duration::days(9),
            removed_at: None,
        });
        let state = WorkspaceStateResult {
            agent_id: "agent-1".into(),
            attached_workspace_ids: vec!["workspace-1".into()],
            workspaces: vec![],
            active: Some(ActiveWorkspaceEntry {
                workspace_id: "workspace-1".into(),
                workspace_anchor: "/tmp/repo".into(),
                execution_root_id: active_root_id.clone(),
                execution_root: "/tmp/active".into(),
                projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                access_mode: WorkspaceAccessMode::ExclusiveWrite,
                cwd: "/tmp/active".into(),
                occupancy_id: None,
                projection_metadata: None,
            }),
            execution_roots,
            occupancies: vec![WorkspaceOccupancyRecord {
                occupancy_id: "occupancy-1".into(),
                execution_root_id: occupied_root_id.clone(),
                workspace_id: "workspace-1".into(),
                holder_agent_id: "agent-2".into(),
                access_mode: WorkspaceAccessMode::ExclusiveWrite,
                acquired_at: now,
                released_at: None,
            }],
            summary_text: None,
        };
        let result = serialize_success(NAME, &state).unwrap();
        assert_eq!(result.envelope.status, ToolResultStatus::Success);
        let rendered = render_for_model(
            &result,
            &ToolModelRenderContext {
                tool_execution_id: "tool-123",
                tool_output_budget_estimated_tokens: 2_500,
            },
        )
        .unwrap();
        assert!(estimated_tokens(&rendered) <= 2_500);

        let receipt: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(receipt["execution_roots"]["total"], 251);
        assert!(receipt["execution_roots"]["returned"].as_u64().unwrap() <= 10);
        assert_eq!(receipt["execution_roots"]["truncated"], true);
        assert_eq!(
            receipt["execution_roots"]["selection"],
            "active_then_occupied_then_recent"
        );
        assert_eq!(
            receipt["execution_roots"]["items"][0]["execution_root_id"],
            active_root_id
        );
        assert_eq!(
            receipt["execution_roots"]["items"][1]["execution_root_id"],
            occupied_root_id
        );
        assert_eq!(receipt["output_ref"], "tool_execution:tool-123:output");
        assert!(receipt.get("next_cursor").is_none());
        assert!(!rendered.contains("changed_files"));
    }
}
