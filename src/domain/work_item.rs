//! Canonical WorkItem records and lifecycle values.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{agent_home_workspace_id, AGENT_HOME_WORKSPACE_ID};
use crate::ids;

fn default_agent_home_workspace_id() -> String {
    AGENT_HOME_WORKSPACE_ID.to_string()
}

fn default_work_item_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemState {
    Open,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemPlanStatus {
    Draft,
    Ready,
    NeedsInput,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemReadiness {
    Runnable,
    Yielded,
    WaitingForOperator,
    Blocked,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemSchedulingState {
    Runnable,
    YieldedToWorkItem,
    WaitingOperator,
    WaitingTask,
    WaitingExternal,
    WaitingTimer,
    WaitingSystem,
    Blocked,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemPlanArtifact {
    #[serde(default)]
    pub owner_agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_alias: Option<String>,
    #[serde(default)]
    pub relative_path: PathBuf,
    pub path: PathBuf,
    pub hash: String,
    pub bytes: u64,
    pub updated_at: DateTime<Utc>,
    pub preview: String,
    pub preview_complete: bool,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemRefKind {
    File,
    ToolExecution,
    Issue,
    Pr,
    Url,
    Memory,
    Task,
    Wait,
    Workspace,
    Other,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemRefStatus {
    Active,
    Resolved,
    Stale,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemRef {
    pub kind: WorkItemRefKind,
    #[serde(rename = "ref")]
    pub ref_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub reason: String,
    pub status: WorkItemRefStatus,
    pub last_seen_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TodoItem {
    pub text: String,
    pub state: TodoItemState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoItemState {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WorkItemRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    #[serde(default = "default_work_item_revision")]
    pub revision: u64,
    pub objective: String,
    pub state: WorkItemState,
    pub plan_status: WorkItemPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_artifact: Option<WorkItemPlanArtifact>,
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
    pub result_brief_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

impl WorkItemRecord {
    pub fn new(
        agent_id: impl Into<String>,
        objective: impl Into<String>,
        state: WorkItemState,
    ) -> Self {
        let now = Utc::now();
        let agent_id = agent_id.into();
        Self {
            id: ids::work_item_id(),
            workspace_id: agent_home_workspace_id(&agent_id),
            agent_id,
            revision: 1,
            objective: objective.into(),
            state,
            plan_status: WorkItemPlanStatus::Draft,
            plan_artifact: None,
            todo_list: Vec::new(),
            work_refs: Vec::new(),
            blocked_by: None,
            recheck_at: None,
            recheck_consumed_at: None,
            result_brief_id: None,
            result_summary: None,
            created_at: now,
            updated_at: now,
            turn_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemContinuationReturnPolicy {
    OnCompleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemContinuationState {
    Active,
    Resumed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemContinuationFrame {
    pub id: String,
    pub agent_id: String,
    pub suspended_work_item_id: String,
    pub active_work_item_id: String,
    pub return_policy: WorkItemContinuationReturnPolicy,
    pub state: WorkItemContinuationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

impl WorkItemContinuationFrame {
    pub fn new_on_completed(
        agent_id: impl Into<String>,
        suspended_work_item_id: impl Into<String>,
        active_work_item_id: impl Into<String>,
        turn_id: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: ids::work_item_continuation_id(),
            agent_id: agent_id.into(),
            suspended_work_item_id: suspended_work_item_id.into(),
            active_work_item_id: active_work_item_id.into(),
            return_policy: WorkItemContinuationReturnPolicy::OnCompleted,
            state: WorkItemContinuationState::Active,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            cancelled_at: None,
            resolution_reason: None,
            turn_id,
        }
    }

    pub fn resume(mut self, reason: impl Into<String>) -> Self {
        let now = Utc::now();
        self.state = WorkItemContinuationState::Resumed;
        self.updated_at = now;
        self.resolved_at = Some(now);
        self.cancelled_at = None;
        self.resolution_reason = Some(reason.into());
        self
    }

    pub fn cancel(mut self, reason: impl Into<String>) -> Self {
        let now = Utc::now();
        self.state = WorkItemContinuationState::Cancelled;
        self.updated_at = now;
        self.cancelled_at = Some(now);
        self.resolved_at = None;
        self.resolution_reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemDelegationState {
    Open,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemDelegationRecord {
    pub delegation_id: String,
    pub parent_agent_id: String,
    pub parent_work_item_id: String,
    pub child_agent_id: String,
    pub child_work_item_id: String,
    pub state: WorkItemDelegationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkItemDelegationRecord {
    pub fn new(
        parent_agent_id: impl Into<String>,
        parent_work_item_id: impl Into<String>,
        child_agent_id: impl Into<String>,
        child_work_item_id: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            delegation_id: ids::work_item_delegation_id(),
            parent_agent_id: parent_agent_id.into(),
            parent_work_item_id: parent_work_item_id.into(),
            child_agent_id: child_agent_id.into(),
            child_work_item_id: child_work_item_id.into(),
            state: WorkItemDelegationState::Open,
            result_summary: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_types_path_reexports_canonical_work_item_types() {
        let state: crate::types::WorkItemState = WorkItemState::Open;
        let todo: crate::types::TodoItem = TodoItem {
            text: "ship".into(),
            state: TodoItemState::Pending,
        };

        assert_eq!(state, WorkItemState::Open);
        assert_eq!(todo.state, TodoItemState::Pending);
    }

    #[test]
    fn legacy_work_item_defaults_and_wire_names_remain_stable() {
        let record: WorkItemRecord = serde_json::from_value(serde_json::json!({
            "id": "work_legacy",
            "session_id": "default",
            "objective": "ship",
            "state": "open",
            "plan_status": "needs_input",
            "created_at": "2026-04-20T00:00:00Z",
            "updated_at": "2026-04-20T00:00:00Z"
        }))
        .unwrap();

        assert_eq!(record.workspace_id, AGENT_HOME_WORKSPACE_ID);
        assert_eq!(record.revision, 1);
        assert_eq!(record.plan_status, WorkItemPlanStatus::NeedsInput);
        let serialized = serde_json::to_value(&record).unwrap();
        assert_eq!(serialized["agent_id"], "default");
        assert!(serialized.get("session_id").is_none());
        assert_eq!(
            serde_json::to_value(WorkItemSchedulingState::YieldedToWorkItem).unwrap(),
            serde_json::json!("yielded_to_work_item")
        );
    }

    #[test]
    fn legacy_types_path_preserves_work_item_json_schema() {
        let canonical = schemars::schema_for!(WorkItemRecord);
        let legacy = schemars::schema_for!(crate::types::WorkItemRecord);

        assert_eq!(
            serde_json::to_value(canonical).unwrap(),
            serde_json::to_value(legacy).unwrap()
        );
    }
}
