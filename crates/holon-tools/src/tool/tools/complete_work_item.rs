use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::TodoItem;
use crate::types::TodoItemState;
use crate::types::ToolCapabilityFamily;
use crate::types::WorkItemRecord;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::COMPLETE_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteWorkItemArgs {
    pub work_item_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkItemCompletionWarning {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample: Vec<TodoItem>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CompleteWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/complete_work_item.md"),
        )?,
    })
}

pub fn completion_warnings(record: &WorkItemRecord) -> Vec<WorkItemCompletionWarning> {
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
        message: "Work item completion requested with unfinished todo items.".into(),
        pending_count: Some(pending_count),
        in_progress_count: Some(in_progress_count),
        sample,
    }]
}

pub fn warnings_json(warnings: &[WorkItemCompletionWarning]) -> Vec<serde_json::Value> {
    warnings
        .iter()
        .filter_map(|warning| serde_json::to_value(warning).ok())
        .collect()
}
