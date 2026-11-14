//! Agent read models that retain their runtime budget guards.

use crate::types::{AgentLineageRecord, WaitingReason};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An array on the wire, retaining the original read phase until consumption.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AgentLineageChildrenSnapshot {
    pub(crate) records: Vec<AgentLineageRecord>,
    #[serde(skip)]
    pub(crate) _material:
        Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

impl AgentLineageChildrenSnapshot {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl std::ops::Deref for AgentLineageChildrenSnapshot {
    type Target = [AgentLineageRecord];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}

impl Clone for AgentLineageChildrenSnapshot {
    fn clone(&self) -> Self {
        let material = self._material.as_ref().map(|guard| guard.fork_owned());
        Self {
            records: self.records.clone(),
            _material: material,
        }
    }
}

impl PartialEq for AgentLineageChildrenSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.records == other.records
    }
}

impl Eq for AgentLineageChildrenSnapshot {}

impl PartialEq<Vec<AgentLineageRecord>> for AgentLineageChildrenSnapshot {
    fn eq(&self, other: &Vec<AgentLineageRecord>) -> bool {
        self.records == *other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentPhase {
    Running,
    Blocked,
    Waiting,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentBlockedReason {
    ManagedTaskQueued,
    ManagedTaskRunning,
    ManagedTaskCancelling,
    AwaitingManagedTask,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ChildAgentObservabilitySnapshot {
    pub phase: ChildAgentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<ChildAgentBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_brief: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result_brief: Option<String>,
    #[serde(skip)]
    pub(crate) _read_material:
        Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    #[serde(skip)]
    pub(crate) _source_material:
        Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

impl std::fmt::Debug for ChildAgentObservabilitySnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChildAgentObservabilitySnapshot")
            .field("phase", &self.phase)
            .field("blocked_reason", &self.blocked_reason)
            .field("waiting_reason", &self.waiting_reason)
            .field("current_work_item_id", &self.current_work_item_id)
            .field("work_summary", &self.work_summary)
            .field("last_progress_brief", &self.last_progress_brief)
            .field("last_result_brief", &self.last_result_brief)
            .finish()
    }
}

impl Clone for ChildAgentObservabilitySnapshot {
    fn clone(&self) -> Self {
        let material = self._read_material.as_ref().map(|guard| guard.fork_owned());
        let source = self
            ._source_material
            .as_ref()
            .map(|guard| guard.fork_owned());
        Self {
            phase: self.phase.clone(),
            blocked_reason: self.blocked_reason.clone(),
            waiting_reason: self.waiting_reason.clone(),
            current_work_item_id: self.current_work_item_id.clone(),
            work_summary: self.work_summary.clone(),
            last_progress_brief: self.last_progress_brief.clone(),
            last_result_brief: self.last_result_brief.clone(),
            _read_material: material,
            _source_material: source,
        }
    }
}

impl PartialEq for ChildAgentObservabilitySnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.phase == other.phase
            && self.blocked_reason == other.blocked_reason
            && self.waiting_reason == other.waiting_reason
            && self.current_work_item_id == other.current_work_item_id
            && self.work_summary == other.work_summary
            && self.last_progress_brief == other.last_progress_brief
            && self.last_result_brief == other.last_result_brief
    }
}
