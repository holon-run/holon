//! Runtime index outbox repository implementation.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::runtime_db::evidence::insert_runtime_index_changes_tx;
use crate::runtime_db::RuntimeDb;
use crate::types::MessageEnvelope;

/// Runtime index outbox repository.
pub struct RuntimeIndexOutboxRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

/// Runtime index operation (upsert or delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeIndexOperation {
    Upsert,
    Delete,
}

impl RuntimeIndexOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "delete" => Self::Delete,
            _ => Self::Upsert,
        }
    }
}

/// Runtime index change event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIndexChange {
    pub agent_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: String,
    pub operation: RuntimeIndexOperation,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub reason: String,
}

impl RuntimeIndexChange {
    pub(crate) fn for_message(message: &MessageEnvelope) -> Self {
        Self {
            agent_id: message.agent_id.clone(),
            source_kind: "message".into(),
            source_id: message.id.clone(),
            source_ref: format!("message:{}", message.id),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(message.created_at),
            reason: "message_written".into(),
        }
    }
}

/// Runtime index outbox row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIndexOutboxRow {
    pub change_seq: i64,
    pub agent_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: String,
    pub operation: RuntimeIndexOperation,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl RuntimeIndexOutboxRepository<'_> {
    pub fn append_changes(&self, changes: &[RuntimeIndexChange]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        self.db
            .transaction(|tx| insert_runtime_index_changes_tx(tx, changes))
    }

    pub fn high_watermark(&self) -> Result<i64> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(MAX(change_seq), 0) FROM runtime_index_outbox",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Return distinct agent ids that have at least one pending outbox row,
    /// ordered for deterministic processing.
    ///
    /// Can be served by the `idx_runtime_index_outbox_agent_seq` covering index,
    /// though the planner ultimately decides the access path.
    pub fn agent_ids_with_pending(&self) -> Result<Vec<String>> {
        let connection = self.db.connection()?;
        let mut statement = connection
            .prepare("SELECT DISTINCT agent_id FROM runtime_index_outbox ORDER BY agent_id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn high_watermark_for_agent(&self, agent_id: &str) -> Result<i64> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(MAX(change_seq), 0)
                 FROM runtime_index_outbox
                 WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Monotonic produced watermark: the highest `change_seq` ever appended
    /// for the agent in this runtime database. Unlike
    /// [`Self::high_watermark_for_agent`], this does not fall back to 0 when
    /// consumed rows are deleted. Rows appended before the watermark table
    /// existed are still covered by taking the max with the current outbox.
    pub fn produced_watermark_for_agent(&self, agent_id: &str) -> Result<i64> {
        let connection = self.db.connection()?;
        let stored: Option<i64> = connection
            .query_row(
                "SELECT produced_change_seq
                 FROM runtime_index_outbox_watermarks
                 WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(anyhow::Error::from)?;
        let current = self.high_watermark_for_agent(agent_id)?;
        Ok(stored.unwrap_or(0).max(current))
    }

    /// Exact number of pending outbox rows for the agent. Sequence distance
    /// between cursors cannot substitute for this: `change_seq` is a global
    /// autoincrement shared across agents. Only rows above the agent's
    /// applied cursor count as pending; rows at or below it are already
    /// acknowledged and merely await GC.
    pub fn pending_count_for_agent(&self, agent_id: &str, after_change_seq: i64) -> Result<i64> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_index_outbox
                 WHERE agent_id = ?1 AND change_seq > ?2",
                params![agent_id, after_change_seq],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// `created_at` of the oldest pending outbox row for the agent, the real
    /// propagation-delay anchor for index lag observability.
    pub fn oldest_pending_created_at_for_agent(
        &self,
        agent_id: &str,
        after_change_seq: i64,
    ) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let created_at: Option<String> = connection
            .query_row(
                "SELECT MIN(created_at) FROM runtime_index_outbox
                 WHERE agent_id = ?1 AND change_seq > ?2",
                params![agent_id, after_change_seq],
                |row| row.get(0),
            )
            .optional()
            .map_err(anyhow::Error::from)?
            .flatten();
        created_at
            .map(|value| DateTime::parse_from_rfc3339(&value).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
                .into()
            })
    }

    pub fn read_after(
        &self,
        agent_id: &str,
        after_change_seq: i64,
        limit: usize,
    ) -> Result<Vec<RuntimeIndexOutboxRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT change_seq, agent_id, source_kind, source_id, source_ref, operation,
                    source_updated_at, reason, created_at
             FROM runtime_index_outbox
             WHERE agent_id = ?1 AND change_seq > ?2
             ORDER BY change_seq ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![agent_id, after_change_seq, limit], |row| {
            let source_updated_at: Option<String> = row.get(6)?;
            let created_at: String = row.get(8)?;
            Ok(RuntimeIndexOutboxRow {
                change_seq: row.get(0)?,
                agent_id: row.get(1)?,
                source_kind: row.get(2)?,
                source_id: row.get(3)?,
                source_ref: row.get(4)?,
                operation: RuntimeIndexOperation::parse(&row.get::<_, String>(5)?),
                source_updated_at: source_updated_at
                    .as_deref()
                    .map(DateTime::parse_from_rfc3339)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
                    .map(|dt| dt.with_timezone(&Utc)),
                reason: row.get(7)?,
                created_at: DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
                    .with_timezone(&Utc),
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn delete_through(&self, agent_id: &str, through_change_seq: i64) -> Result<usize> {
        self.db.transaction(|tx| {
            tx.execute(
                "DELETE FROM runtime_index_outbox
                 WHERE agent_id = ?1 AND change_seq <= ?2",
                params![agent_id, through_change_seq],
            )
            .map_err(Into::into)
        })
    }

    /// Best-effort GC for acknowledged rows at or below `through_change_seq`.
    /// Returns the number of rows removed without opening a write transaction
    /// when no such row exists. Callers use it after the applied cursor has
    /// already covered those rows: either directly after consumption, after a
    /// full rebuild jumped the cursor, or to compensate a crash between apply
    /// and delete.
    pub fn delete_acknowledged_through(
        &self,
        agent_id: &str,
        through_change_seq: i64,
    ) -> Result<usize> {
        let connection = self.db.connection()?;
        let has_acknowledged_rows: bool = connection
            .query_row(
                "SELECT 1 FROM runtime_index_outbox
                 WHERE agent_id = ?1 AND change_seq <= ?2
                 LIMIT 1",
                params![agent_id, through_change_seq],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_acknowledged_rows {
            return Ok(0);
        }
        self.delete_through(agent_id, through_change_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn runtime_db() -> (tempfile::TempDir, RuntimeDb) {
        let dir = tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )
        .unwrap();
        (dir, db)
    }

    fn change(agent_id: &str, id: &str) -> RuntimeIndexChange {
        RuntimeIndexChange {
            agent_id: agent_id.into(),
            source_kind: "brief".into(),
            source_id: id.into(),
            source_ref: format!("brief:{id}"),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(Utc::now()),
            reason: "test_watermark".into(),
        }
    }

    #[test]
    fn produced_watermark_survives_outbox_gc_and_stays_monotonic() {
        let (_dir, db) = runtime_db();
        let outbox = db.runtime_index_outbox();
        outbox
            .append_changes(&[
                change("agent-a", "1"),
                change("agent-b", "2"),
                change("agent-a", "3"),
            ])
            .unwrap();
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 3);
        assert_eq!(outbox.produced_watermark_for_agent("agent-b").unwrap(), 2);
        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 2);
        assert_eq!(outbox.pending_count_for_agent("agent-b", 0).unwrap(), 1);

        // Draining an agent's outbox resets the row max but not the produced
        // watermark: `produced - applied` must stay meaningful after GC.
        outbox.delete_through("agent-a", 3).unwrap();
        assert_eq!(outbox.high_watermark_for_agent("agent-a").unwrap(), 0);
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 3);
        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 0);

        // Later appends only move the watermark forward.
        outbox.append_changes(&[change("agent-a", "4")]).unwrap();
        assert_eq!(outbox.produced_watermark_for_agent("agent-a").unwrap(), 4);
    }

    #[test]
    fn pending_count_and_oldest_pending_track_per_agent_backlog() {
        let (_dir, db) = runtime_db();
        let outbox = db.runtime_index_outbox();
        outbox
            .append_changes(&[change("agent-a", "1"), change("agent-b", "2")])
            .unwrap();

        assert_eq!(outbox.pending_count_for_agent("agent-a", 0).unwrap(), 1);
        assert_eq!(outbox.pending_count_for_agent("agent-c", 0).unwrap(), 0);
        let oldest = outbox
            .oldest_pending_created_at_for_agent("agent-a", 0)
            .unwrap()
            .expect("pending row has created_at");
        assert!(oldest <= Utc::now());
        assert_eq!(
            outbox
                .oldest_pending_created_at_for_agent("agent-c", 0)
                .unwrap(),
            None
        );

        // Rows at or below the applied cursor are acknowledged garbage, not
        // backlog, even before GC removes them.
        assert_eq!(outbox.pending_count_for_agent("agent-a", 1).unwrap(), 0);
        assert_eq!(
            outbox
                .oldest_pending_created_at_for_agent("agent-a", 1)
                .unwrap(),
            None
        );
        assert_eq!(outbox.delete_acknowledged_through("agent-a", 1).unwrap(), 1);
        assert_eq!(outbox.delete_acknowledged_through("agent-a", 1).unwrap(), 0);
    }
}
