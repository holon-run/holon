use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::{RuntimeHandle, WorkItemCompletionAuthority},
    runtime_error::RuntimeError,
    tool::helpers::{parse_tool_args, validate_non_empty},
    tool::spec::{
        typed_spec, AwaitCompletionReportDirective, CompletionReportCandidate,
        ToolExecutionContext, ToolLoopDirective,
    },
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

pub(crate) const NAME: &str = crate::tool::names::COMPLETE_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteWorkItemArgs {
    pub(crate) work_item_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    context: &ToolExecutionContext,
) -> Result<crate::tool::ToolResult> {
    let args: CompleteWorkItemArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let before = runtime.latest_work_item(&work_item_id).await?;
    let warnings = before.as_ref().map(completion_warnings).unwrap_or_default();
    let candidate = context
        .completion_report_candidate
        .as_ref()
        .filter(|candidate| !candidate.text.trim().is_empty());
    let execution_binding = runtime
        .agent_state()
        .await?
        .current_execution_binding
        .ok_or_else(|| {
            RuntimeError::policy(
                "work_item_execution_binding_missing",
                "CompleteWorkItem requires an active agent execution binding",
            )
        })?;
    let authority = WorkItemCompletionAuthority::AgentExecution(execution_binding);
    if candidate.is_none()
        && before
            .as_ref()
            .is_some_and(|record| record.state != WorkItemState::Completed)
    {
        let expected_work_revision = runtime
            .validate_work_item_completion_request(&work_item_id, &authority)
            .await?;
        let request_id = crate::ids::completion_report_request_id();
        return Ok(crate::tool::ToolResult::deferred(
            NAME,
            serde_json::json!({
                "disposition": "awaiting_completion_report",
                "completion_request_id": request_id,
                "work_item_id": work_item_id,
                "completed_transition": false,
                "expected_output": "final_text_only",
                "warnings": warnings_json(&warnings),
            }),
            Some("Awaiting the final operator-facing completion report.".into()),
            ToolLoopDirective::AwaitCompletionReport(AwaitCompletionReportDirective {
                request_id,
                work_item_id,
                expected_work_revision,
                warnings: warnings_json(&warnings),
            }),
        ));
    }
    complete_with_report_candidate(
        runtime,
        work_item_id,
        authority,
        candidate,
        warnings,
        "same_assistant_round_preceding_text",
    )
    .await
}

pub(crate) async fn complete_with_report_candidate(
    runtime: &RuntimeHandle,
    work_item_id: String,
    authority: WorkItemCompletionAuthority,
    candidate: Option<&CompletionReportCandidate>,
    warnings: Vec<WorkItemCompletionWarning>,
    report_source: &'static str,
) -> Result<crate::tool::ToolResult> {
    let prepared = runtime
        .prepare_work_item_completion_with_report(
            work_item_id.clone(),
            authority,
            candidate
                .map(|candidate| candidate.text.clone())
                .unwrap_or_default(),
            candidate
                .map(|candidate| candidate.citations.clone())
                .unwrap_or_default(),
            candidate.map(|candidate| candidate.source_turn_index),
            candidate.map(|candidate| candidate.source_round),
            candidate.and_then(|candidate| candidate.source_turn_id.clone()),
            candidate.and_then(|candidate| candidate.source_message_id.clone()),
            candidate.map(|candidate| candidate.source_assistant_round_id.clone()),
            candidate.map(|candidate| candidate.source_tool_call_id.clone()),
            report_source,
            warnings_json(&warnings),
        )
        .await?;
    let (completed, completed_transition, completion_report_promoted, continuation_resumed) =
        match prepared.as_ref() {
            Some(prepared) => (
                prepared.record.clone(),
                true,
                true,
                prepared.continuation_resumed.clone(),
            ),
            None => (
                runtime
                    .latest_work_item(&work_item_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("work item {work_item_id} not found"))?,
                false,
                false,
                None,
            ),
        };
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, completed, true, None, None).await?;
    let terminal_transition = continuation_resumed.is_some();
    let mut result = serde_json::to_value(
        WorkItemMutationResult::with_completion_transition(
            work_item,
            warnings_json(&warnings),
            completed_transition,
        )
        .with_continuation_resumed(continuation_resumed),
    )?;
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "completion_report_promoted".into(),
            serde_json::json!(completion_report_promoted),
        );
        if completion_report_promoted {
            object.insert(
                "completion_report_source".into(),
                serde_json::json!(report_source),
            );
        }
    }
    let mut result = serialize_success(NAME, &result)?;
    if terminal_transition {
        result.should_sleep = true;
        result.terminal_transition = true;
    }
    result.prepared_work_item_completion = prepared.map(Box::new);
    Ok(result)
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
        message: "Work item completion requested with unfinished todo items.".into(),
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
