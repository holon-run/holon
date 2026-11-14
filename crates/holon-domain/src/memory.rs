use super::serde_helpers::default_agent_home_workspace_id;
use super::state::AgentState;
use crate::ids;
use crate::work_item::TodoItem;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkingMemorySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todo_list: Vec<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeBoundaryReason {
    ActiveWorkSwitched,
    WaitBoundary,
    #[serde(rename = "result_checkpoint")]
    LegacyResultCheckpoint,
    TaskRejoined,
    HardTurnCap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveEpisodeBuilder {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub start_turn_index: u64,
    pub latest_turn_index: u64,
    // Retained for legacy payloads. New builders leave both counts absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry_forward: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_turn_ids: Vec<String>,
}

impl ActiveEpisodeBuilder {
    pub fn new(agent: &AgentState, snapshot: &WorkingMemorySnapshot) -> Self {
        Self::new_with_start(agent.id.clone(), snapshot, agent.turn_index.max(1))
    }

    pub fn new_with_start(
        _agent_id: impl Into<String>,
        snapshot: &WorkingMemorySnapshot,
        start_turn_index: u64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: ids::episode_id(),
            started_at: now,
            start_turn_index,
            latest_turn_index: start_turn_index,
            start_message_count: None,
            latest_message_count: None,
            current_work_item_id: snapshot.current_work_item_id.clone(),
            objective: snapshot.objective.clone(),
            work_summary: snapshot.work_summary.clone(),
            scope_hints: snapshot.scope_hints.clone(),
            working_set_files: snapshot.working_set_files.clone(),
            carry_forward: snapshot.pending_followups.clone(),
            waiting_on: snapshot.waiting_on.clone(),
            source_turn_ids: Vec::new(),
        }
    }
}

fn deserialize_source_turn_ids<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| match value {
            Value::String(value) => Some(value),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextEpisodeGeneratedBy {
    pub component: String,
    pub reason: EpisodeBoundaryReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextEpisodeRecord {
    pub id: String,
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    pub created_at: DateTime<Utc>,
    pub finalized_at: DateTime<Utc>,
    pub start_turn_index: u64,
    pub end_turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_message_count: Option<usize>,
    pub boundary_reason: EpisodeBoundaryReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_source_turn_ids",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub source_turn_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<ContextEpisodeGeneratedBy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry_forward: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkingMemoryState {
    #[serde(default)]
    pub compression_epoch: u64,
    #[serde(default)]
    pub current_working_memory: WorkingMemorySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_episode_id: Option<String>,
    #[serde(default)]
    pub archived_episode_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_episode_builder: Option<ActiveEpisodeBuilder>,
}
