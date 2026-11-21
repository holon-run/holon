//! Batched read facts for the `/agents/list` projection.
//!
//! Assembling one `AgentListEntry` per public active agent needs per-agent
//! facts from six record families (committed AgentState, queued input,
//! work items, external triggers, wait conditions, continuation frames).
//! Reading them per agent opens one SQLite connection per small query
//! (N+1); this module reads every family for every requested agent
//! through one connection inside one deferred read transaction, so a list
//! response observes a single committed database view with O(1)
//! connection churn.
//!
//! Decode failures are captured per agent instead of failing the whole
//! read: `/agents/list` degrades that one agent to placeholder facts
//! (matching the previous per-agent read-error behavior) while the rest of
//! the roster stays servable.

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "agent_list_tests.rs"]
mod tests;

use anyhow::Result;
use holon_storage::agent_registry::list;
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::agent_registry::list::read_agent_record_snapshot;
pub use holon_storage::agent_registry::list::{
    AgentListPostureRow, AgentListPostureRows, AgentPostureFacts,
};
#[cfg(test)]
use rusqlite::Connection;

use crate::storage::read_models::WORK_QUEUE_NON_OPEN_WINDOW;
use crate::types::{TaskRecord, TimerRecord, ToolExecutionRecord};

use super::conversation::ConversationIdentity;
use super::RuntimeDb;

impl RuntimeDb {
    pub(crate) fn agent_tasks_snapshot(
        &self,
        agent_id: &str,
        expected_incarnation: Option<u64>,
        terminal: bool,
        limit: usize,
    ) -> Result<Option<(ConversationIdentity, Vec<TaskRecord>)>> {
        self.with_read(|connection| {
            list::agent_tasks_snapshot(connection, agent_id, expected_incarnation, terminal, limit)
        })
    }

    #[cfg(test)]
    pub(crate) fn agent_timers_snapshot(
        &self,
        agent_id: &str,
        expected_incarnation: Option<u64>,
        limit: usize,
    ) -> Result<Option<(ConversationIdentity, Vec<TimerRecord>)>> {
        self.agent_timers_snapshot_filtered(
            agent_id,
            expected_incarnation,
            limit,
            crate::types::RecordListStatus::All,
        )
    }

    pub(crate) fn agent_timers_snapshot_filtered(
        &self,
        agent_id: &str,
        expected_incarnation: Option<u64>,
        limit: usize,
        status: crate::types::RecordListStatus,
    ) -> Result<Option<(ConversationIdentity, Vec<TimerRecord>)>> {
        self.with_read(|connection| {
            list::read_agent_record_snapshot(connection, agent_id, expected_incarnation, |view| {
                super::repositories::filtered_timers_for_agent(view, agent_id, limit, status)
            })
        })
    }

    pub(crate) fn agent_work_queue_snapshot_filtered(
        &self,
        agent_id: &str,
        expected_incarnation: Option<u64>,
        status: crate::types::RecordListStatus,
        limit: usize,
    ) -> Result<Option<(ConversationIdentity, AgentListPostureRow)>> {
        self.with_read(|connection| {
            list::read_work_queue_snapshot_filtered(
                connection,
                agent_id,
                expected_incarnation,
                || {},
                status,
                limit,
            )
        })
    }

    /// Captures the work queue's identity and all facts in one committed view.
    #[cfg(test)]
    pub(crate) fn agent_work_queue_snapshot(
        &self,
        agent_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<Option<(ConversationIdentity, AgentListPostureRow)>> {
        self.with_read(|connection| {
            read_work_queue_snapshot(connection, agent_id, expected_incarnation, || {})
        })
    }

    /// One committed view of the agent identity fence plus one work item.
    pub(crate) fn agent_work_item_snapshot(
        &self,
        agent_id: &str,
        work_item_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<Option<(ConversationIdentity, Option<AgentListPostureRow>)>> {
        self.with_read(|connection| {
            list::agent_work_item_snapshot(connection, agent_id, work_item_id, expected_incarnation)
        })
    }

    /// One committed view of the agent identity fence plus the latest task record.
    pub(crate) fn agent_task_snapshot(
        &self,
        agent_id: &str,
        task_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<Option<(ConversationIdentity, Option<TaskRecord>)>> {
        self.with_read(|connection| {
            list::agent_task_snapshot(connection, agent_id, task_id, expected_incarnation)
        })
    }

    /// One committed view of the agent identity fence plus one tool execution.
    pub(crate) fn agent_tool_execution_snapshot(
        &self,
        agent_id: &str,
        tool_execution_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<Option<(ConversationIdentity, Option<ToolExecutionRecord>)>> {
        self.with_read(|connection| {
            list::agent_tool_execution_snapshot(
                connection,
                agent_id,
                tool_execution_id,
                expected_incarnation,
            )
        })
    }

    /// Reads posture facts for every requested agent through one
    /// connection inside one deferred read transaction. Returns exactly
    /// one row per requested agent id (empty facts when the agent has no
    /// rows in a family). An empty request opens no connection.
    pub fn agent_list_posture_rows(&self, agent_ids: &[String]) -> Result<AgentListPostureRows> {
        if agent_ids.is_empty() {
            return Ok(AgentListPostureRows { rows: Vec::new() });
        }
        let started_at = std::time::Instant::now();
        let rows = self.with_read(|connection| {
            list::read_posture_rows(connection, agent_ids, WORK_QUEUE_NON_OPEN_WINDOW)
        })?;
        crate::diagnostics::record_agent_list_projection(started_at.elapsed());
        Ok(rows)
    }

    #[cfg(test)]
    pub(crate) async fn agent_list_posture_rows_async(
        &self,
        agent_ids: Vec<String>,
    ) -> Result<AgentListPostureRows> {
        if agent_ids.is_empty() {
            return Ok(AgentListPostureRows { rows: Vec::new() });
        }
        let started_at = std::time::Instant::now();
        let rows = self
            .with_read_async(move |connection| {
                list::read_posture_rows(connection, &agent_ids, WORK_QUEUE_NON_OPEN_WINDOW)
            })
            .await?;
        crate::diagnostics::record_agent_list_projection(started_at.elapsed());
        Ok(rows)
    }
}

#[cfg(test)]
fn read_work_queue_snapshot(
    connection: &mut Connection,
    agent_id: &str,
    expected_incarnation: Option<u64>,
    after_identity: impl FnOnce(),
) -> Result<Option<(ConversationIdentity, AgentListPostureRow)>> {
    list::read_work_queue_snapshot_filtered(
        connection,
        agent_id,
        expected_incarnation,
        after_identity,
        crate::types::RecordListStatus::All,
        WORK_QUEUE_NON_OPEN_WINDOW,
    )
}
