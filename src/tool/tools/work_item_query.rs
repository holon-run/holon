use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::RuntimeHandle,
    types::{
        DeliverySummaryRecord, TodoItem, WorkItemPlanArtifact, WorkItemPlanStatus,
        WorkItemReadiness, WorkItemRecord, WorkItemState,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemLifecycleView {
    Open,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemFocusView {
    Current,
    Queued,
    Blocked,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemCompletionReportSource {
    WorkItemResultSummary,
    DeliverySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemCompletionReportView {
    pub(crate) text: String,
    pub(crate) source: WorkItemCompletionReportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_summary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemView {
    pub(crate) id: String,
    pub(crate) agent_id: String,
    pub(crate) workspace_id: String,
    pub(crate) objective: String,
    pub(crate) state: WorkItemLifecycleView,
    pub(crate) focus: WorkItemFocusView,
    pub(crate) readiness: WorkItemReadiness,
    pub(crate) is_current: bool,
    pub(crate) is_runnable: bool,
    pub(crate) plan_status: WorkItemPlanStatus,
    pub(crate) plan_artifact: WorkItemPlanArtifact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) todo_list: Vec<TodoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_consumed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completion_report: Option<WorkItemCompletionReportView>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemQueryContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current_work_item_id: Option<String>,
}

pub(crate) type WorkItemDeliverySummaryMap = BTreeMap<String, DeliverySummaryRecord>;

pub(crate) async fn query_context(runtime: &RuntimeHandle) -> Result<WorkItemQueryContext> {
    let state = runtime.agent_state().await?;
    if let Some(bound_id) = state.current_turn_work_item_id.as_deref() {
        if let Some(record) = runtime.latest_work_item(bound_id).await? {
            if record.state == WorkItemState::Open {
                return Ok(WorkItemQueryContext {
                    current_work_item_id: Some(record.id),
                });
            }
        }
    }
    let current_work_item_id = match state.current_work_item_id.as_deref() {
        Some(current_id) => runtime
            .latest_work_item(current_id)
            .await?
            .filter(|record| record.state == WorkItemState::Open)
            .map(|record| record.id),
        None => None,
    };
    Ok(WorkItemQueryContext {
        current_work_item_id,
    })
}

pub(crate) async fn view_for_record(
    runtime: &RuntimeHandle,
    context: &WorkItemQueryContext,
    record: WorkItemRecord,
    include_todo_list: bool,
    delivery_summaries: Option<&WorkItemDeliverySummaryMap>,
) -> Result<WorkItemView> {
    let is_current = context.current_work_item_id.as_deref() == Some(record.id.as_str())
        && record.state == WorkItemState::Open;
    let mut record = record;
    crate::work_item_plan::refresh_plan_artifact_metadata(
        runtime.agent_home().as_path(),
        &mut record,
    )?;
    let plan_artifact = record
        .plan_artifact
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing plan artifact for work item {}", record.id))?;
    let todo_list = if include_todo_list {
        record.todo_list.clone()
    } else {
        Vec::new()
    };
    let state = lifecycle_view(&record.state);
    let focus = focus_view(&record, is_current);
    let readiness = record.readiness();
    let completion_report = completion_report_for_record(runtime, &record, delivery_summaries)?;
    Ok(WorkItemView {
        id: record.id,
        agent_id: record.agent_id,
        workspace_id: record.workspace_id,
        objective: record.objective,
        state,
        focus,
        readiness,
        is_current,
        is_runnable: readiness == WorkItemReadiness::Runnable,
        plan_status: record.plan_status,
        plan_artifact,
        todo_list,
        blocked_by: record.blocked_by,
        recheck_at: record.recheck_at,
        recheck_consumed_at: record.recheck_consumed_at,
        completion_report,
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

fn completion_report_view(
    text: String,
    source: WorkItemCompletionReportSource,
    delivery_summary: Option<&DeliverySummaryRecord>,
) -> WorkItemCompletionReportView {
    WorkItemCompletionReportView {
        text,
        source,
        delivery_summary_id: delivery_summary.map(|summary| summary.id.clone()),
        source_turn_index: delivery_summary.and_then(|summary| summary.source_turn_index),
        created_at: delivery_summary.map(|summary| summary.created_at),
    }
}

pub(crate) fn lifecycle_view(state: &WorkItemState) -> WorkItemLifecycleView {
    match state {
        WorkItemState::Open => WorkItemLifecycleView::Open,
        WorkItemState::Completed => WorkItemLifecycleView::Completed,
    }
}

pub(crate) fn focus_view(record: &WorkItemRecord, is_current: bool) -> WorkItemFocusView {
    if record.state == WorkItemState::Completed {
        return WorkItemFocusView::Completed;
    }
    if is_current {
        return WorkItemFocusView::Current;
    }
    if record.blocked_by.is_some() {
        WorkItemFocusView::Blocked
    } else {
        WorkItemFocusView::Queued
    }
}
