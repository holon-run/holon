//! Durable blockers for upgrading retired scheduler tables.

use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetiredSchedulerCleanupBlockerKind {
    OpenExecutionAttempt,
    InFlightExecutionWorkItem,
    DequeuedQueueEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerCleanupBlocker {
    pub kind: RetiredSchedulerCleanupBlockerKind,
    pub agent_id: String,
    pub record_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerCleanupInventory {
    pub blockers: Vec<RetiredSchedulerCleanupBlocker>,
}

impl RetiredSchedulerCleanupInventory {
    pub fn is_fixed_point(&self) -> bool {
        self.blockers.is_empty()
    }

    pub fn open_execution_attempts(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::OpenExecutionAttempt)
    }

    pub fn in_flight_execution_work_items(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem)
    }

    pub fn dequeued_queue_entries(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry)
    }

    pub fn affected_agents(&self) -> Vec<String> {
        self.blockers
            .iter()
            .map(|blocker| blocker.agent_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn count(&self, kind: RetiredSchedulerCleanupBlockerKind) -> usize {
        self.blockers
            .iter()
            .filter(|blocker| blocker.kind == kind)
            .count()
    }
}

pub fn retired_scheduler_cleanup_inventory(
    connection: &Connection,
) -> Result<RetiredSchedulerCleanupInventory> {
    let mut blockers = Vec::new();
    collect_blockers(
        connection,
        "SELECT agent_id, attempt_id
         FROM execution_protocol_attempts
         WHERE lifecycle_state = 'open'
         ORDER BY agent_id, attempt_id",
        RetiredSchedulerCleanupBlockerKind::OpenExecutionAttempt,
        &mut blockers,
    )?;
    collect_blockers(
        connection,
        "SELECT agent_id, work_item_id
         FROM execution_protocol_work_items
         WHERE lifecycle_state = 'in_flight'
         ORDER BY agent_id, work_item_id",
        RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem,
        &mut blockers,
    )?;
    collect_blockers(
        connection,
        "SELECT agent_id, message_id
         FROM queue_entries
         WHERE status = 'dequeued'
         ORDER BY agent_id, message_id",
        RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry,
        &mut blockers,
    )?;
    Ok(RetiredSchedulerCleanupInventory { blockers })
}

fn collect_blockers(
    connection: &Connection,
    sql: &str,
    kind: RetiredSchedulerCleanupBlockerKind,
    blockers: &mut Vec<RetiredSchedulerCleanupBlocker>,
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    blockers.extend(
        statement
            .query_map([], |row| {
                Ok(RetiredSchedulerCleanupBlocker {
                    kind,
                    agent_id: row.get(0)?,
                    record_id: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    Ok(())
}
