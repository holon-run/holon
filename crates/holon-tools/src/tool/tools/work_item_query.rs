use crate::types::DeliverySummaryRecord;
use crate::types::TodoItem;
use crate::types::WaitConditionSummary;
use crate::types::WorkItemPlanArtifact;
use crate::types::WorkItemPlanStatus;
use crate::types::WorkItemReadiness;
use crate::types::WorkItemRef;
use crate::types::WorkItemSchedulingState;
use crate::types::WorkItemState;
use chrono::DateTime;
use chrono::Utc;
use holon_domain::work_item_scheduling::WorkItemCandidateClass;
use holon_domain::work_item_scheduling::WorkItemFocus;
use holon_domain::work_item_scheduling::WorkItemSchedulingReasonCode;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemLifecycleView {
    Open,
    Completing,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemCompletionReportSource {
    ResultBrief,
    WorkItemResultSummary,
    DeliverySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemPlanArtifactStatus {
    NotRecorded,
    Available,
    Missing,
    Unreadable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemCompletionReportView {
    pub text: String,
    pub source: WorkItemCompletionReportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_summary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemView {
    pub id: String,
    pub agent_id: String,
    pub workspace_id: String,
    pub objective: String,
    pub state: WorkItemLifecycleView,
    pub focus: WorkItemFocus,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub reason_code: WorkItemSchedulingReasonCode,
    pub is_current: bool,
    pub is_runnable: bool,
    pub plan_status: WorkItemPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_artifact: Option<WorkItemPlanArtifact>,
    pub plan_artifact_status: WorkItemPlanArtifactStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todo_list: Vec<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_refs: Vec<WorkItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_consumed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_report: Option<WorkItemCompletionReportView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_wait_conditions: Vec<WaitConditionSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemQueryContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_work_item_id: Option<String>,
}

pub type WorkItemDeliverySummaryMap = BTreeMap<String, DeliverySummaryRecord>;

pub fn completion_report_view(
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

pub fn completion_report_view_from_brief(
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

pub fn lifecycle_view(state: &WorkItemState) -> WorkItemLifecycleView {
    match state {
        WorkItemState::Open => WorkItemLifecycleView::Open,
        WorkItemState::Completing => WorkItemLifecycleView::Completing,
        WorkItemState::Completed => WorkItemLifecycleView::Completed,
    }
}
