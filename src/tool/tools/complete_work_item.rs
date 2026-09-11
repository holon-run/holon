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
    let authority = WorkItemCompletionAuthority::AgentExecution {
        binding: execution_binding,
        effective_work_item_id: context.effective_work_item_id.clone(),
    };
    if candidate.is_none()
        && before
            .as_ref()
            .is_some_and(|record| record.state != WorkItemState::Completed)
    {
        let (expected_work_revision, settlement) = runtime
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
                "completion_mode": completion_mode_name(settlement),
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
    let dispatch = runtime
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
    let (
        completed,
        completed_transition,
        completion_report_promoted,
        continuation_resumed,
        settlement,
        prepared,
    ) = match dispatch {
        crate::runtime::WorkItemCompletionDispatch::Prepared(prepared) => {
            let settlement = prepared.settlement;
            (
                prepared.record.clone(),
                true,
                true,
                prepared.continuation_resumed.clone(),
                settlement,
                Some(prepared),
            )
        }
        crate::runtime::WorkItemCompletionDispatch::Unchanged(completed) => (
            completed,
            false,
            false,
            None,
            crate::runtime::WorkItemCompletionSettlement::Detached,
            None,
        ),
    };
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, completed, true, None, None).await?;
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
        object.insert(
            "completion_mode".into(),
            serde_json::json!(completion_mode_name(settlement)),
        );
        object.insert(
            "completion_phase".into(),
            serde_json::json!(if prepared.is_some() {
                "prepared"
            } else {
                "unchanged"
            }),
        );
    }
    let mut result = serialize_success(NAME, &result)?;
    result.envelope.summary_text = Some(format!(
        "Completion {} for {work_item_id}: completed_transition={completed_transition}, mode={}, report_promoted={completion_report_promoted}.",
        if prepared.is_some() { "prepared" } else { "unchanged" },
        completion_mode_name(settlement),
    ));
    let terminal_transition =
        settlement == crate::runtime::WorkItemCompletionSettlement::BoundExecution;
    if terminal_transition {
        result.should_sleep = true;
        result.terminal_transition = true;
    }
    result.prepared_work_item_completion = prepared.map(Box::new);
    Ok(result)
}

fn completion_mode_name(settlement: crate::runtime::WorkItemCompletionSettlement) -> &'static str {
    match settlement {
        crate::runtime::WorkItemCompletionSettlement::BoundExecution => "bound_execution",
        crate::runtime::WorkItemCompletionSettlement::Detached => "detached",
    }
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
