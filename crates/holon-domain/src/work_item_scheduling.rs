//! Shared WorkItem scheduling projection. The runtime owns its derivation.
use crate::{
    TodoItem, WaitConditionSummary, WorkItemReadiness, WorkItemRecord, WorkItemSchedulingState,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Borrow,
    ops::{Deref, DerefMut},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemCandidateClass {
    CurrentRunnable,
    TriggeredBlocked,
    QueuedRunnable,
    WaitingForOperator,
    Yielded,
    Blocked,
    Completing,
    CompletedRecent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemFocus {
    Current,
    Queued,
    Yielded,
    Blocked,
    Completing,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemSchedulingReasonCode {
    Completing,
    Completed,
    ContinuationYielded,
    ActiveTaskWait,
    ActiveOperatorWait,
    ActiveTimerWait,
    ActiveExternalWait,
    ActiveSystemWait,
    ManualBlocker,
    PlanNeedsInput,
    Runnable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemSchedulingProjection<W = WorkItemRecord> {
    #[serde(flatten)]
    pub work_item: W,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub focus: WorkItemFocus,
    pub is_current: bool,
    pub is_runnable: bool,
    pub has_active_waits: bool,
    pub has_active_task_waits: bool,
    pub has_triggered_waits: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_triggered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_todo: Option<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_wait_conditions: Vec<WaitConditionSummary>,
    pub reason_code: WorkItemSchedulingReasonCode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl<W: Borrow<WorkItemRecord>> WorkItemSchedulingProjection<W> {
    pub fn record(&self) -> &WorkItemRecord {
        self.work_item.borrow()
    }

    pub fn posture_reason(&self) -> String {
        self.posture_reason_display().to_string()
    }

    pub fn posture_reason_display(&self) -> impl std::fmt::Display + '_ {
        WorkItemPostureReason {
            id: &self.record().id,
            is_current: self.is_current,
            scheduling_state: self.scheduling_state,
        }
    }
}

pub struct WorkItemPostureReason<'a> {
    pub id: &'a str,
    pub is_current: bool,
    pub scheduling_state: WorkItemSchedulingState,
}

impl std::fmt::Display for WorkItemPostureReason<'_> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = if self.is_current {
            "current WorkItem"
        } else {
            "queued WorkItem"
        };
        write!(output, "{label} {} is {:?}", self.id, self.scheduling_state)
    }
}

impl<W: Borrow<WorkItemRecord>> Deref for WorkItemSchedulingProjection<W> {
    type Target = WorkItemRecord;

    fn deref(&self) -> &Self::Target {
        self.record()
    }
}

impl DerefMut for WorkItemSchedulingProjection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.work_item
    }
}

impl<W> WorkItemSchedulingProjection<W> {
    #[doc(hidden)]
    pub fn try_map_record<R>(
        self,
        map: impl FnOnce(W) -> anyhow::Result<R>,
    ) -> anyhow::Result<WorkItemSchedulingProjection<R>> {
        Ok(WorkItemSchedulingProjection {
            work_item: map(self.work_item)?,
            scheduling_state: self.scheduling_state,
            readiness: self.readiness,
            candidate_class: self.candidate_class,
            focus: self.focus,
            is_current: self.is_current,
            is_runnable: self.is_runnable,
            has_active_waits: self.has_active_waits,
            has_active_task_waits: self.has_active_task_waits,
            has_triggered_waits: self.has_triggered_waits,
            last_triggered_at: self.last_triggered_at,
            current_todo: self.current_todo,
            active_wait_conditions: self.active_wait_conditions,
            reason_code: self.reason_code,
            diagnostics: self.diagnostics,
        })
    }
}
