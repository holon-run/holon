use super::serialize_success;
use super::ToolModelRenderContext;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
#[cfg(test)]
use crate::types::ExecutionRootEntry;
#[cfg(test)]
use crate::types::WorkspaceOccupancyRecord;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::get_workspace_state::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let _: GetWorkspaceStateArgs = parse_tool_args(NAME, input)?;
    let state = runtime.workspace_state_result().await?;
    let _serialize = crate::diagnostics::attribution::WORKSPACE_STATE_SERIALIZE.start();
    serialize_success(NAME, &state)
}

pub(crate) fn render_for_model(
    result: &crate::tool::ToolResult,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    let _render = crate::diagnostics::attribution::WORKSPACE_STATE_RENDER.start();
    holon_tools::builtins::get_workspace_state::render_for_model(result, context)
}

#[cfg(holon_lib_test_shard = "c")]
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
