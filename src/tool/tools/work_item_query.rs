use std::collections::BTreeMap;

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
    work_item_scheduling::{
        WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingProjection,
        WorkItemSchedulingReasonCode,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemLifecycleView {
    Open,
    Completing,
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
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemPlanArtifactStatus {
    NotRecorded,
    Available,
    Missing,
    Unreadable,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan_artifact: Option<WorkItemPlanArtifact>,
    pub(crate) plan_artifact_status: WorkItemPlanArtifactStatus,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bound_work_item_id: Option<String>,
}

pub(crate) type WorkItemDeliverySummaryMap = BTreeMap<String, DeliverySummaryRecord>;

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

/// Result outcome and the one selected projection publication both survive
/// until the caller's synchronous serialization boundary.
pub(crate) struct WorkItemMutationViewMaterial {
    _result: std::sync::Arc<crate::runtime::WorkItemMutationMaterial>,
    _snapshot: crate::runtime::WorkQuerySnapshot,
}

/// A real create kernel observation bound by its successful original Work ACK.
/// A later resource refresh failure cannot turn this committed result into I/O.
pub(crate) async fn view_for_created_work_item(
    runtime: &RuntimeHandle,
    read: crate::runtime::OwnedCreatedWorkItemRead,
) -> Result<(WorkItemView, WorkItemMutationViewMaterial)> {
    view_for_owned_work_item(runtime, read).await
}
pub(crate) async fn view_for_owned_work_item(
    runtime: &RuntimeHandle,
    read: crate::runtime::OwnedWorkItemMutationRead,
) -> Result<(WorkItemView, WorkItemMutationViewMaterial)> {
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    view_for_owned_work_item_in_snapshot(runtime, snapshot, read)
}
pub(crate) fn view_for_owned_work_item_in_snapshot(
    runtime: &RuntimeHandle,
    snapshot: crate::runtime::WorkQuerySnapshot,
    read: crate::runtime::OwnedWorkItemMutationRead,
) -> Result<(WorkItemView, WorkItemMutationViewMaterial)> {
    let (record, material) = read.into_parts();
    let context = query_context_from_work_items(&snapshot.state, &snapshot.items);
    // A later ACK may have changed this same ID. Keep the original successful
    // writer record and never apply a successor body's waits/readiness to it.
    let projection = snapshot
        .items
        .iter()
        .find(|item| item.work_item == record)
        .cloned()
        .unwrap_or_else(|| projection_outside_query_window(&context, &record));
    let query = material.plan_query();
    let prepared_report = if let Some(report) = material.acknowledged_report(&record)? {
        Some(report.map(|report| WorkItemCompletionReportView {
            text: report.text.clone(),
            source: match report.kind {
                crate::runtime::OriginalWorkReportKind::ResultBrief => {
                    WorkItemCompletionReportSource::ResultBrief
                }
                crate::runtime::OriginalWorkReportKind::WorkItemResultSummary => {
                    WorkItemCompletionReportSource::WorkItemResultSummary
                }
                crate::runtime::OriginalWorkReportKind::DeliverySummary => {
                    WorkItemCompletionReportSource::DeliverySummary
                }
            },
            brief_id: report.brief_id.clone(),
            delivery_summary_id: report.delivery_summary_id.clone(),
            source_turn_index: report.source_turn_index,
            created_at: report.created_at,
        }))
    } else {
        material.prepared_report(&record)?.map(|report| {
            prepared_completion_report(report.result_summary(), report.legacy_summary())
        })
    };
    let view = view_from_plan_query(
        runtime,
        record,
        true,
        None,
        projection,
        query,
        prepared_report,
    )?;
    Ok((
        view,
        WorkItemMutationViewMaterial {
            _result: material,
            _snapshot: snapshot,
        },
    ))
}

fn prepared_completion_report(
    summary: Option<&str>,
    legacy: Option<(&str, &str, Option<u64>, chrono::DateTime<chrono::Utc>)>,
) -> Option<WorkItemCompletionReportView> {
    let (text, source) = if let Some(text) = summary {
        (text, WorkItemCompletionReportSource::WorkItemResultSummary)
    } else {
        let (_, text, _, _) = legacy.filter(|(_, text, _, _)| !text.is_empty())?;
        (text, WorkItemCompletionReportSource::DeliverySummary)
    };
    let metadata = legacy.filter(|(_, original, _, _)| *original == text);
    Some(WorkItemCompletionReportView {
        text: text.to_owned(),
        source,
        brief_id: None,
        delivery_summary_id: metadata.map(|(id, _, _, _)| id.to_owned()),
        source_turn_index: metadata.and_then(|(_, _, turn, _)| turn),
        created_at: metadata.map(|(_, _, _, at)| at),
    })
}

pub(crate) fn view_for_planned_work_item(
    runtime: &RuntimeHandle,
    snapshot: crate::runtime::WorkQuerySnapshot,
    read: crate::runtime::OwnedWorkItemMutationRead,
    projection: WorkItemSchedulingProjection,
) -> Result<(WorkItemView, WorkItemMutationViewMaterial)> {
    let (record, material) = read.into_parts();
    anyhow::ensure!(
        projection.work_item == record,
        "planned Work projection body differs"
    );
    let query = material.plan_query();
    let prepared_report = material
        .prepared_report(&record)?
        .map(|report| prepared_completion_report(report.result_summary(), report.legacy_summary()));
    let view = view_from_plan_query(
        runtime,
        record,
        true,
        None,
        projection,
        query,
        prepared_report,
    )?;
    Ok((
        view,
        WorkItemMutationViewMaterial {
            _result: material,
            _snapshot: snapshot,
        },
    ))
}

/// The selected plan was captured under the request's original Work source.
/// Its material remains alive through the caller's actual synchronous wire.
/// Ordinary reports use only the same publication as the selected Work/plan.
pub(crate) fn view_for_record_in_snapshot(
    runtime: &RuntimeHandle,
    snapshot: &crate::runtime::WorkQuerySnapshot,
    record: WorkItemRecord,
    include_todo_list: bool,
    latest_nonempty: bool,
    projection: WorkItemSchedulingProjection,
    read: crate::runtime::WorkItemPlanQueryRead,
) -> Result<(WorkItemView, crate::runtime::WorkItemPlanQueryRead)> {
    let report = completion_report_in_snapshot(snapshot, &record, latest_nonempty)?;
    let (query, material) = read.into_parts();
    let view = view_from_plan_query(
        runtime,
        record,
        include_todo_list,
        None,
        projection,
        query,
        Some(report),
    )?;
    Ok((view, material))
}
fn completion_report_in_snapshot(
    snapshot: &crate::runtime::WorkQuerySnapshot,
    record: &WorkItemRecord,
    latest_nonempty: bool,
) -> Result<Option<WorkItemCompletionReportView>> {
    if record.state != WorkItemState::Completed {
        return Ok(None);
    }
    if let Some((brief, text)) = snapshot.work_report_brief_text(record)? {
        return Ok(Some(WorkItemCompletionReportView {
            text,
            source: WorkItemCompletionReportSource::ResultBrief,
            brief_id: Some(brief.id.clone()),
            delivery_summary_id: None,
            source_turn_index: brief.turn_index,
            created_at: Some(brief.created_at),
        }));
    }
    let delivery = snapshot.work_report_delivery_summary(record, latest_nonempty)?;
    if let Some(text) = record
        .result_summary
        .as_ref()
        .filter(|text| !text.is_empty())
    {
        return Ok(Some(completion_report_view(
            text.clone(),
            WorkItemCompletionReportSource::WorkItemResultSummary,
            delivery.filter(|summary| summary.text == *text),
        )));
    }
    Ok(delivery
        .filter(|summary| !summary.text.is_empty())
        .map(|summary| {
            completion_report_view(
                summary.text.clone(),
                WorkItemCompletionReportSource::DeliverySummary,
                Some(summary),
            )
        }))
}
/// Original explicit by-ID cause; its finite SQL report and guards were
/// admitted together with the recalled row before any payload decoding.
pub(crate) fn view_for_recalled_record_observed(
    runtime: &RuntimeHandle,
    record: WorkItemRecord,
    include_todo_list: bool,
    inspection: &crate::runtime_db::work_item_recall::ExplicitWorkItemRecallMaterial,
    projection: WorkItemSchedulingProjection,
    read: crate::runtime::WorkItemPlanQueryRead,
) -> Result<(WorkItemView, crate::runtime::WorkItemPlanQueryRead)> {
    use crate::runtime_db::work_item_recall::ExplicitWorkReportSource;
    let report = inspection
        .completion_report()
        .map(|value| WorkItemCompletionReportView {
            text: value.text.clone(),
            source: match value.source {
                ExplicitWorkReportSource::ResultBrief => {
                    WorkItemCompletionReportSource::ResultBrief
                }
                ExplicitWorkReportSource::WorkItemResultSummary => {
                    WorkItemCompletionReportSource::WorkItemResultSummary
                }
                ExplicitWorkReportSource::DeliverySummary => {
                    WorkItemCompletionReportSource::DeliverySummary
                }
            },
            brief_id: value.brief_id.clone(),
            delivery_summary_id: value.delivery_summary_id.clone(),
            source_turn_index: value.source_turn_index,
            created_at: value.created_at,
        });
    let (query, material) = read.into_parts();
    let view = view_from_plan_query(
        runtime,
        record,
        include_todo_list,
        None,
        projection,
        query,
        Some(report),
    )?;
    Ok((view, material))
}
pub(crate) fn view_for_record_observed(
    runtime: &RuntimeHandle,
    record: WorkItemRecord,
    include_todo_list: bool,
    delivery_summaries: Option<&WorkItemDeliverySummaryMap>,
    projection: WorkItemSchedulingProjection,
    read: crate::runtime::WorkItemPlanQueryRead,
) -> Result<(WorkItemView, crate::runtime::WorkItemPlanQueryRead)> {
    let (query, material) = read.into_parts();
    let view = view_from_plan_query(
        runtime,
        record,
        include_todo_list,
        delivery_summaries,
        projection,
        query,
        None,
    )?;
    Ok((view, material))
}
fn view_from_plan_query(
    runtime: &RuntimeHandle,
    record: WorkItemRecord,
    include_todo_list: bool,
    delivery_summaries: Option<&WorkItemDeliverySummaryMap>,
    projection: WorkItemSchedulingProjection,
    query: crate::work_item_plan::PlanArtifactQuery,
    observed_report: Option<Option<WorkItemCompletionReportView>>,
) -> Result<WorkItemView> {
    let (plan_artifact, plan_artifact_status, plan_diagnostic) = match query {
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
    let completion_report = match observed_report {
        Some(report) => report,
        None => completion_report_for_record(runtime, &record, delivery_summaries)?,
    };
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
        WorkItemState::Completing => WorkItemLifecycleView::Completing,
        WorkItemState::Completed => WorkItemLifecycleView::Completed,
    }
}
