use crate::object_resolver::RuntimeObjectResolver;
use crate::runtime::RuntimeHandle;
use crate::types::WorkItemRecord;
use crate::types::WorkItemState;
use crate::work_item_scheduling::WorkItemSchedulingProjection;
use anyhow::Result;
use std::collections::BTreeMap;

pub(crate) use holon_tools::builtins::work_item_query::*;

pub(crate) fn query_context_from_work_items(
    state: &crate::types::AgentState,
    items: &[crate::work_item_scheduling::WorkItemSchedulingProjection],
) -> WorkItemQueryContext {
    WorkItemQueryContext {
        current_work_item_id: state.current_work_item_id.as_ref().and_then(|id| {
            items
                .iter()
                .find(|item| {
                    item.work_item.id == *id && item.work_item.state == WorkItemState::Open
                })
                .map(|item| item.work_item.id.clone())
        }),
        bound_work_item_id: state.current_turn_work_item_id.clone(),
    }
}

pub(crate) async fn query_context(runtime: &RuntimeHandle) -> Result<WorkItemQueryContext> {
    let snapshot = runtime.capture_work_query_snapshot().await?;
    Ok(query_context_from_work_items(
        &snapshot.state,
        &snapshot.items,
    ))
}

/// Preserve the existing Get/by-ID fallback for records outside the ordinary window.

pub(crate) fn projection_outside_query_window(
    context: &WorkItemQueryContext,
    record: &WorkItemRecord,
) -> WorkItemSchedulingProjection {
    crate::work_item_scheduling::derive_work_item_scheduling(
        crate::work_item_scheduling::WorkItemSchedulingFacts {
            work_item: record,
            is_current: context.current_work_item_id.as_deref() == Some(record.id.as_str()),
            is_yielded: false,
            active_wait_conditions: &[],
            trigger_delivery_by_id: &BTreeMap::new(),
        },
    )
}

pub(crate) async fn view_for_record(
    runtime: &RuntimeHandle,
    context: &WorkItemQueryContext,
    record: WorkItemRecord,
    include_todo_list: bool,
    delivery_summaries: Option<&WorkItemDeliverySummaryMap>,
    projection: Option<WorkItemSchedulingProjection>,
) -> Result<WorkItemView> {
    // Mutation result views use the same ordinary resident policy. Get supplies
    // its captured projection, including the explicit by-ID historical fallback.
    let snapshot = if projection.is_none() {
        Some(runtime.capture_work_query_snapshot().await?)
    } else {
        None
    };
    let projection = projection.unwrap_or_else(|| {
        snapshot
            .as_ref()
            .expect("captured query")
            .items
            .iter()
            .find(|item| item.work_item.id == record.id)
            .cloned()
            .unwrap_or_else(|| projection_outside_query_window(context, &record))
    });
    let record = record;
    let (plan_artifact, plan_artifact_status, plan_diagnostic) =
        match crate::work_item_plan::query_plan_artifact(runtime.agent_home().as_path(), &record) {
            crate::work_item_plan::PlanArtifactQuery::NotRecorded => {
                (None, WorkItemPlanArtifactStatus::NotRecorded, None)
            }
            crate::work_item_plan::PlanArtifactQuery::Missing => (
                None,
                WorkItemPlanArtifactStatus::Missing,
                Some(format!(
                    "plan artifact missing at {} for work item {}",
                    crate::work_item_plan::plan_path(runtime.agent_home().as_path(), &record.id)
                        .display(),
                    record.id
                )),
            ),
            crate::work_item_plan::PlanArtifactQuery::Available(artifact) => {
                (Some(artifact), WorkItemPlanArtifactStatus::Available, None)
            }
            crate::work_item_plan::PlanArtifactQuery::Unreadable(diagnostic) => (
                None,
                WorkItemPlanArtifactStatus::Unreadable,
                Some(diagnostic),
            ),
        };
    let todo_list = if include_todo_list {
        record.todo_list.clone()
    } else {
        Vec::new()
    };
    let state = lifecycle_view(&record.state);
    let completion_report = completion_report_for_record(runtime, &record, delivery_summaries)?;
    let mut diagnostics = projection.diagnostics;
    if let Some(diagnostic) = plan_diagnostic {
        diagnostics.push(diagnostic);
    }
    Ok(WorkItemView {
        id: record.id,
        agent_id: record.agent_id,
        workspace_id: record.workspace_id,
        objective: record.objective,
        state,
        focus: projection.focus,
        scheduling_state: projection.scheduling_state,
        readiness: projection.readiness,
        candidate_class: projection.candidate_class,
        reason_code: projection.reason_code,
        is_current: projection.is_current,
        is_runnable: projection.is_runnable,
        plan_status: record.plan_status,
        plan_artifact,
        plan_artifact_status,
        todo_list,
        work_refs: record.work_refs,
        blocked_by: record.blocked_by,
        recheck_at: record.recheck_at,
        recheck_consumed_at: record.recheck_consumed_at,
        completion_report,
        active_wait_conditions: projection.active_wait_conditions,
        diagnostics,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

pub(crate) fn latest_delivery_summaries_by_work_item(
    runtime: &RuntimeHandle,
) -> Result<WorkItemDeliverySummaryMap> {
    let mut summaries = BTreeMap::new();
    for summary in runtime
        .storage()
        .read_recent_delivery_summaries(usize::MAX)?
        .into_iter()
        .rev()
        .filter(|summary| !summary.text.is_empty())
    {
        summaries
            .entry(summary.work_item_id.clone())
            .or_insert(summary);
    }
    Ok(summaries)
}

fn completion_report_for_record(
    runtime: &RuntimeHandle,
    record: &WorkItemRecord,
    delivery_summaries: Option<&WorkItemDeliverySummaryMap>,
) -> Result<Option<WorkItemCompletionReportView>> {
    if record.state != WorkItemState::Completed {
        return Ok(None);
    }
    if let Some(brief_id) = record
        .result_brief_id
        .as_ref()
        .filter(|brief_id| !brief_id.trim().is_empty())
    {
        if let Some(brief) = runtime.storage().read_brief_by_id(brief_id)? {
            if !brief.text.trim().is_empty() {
                let text = RuntimeObjectResolver::with_cache(
                    runtime.storage(),
                    runtime.object_query_cache(),
                )
                .resolve_brief_content(&brief)
                .unwrap_or_else(|_| brief.text.clone());
                return Ok(Some(completion_report_view_from_brief(brief, text)));
            }
        }
    }
    let cached_delivery_summary = delivery_summaries
        .and_then(|summaries| summaries.get(&record.id))
        .cloned();
    let latest_delivery_summary = match (delivery_summaries.is_some(), cached_delivery_summary) {
        (_, Some(summary)) => Some(summary),
        (true, None) => None,
        (false, None) => runtime.storage().latest_delivery_summary(&record.id)?,
    };
    if let Some(text) = record
        .result_summary
        .as_ref()
        .filter(|text| !text.is_empty())
    {
        return Ok(Some(completion_report_view(
            text.clone(),
            WorkItemCompletionReportSource::WorkItemResultSummary,
            latest_delivery_summary
                .filter(|summary| summary.text == *text)
                .as_ref(),
        )));
    }
    Ok(latest_delivery_summary
        .filter(|summary| !summary.text.is_empty())
        .map(|summary| {
            completion_report_view(
                summary.text.clone(),
                WorkItemCompletionReportSource::DeliverySummary,
                Some(&summary),
            )
        }))
}
