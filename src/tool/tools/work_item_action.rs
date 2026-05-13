use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    tool::helpers::{parse_tool_args_with_recovery_hint, validate_non_empty},
    types::{TodoItem, TodoItemState},
};

use super::work_item_query::WorkItemView;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum TodoItemStateArgs {
    Pending,
    InProgress,
    Completed,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TodoItemArgs {
    pub(crate) text: String,
    pub(crate) state: TodoItemStateArgs,
}

pub(crate) fn convert_todo_list(
    tool_name: &str,
    items: Vec<TodoItemArgs>,
) -> Result<Vec<TodoItem>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(TodoItem {
                text: validate_non_empty(item.text, tool_name, "text")
                    .with_context(|| format!("invalid todo_list item {index}"))?,
                state: match item.state {
                    TodoItemStateArgs::Pending => TodoItemState::Pending,
                    TodoItemStateArgs::InProgress => TodoItemState::InProgress,
                    TodoItemStateArgs::Completed => TodoItemState::Completed,
                },
            })
        })
        .collect()
}

pub(crate) fn parse_work_item_action_args<T>(
    tool_name: &str,
    input: &serde_json::Value,
) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    parse_tool_args_with_recovery_hint(tool_name, input, || {
        work_item_action_recovery_hint(tool_name).to_string()
    })
}

fn work_item_action_recovery_hint(tool_name: &str) -> &'static str {
    match tool_name {
        "CreateWorkItem" => {
            "ensure the JSON matches the CreateWorkItem schema, including required top-level field \"objective\"; use todo_list items like {\"text\":\"inspect current handler\",\"state\":\"completed\"}; todo state must be pending, in_progress, or completed"
        }
        "UpdateWorkItem" => {
            "ensure the JSON matches the UpdateWorkItem schema, including required top-level field \"work_item_id\"; use todo_list items like {\"text\":\"inspect current handler\",\"state\":\"completed\"}; todo state must be pending, in_progress, or completed"
        }
        _ => {
            "ensure the JSON matches the tool schema exactly; todo state must be pending, in_progress, or completed"
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WorkItemMutationResult {
    pub(crate) work_item: WorkItemView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_transition: Option<bool>,
}

impl WorkItemMutationResult {
    pub(crate) fn new(work_item: WorkItemView) -> Self {
        Self {
            work_item,
            warnings: Vec::new(),
            completed_transition: None,
        }
    }

    pub(crate) fn with_completion_transition(
        work_item: WorkItemView,
        warnings: Vec<serde_json::Value>,
        completed_transition: bool,
    ) -> Self {
        Self {
            work_item,
            warnings,
            completed_transition: Some(completed_transition),
        }
    }
}
