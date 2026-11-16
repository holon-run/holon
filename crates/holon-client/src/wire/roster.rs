use chrono::{DateTime, Utc};
use holon_domain::AgentListEntry;
use serde::{Deserialize, Serialize};

/// Authoritative roster snapshot for membership, authorization visibility,
/// and per-Agent event-window anchors. All-or-nothing: a failure to assemble
/// one Agent fails the whole response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRosterSnapshot {
    pub contract_version: u32,
    /// Stable public identity of the runtime installation. Distinguishes a
    /// replaced server from an ordinary restart. Not a secret.
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    /// Active public Agents visible to the caller. Private child Agents are
    /// never included.
    pub agents: Vec<AgentRosterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRosterEntry {
    /// Same entry shape as `GET /api/agents/list`.
    pub agent: AgentListEntry,
    pub event_window: AgentEventWindow,
    /// Latest canonical Brief; `null` when the Agent has no Brief yet.
    pub latest_brief: Option<AgentLatestBrief>,
}

/// Committed event window for one Agent inside the snapshot read view. Both
/// values must come from the same committed database view, never from an
/// in-memory watcher or a sequence allocator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEventWindow {
    /// Greatest committed `event_seq` visible in the response read view.
    pub event_head_seq: u64,
    /// Retention floor; `0` means no retained prefix has ever been deleted.
    pub oldest_retained_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentLatestBrief {
    pub brief_id: String,
    /// `brief_created` event sequence linkage. `null` when the Brief cannot
    /// be linked to a unique retained event; such Briefs do not participate
    /// in exact unread calculation.
    pub created_event_seq: Option<u64>,
    pub created_at: DateTime<Utc>,
    /// Bounded preview, at most `LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES`
    /// UTF-8 bytes. Full text remains available from the Brief APIs.
    pub preview: String,
}
