use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::RuntimeHandle,
    types::{
        TodoItem, WorkItemPlanArtifact, WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord,
        WorkItemState,
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
    include_todo_list: bool,
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
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
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
