//! SQLite operations for the runtime index outbox.

use anyhow::Result;
use chrono::{DateTime, Utc};
use holon_domain::MessageEnvelope;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// Runtime index operation (upsert or delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RuntimeIndexOperation {
    Upsert,
    Delete,
}

impl RuntimeIndexOperation {
    pub fn as_str(self) -> &'static str {
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    pub fn for_task(task: &holon_domain::TaskRecord) -> Self {
        Self {
            agent_id: task.agent_id.clone(),
            source_kind: "task".into(),
            source_id: task.id.clone(),
            source_ref: format!("task:{}", task.id),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(task.updated_at),
            reason: "task_written".into(),
        }
    }

    pub fn for_message(message: &MessageEnvelope) -> Self {
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

pub fn insert_runtime_index_changes_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
) -> Result<()> {
    insert_runtime_index_changes_with_time_tx(tx, changes, None)
}

pub fn insert_runtime_index_changes_at_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
    created_at: DateTime<Utc>,
) -> Result<()> {
    insert_runtime_index_changes_with_time_tx(tx, changes, Some(created_at))
}

fn insert_runtime_index_changes_with_time_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
    created_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let mut agent_ids = Vec::new();
    for change in changes {
        // Outbox inserts and the produced watermark advance below share one
        // transaction, so a committed append always advances its watermark.
        tx.execute(
            "INSERT INTO runtime_index_outbox (
                agent_id, source_kind, source_id, source_ref, operation,
                source_updated_at, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                change.agent_id,
                change.source_kind,
                change.source_id,
                change.source_ref,
                change.operation.as_str(),
                change.source_updated_at.map(timestamp),
                change.reason,
                timestamp(created_at.unwrap_or_else(Utc::now)),
            ],
        )?;
        if !agent_ids.contains(&change.agent_id) {
            agent_ids.push(change.agent_id.clone());
        }
    }
    for agent_id in &agent_ids {
        update_runtime_index_outbox_watermark_at_tx(
            tx,
            agent_id,
            created_at.unwrap_or_else(Utc::now),
        )?;
    }
    Ok(())
}

/// Advance the agent's monotonic produced watermark to the highest appended
/// `change_seq`. Deleting consumed outbox rows never lowers this value, which
/// keeps `produced - applied` meaningful after the outbox is drained.
fn update_runtime_index_outbox_watermark_at_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    let produced: i64 = tx.query_row(
        "SELECT COALESCE(MAX(change_seq), 0) FROM runtime_index_outbox WHERE agent_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO runtime_index_outbox_watermarks (
            agent_id, produced_change_seq, updated_at
         ) VALUES (?1, ?2, ?3)
         ON CONFLICT(agent_id) DO UPDATE SET
            produced_change_seq = MAX(
                runtime_index_outbox_watermarks.produced_change_seq,
                excluded.produced_change_seq
            ),
            updated_at = excluded.updated_at",
        params![agent_id, produced, timestamp(updated_at)],
    )?;
    Ok(())
}

pub fn agent_ids_with_pending_in(connection: &mut rusqlite::Connection) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT DISTINCT agent_id FROM runtime_index_outbox ORDER BY agent_id ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

pub fn agent_ids_with_watermarks_in(connection: &mut rusqlite::Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT agent_id FROM runtime_index_outbox_watermarks
                 UNION SELECT DISTINCT agent_id FROM runtime_index_outbox
                 ORDER BY agent_id ASC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

pub fn high_watermark_for_agent_in(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<i64> {
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

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn high_watermark_in(connection: &Connection) -> Result<i64> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(change_seq), 0) FROM runtime_index_outbox",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub fn produced_watermark_for_agent_in(connection: &Connection, agent_id: &str) -> Result<i64> {
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
    let current = high_watermark_for_agent_in(connection, agent_id)?;
    Ok(stored.unwrap_or(0).max(current))
}

pub fn pending_count_for_agent_in(
    connection: &Connection,
    agent_id: &str,
    after_change_seq: i64,
) -> Result<i64> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM runtime_index_outbox
             WHERE agent_id = ?1 AND change_seq > ?2",
            params![agent_id, after_change_seq],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub fn oldest_pending_created_at_for_agent_in(
    connection: &Connection,
    agent_id: &str,
    after_change_seq: i64,
) -> Result<Option<DateTime<Utc>>> {
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

pub fn read_after_in(
    connection: &Connection,
    agent_id: &str,
    after_change_seq: i64,
    limit: i64,
) -> Result<Vec<RuntimeIndexOutboxRow>> {
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

pub fn delete_through_in(
    tx: &Transaction<'_>,
    agent_id: &str,
    through_change_seq: i64,
) -> Result<usize> {
    tx.execute(
        "DELETE FROM runtime_index_outbox
         WHERE agent_id = ?1 AND change_seq <= ?2",
        params![agent_id, through_change_seq],
    )
    .map_err(Into::into)
}

pub fn delete_all_for_agent_in(connection: &Connection, agent_id: &str) -> Result<usize> {
    connection
        .execute(
            "DELETE FROM runtime_index_outbox WHERE agent_id = ?1",
            [agent_id],
        )
        .map_err(Into::into)
}

pub fn has_acknowledged_rows(
    connection: &Connection,
    agent_id: &str,
    through_change_seq: i64,
) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM runtime_index_outbox
             WHERE agent_id = ?1 AND change_seq <= ?2
             LIMIT 1",
            params![agent_id, through_change_seq],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}
