use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::RuntimeHandle,
    types::{WorkItemRecord, WorkItemState},
};

use super::work_item_action::WorkPlanView;

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
    pub(crate) is_current: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan: Option<WorkPlanView>,
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
    include_plan: bool,
) -> Result<WorkItemView> {
    let is_current = context.current_work_item_id.as_deref() == Some(record.id.as_str())
        && record.state == WorkItemState::Open;
    let plan = if include_plan {
        runtime.latest_work_plan(&record.id).await?.map(Into::into)
    } else {
        None
    };
    let state = lifecycle_view(&record.state);
    let focus = focus_view(&record, is_current);
    Ok(WorkItemView {
        id: record.id,
        agent_id: record.agent_id,
        workspace_id: record.workspace_id,
        delivery_target: record.delivery_target,
        state,
        focus,
        is_current,
        blocked_by: record.blocked_by,
        plan,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

pub(crate) fn lifecycle_view(state: &WorkItemState) -> WorkItemLifecycleView {
    match state {
        WorkItemState::Open => WorkItemLifecycleView::Open,
        WorkItemState::Done => WorkItemLifecycleView::Done,
    }
}

pub(crate) fn focus_view(record: &WorkItemRecord, is_current: bool) -> WorkItemFocusView {
    if record.state == WorkItemState::Done {
        return WorkItemFocusView::Done;
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
