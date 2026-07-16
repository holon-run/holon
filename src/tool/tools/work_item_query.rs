use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    object_resolver::RuntimeObjectResolver,
    runtime::RuntimeHandle,
    types::{
        DeliverySummaryRecord, TodoItem, WaitConditionSummary, WorkItemPlanArtifact,
        WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord, WorkItemRef,
        WorkItemSchedulingState, WorkItemState,
    },
    work_item_scheduling::{WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingReasonCode},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemLifecycleView {
    Open,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemCompletionReportSource {
    ResultBrief,
    WorkItemResultSummary,
    DeliverySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemCompletionReportView {
    pub(crate) text: String,
    pub(crate) source: WorkItemCompletionReportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) brief_id: Option<String>,
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
    pub(crate) focus: WorkItemFocus,
    pub(crate) scheduling_state: WorkItemSchedulingState,
    pub(crate) readiness: WorkItemReadiness,
    pub(crate) candidate_class: WorkItemCandidateClass,
    pub(crate) reason_code: WorkItemSchedulingReasonCode,
    pub(crate) is_current: bool,
    pub(crate) is_runnable: bool,
    pub(crate) plan_status: WorkItemPlanStatus,
    pub(crate) plan_artifact: WorkItemPlanArtifact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) todo_list: Vec<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) work_refs: Vec<WorkItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recheck_consumed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completion_report: Option<WorkItemCompletionReportView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) active_wait_conditions: Vec<WaitConditionSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemQueryContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current_work_item_id: Option<String>,
}

pub(crate) type WorkItemDeliverySummaryMap = BTreeMap<String, DeliverySummaryRecord>;
pub(crate) type WorkItemWaitConditionSummaryMap = BTreeMap<String, Vec<WaitConditionSummary>>;
pub(crate) type WorkItemYieldedSet = BTreeSet<String>;

pub(crate) async fn query_context(runtime: &RuntimeHandle) -> Result<WorkItemQueryContext> {
    let state = runtime.agent_state().await?;
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
    wait_conditions: Option<&WorkItemWaitConditionSummaryMap>,
    yielded_ids: Option<&WorkItemYieldedSet>,
) -> Result<WorkItemView> {
    let projection = runtime
        .storage()
        .work_queue_read_model()?
        .items
        .into_iter()
        .find(|item| item.work_item.id == record.id)
        .unwrap_or_else(|| {
            crate::work_item_scheduling::derive_work_item_scheduling(
                crate::work_item_scheduling::WorkItemSchedulingFacts {
                    work_item: &record,
                    is_current: context.current_work_item_id.as_deref() == Some(record.id.as_str()),
                    is_yielded: false,
                    active_wait_conditions: &[],
                    trigger_delivery_by_id: &BTreeMap::new(),
                },
            )
        });
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
    let completion_report = completion_report_for_record(runtime, &record, delivery_summaries)?;
    let _ = (wait_conditions, yielded_ids);
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
        todo_list,
        work_refs: record.work_refs,
        blocked_by: record.blocked_by,
        recheck_at: record.recheck_at,
        recheck_consumed_at: record.recheck_consumed_at,
        completion_report,
        active_wait_conditions: projection.active_wait_conditions,
        diagnostics: projection.diagnostics,
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

fn completion_report_view(
    text: String,
    source: WorkItemCompletionReportSource,
    delivery_summary: Option<&DeliverySummaryRecord>,
) -> WorkItemCompletionReportView {
    WorkItemCompletionReportView {
        text,
        source,
        brief_id: None,
        delivery_summary_id: delivery_summary.map(|summary| summary.id.clone()),
        source_turn_index: delivery_summary.and_then(|summary| summary.source_turn_index),
        created_at: delivery_summary.map(|summary| summary.created_at),
    }
}

fn completion_report_view_from_brief(
    brief: crate::types::BriefRecord,
    text: String,
) -> WorkItemCompletionReportView {
    WorkItemCompletionReportView {
        text,
        source: WorkItemCompletionReportSource::ResultBrief,
        brief_id: Some(brief.id),
        delivery_summary_id: None,
        source_turn_index: brief.turn_index,
        created_at: Some(brief.created_at),
    }
}

pub(crate) fn lifecycle_view(state: &WorkItemState) -> WorkItemLifecycleView {
    match state {
        WorkItemState::Open => WorkItemLifecycleView::Open,
        WorkItemState::Completed => WorkItemLifecycleView::Completed,
    }
}
