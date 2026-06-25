//! Runtime index outbox repository implementation.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::runtime_db::evidence::insert_runtime_index_changes_tx;
use crate::runtime_db::RuntimeDb;

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
}
