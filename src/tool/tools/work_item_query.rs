use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::RuntimeHandle,
    types::{WorkItemRecord, WorkItemStatus, WorkPlanSnapshot},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemLifecycleView {
    Open,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemFocusView {
    Current,
    Queued,
    Blocked,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemView {
    pub(crate) id: String,
    pub(crate) agent_id: String,
    pub(crate) workspace_id: String,
    pub(crate) delivery_target: String,
    pub(crate) state: WorkItemLifecycleView,
    pub(crate) focus: WorkItemFocusView,
    pub(crate) legacy_status: WorkItemStatus,
    pub(crate) is_current: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) progress_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan: Option<WorkPlanSnapshot>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkItemQueryContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current_work_item_id: Option<String>,
}

pub(crate) async fn query_context(runtime: &RuntimeHandle) -> Result<WorkItemQueryContext> {
    let state = runtime.agent_state().await?;
    let current_work_item_id = if state.current_turn_work_item_id.is_some() {
        state.current_turn_work_item_id
    } else {
        runtime
            .latest_work_items()
            .await?
            .into_iter()
            .filter(|item| item.status == WorkItemStatus::Active)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|item| item.id)
    };
    Ok(WorkItemQueryContext {
        current_work_item_id,
    })
}

pub(crate) async fn view_for_record(
    runtime: &RuntimeHandle,
    context: &WorkItemQueryContext,
    record: WorkItemRecord,
    include_plan: bool,
) -> Result<WorkItemView> {
    let is_current = context.current_work_item_id.as_deref() == Some(record.id.as_str())
        && record.status != WorkItemStatus::Completed;
    let legacy_status = record.status.clone();
    let plan = if include_plan {
        runtime.latest_work_plan(&record.id).await?
    } else {
        None
    };
    Ok(WorkItemView {
        id: record.id,
        agent_id: record.agent_id,
        workspace_id: record.workspace_id,
        delivery_target: record.delivery_target,
        state: lifecycle_view(&legacy_status),
        focus: focus_view(&legacy_status, is_current),
        legacy_status,
        is_current,
        parent_id: record.parent_id,
        summary: record.summary,
        progress_note: record.progress_note,
        plan,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

pub(crate) async fn latest_current_record(
    runtime: &RuntimeHandle,
    context: &WorkItemQueryContext,
) -> Result<Option<WorkItemRecord>> {
    if let Some(current_work_item_id) = context.current_work_item_id.as_deref() {
        if let Some(record) = runtime.latest_work_item(current_work_item_id).await? {
            if record.status != WorkItemStatus::Completed {
                return Ok(Some(record));
            }
        }
    }
    Ok(None)
}

pub(crate) fn lifecycle_view(status: &WorkItemStatus) -> WorkItemLifecycleView {
    match status {
        &WorkItemStatus::Completed => WorkItemLifecycleView::Done,
        &WorkItemStatus::Active | &WorkItemStatus::Queued | &WorkItemStatus::Waiting => {
            WorkItemLifecycleView::Open
        }
    }
}

pub(crate) fn focus_view(status: &WorkItemStatus, is_current: bool) -> WorkItemFocusView {
    if *status == WorkItemStatus::Completed {
        return WorkItemFocusView::Done;
    }
    if is_current {
        return WorkItemFocusView::Current;
    }
    match status {
        &WorkItemStatus::Waiting => WorkItemFocusView::Blocked,
        &WorkItemStatus::Active | &WorkItemStatus::Queued => WorkItemFocusView::Queued,
        &WorkItemStatus::Completed => WorkItemFocusView::Done,
    }
}
