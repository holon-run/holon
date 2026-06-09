use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{parse_tool_args, validate_non_empty},
    tool::spec::typed_spec,
    types::{
        AuthorityClass, TodoItem, TodoItemState, ToolCapabilityFamily, WorkItemRecord,
        WorkItemState,
    },
};

use super::{
    serialize_success,
    work_item_action::WorkItemMutationResult,
    work_item_query::{query_context, view_for_record},
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "CompleteWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteWorkItemArgs {
    pub(crate) work_item_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkItemCompletionWarning {
    pub(crate) kind: String,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) in_progress_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) sample: Vec<TodoItem>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CompleteWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/complete_work_item.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CompleteWorkItemArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let before = runtime.latest_work_item(&work_item_id).await?;
    let completed_transition = before
        .as_ref()
        .map(|record| record.state != WorkItemState::Completed)
        .unwrap_or(false);
    let warnings = before.as_ref().map(completion_warnings).unwrap_or_default();
    let work_item = runtime
        .complete_work_item(work_item_id, warnings_json(&warnings))
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true, None, None).await?;
    serialize_success(
        NAME,
        &WorkItemMutationResult::with_completion_transition(
            work_item,
            warnings_json(&warnings),
            completed_transition,
        ),
    )
}

pub(crate) fn completion_warnings(record: &WorkItemRecord) -> Vec<WorkItemCompletionWarning> {
    let pending_count = record
        .todo_list
        .iter()
        .filter(|item| item.state == TodoItemState::Pending)
        .count();
    let in_progress_count = record
        .todo_list
        .iter()
        .filter(|item| item.state == TodoItemState::InProgress)
        .count();
    if pending_count == 0 && in_progress_count == 0 {
        return Vec::new();
    }
    let sample = record
        .todo_list
        .iter()
        .filter(|item| item.state != TodoItemState::Completed)
        .take(5)
        .cloned()
        .collect();
    vec![WorkItemCompletionWarning {
        kind: "unfinished_todos".into(),
        message: "Work item completed with unfinished todo items.".into(),
        pending_count: Some(pending_count),
        in_progress_count: Some(in_progress_count),
        sample,
    }]
}

fn warnings_json(warnings: &[WorkItemCompletionWarning]) -> Vec<serde_json::Value> {
    warnings
        .iter()
        .filter_map(|warning| serde_json::to_value(warning).ok())
        .collect()
}
