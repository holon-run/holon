//! Durable storage for user reports of user-visible AI content.

use std::fmt;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

use crate::evidence::content_hash;
use holon_domain::ids;

pub const MAX_DESCRIPTION_CHARS: usize = 2_000;
const MAX_SNAPSHOT_CHARS: usize = 64 * 1024;
pub const MAX_IDENTIFIER_CHARS: usize = 256;
pub const MAX_CLIENT_REQUEST_ID_CHARS: usize = 128;
#[doc(hidden)]
pub const REPORT_RATE_LIMIT: i64 = 10;
const REPORT_RATE_WINDOW_SECONDS: i64 = 60 * 60;

pub const REPORT_CATEGORIES: &[&str] = &[
    "harmful_or_abusive",
    "sexual_content",
    "hate_or_harassment",
    "self_harm",
    "violence",
    "privacy",
    "spam_or_other",
];

#[derive(Debug, Clone)]
pub struct NewContentReport {
    pub reporter_principal: String,
    pub agent_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub category: String,
    pub description: Option<String>,
    pub client_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentReportRecord {
    pub report_id: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug)]
pub enum ContentReportError {
    RateLimited,
    TargetNotFound,
    TargetNotReportable,
}

impl fmt::Display for ContentReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => f.write_str("content report rate limit exceeded"),
            Self::TargetNotFound | Self::TargetNotReportable => {
                f.write_str("reported message was not found")
            }
        }
    }
}

impl std::error::Error for ContentReportError {}

pub fn create_tx(tx: &Transaction<'_>, report: &NewContentReport) -> Result<ContentReportRecord> {
    let existing = if let Some(client_request_id) = report.client_request_id.as_deref() {
        tx.query_row(
            "SELECT report_id, status, created_at
             FROM content_reports
             WHERE reporter_principal = ?1
               AND agent_id = ?2
               AND turn_id = ?3
               AND message_id = ?4
               AND client_request_id = ?5",
            params![
                report.reporter_principal,
                report.agent_id,
                report.turn_id,
                report.message_id,
                client_request_id
            ],
            report_record_from_row,
        )
        .optional()?
    } else {
        tx.query_row(
            "SELECT report_id, status, created_at
             FROM content_reports
             WHERE reporter_principal = ?1
               AND agent_id = ?2
               AND turn_id = ?3
               AND message_id = ?4
               AND client_request_id IS NULL
               AND category = ?5",
            params![
                report.reporter_principal,
                report.agent_id,
                report.turn_id,
                report.message_id,
                report.category
            ],
            report_record_from_row,
        )
        .optional()?
    };
    if let Some(existing) = existing {
        return Ok(existing);
    }

    let visible = tx
        .query_row(
            "SELECT 1
             FROM agent_identities
             WHERE agent_id = ?1
               AND status = 'active'
               AND visibility = 'public'",
            [&report.agent_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !visible {
        return Err(ContentReportError::TargetNotFound.into());
    }

    let target = tx
        .query_row(
            "SELECT turn_id, kind, created_at, payload_json
             FROM transcript_entries
             WHERE agent_id = ?1
               AND turn_id = ?2
               AND evidence_id = ?3",
            params![report.agent_id, report.turn_id, report.message_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(ContentReportError::TargetNotFound)?;

    if target.0.as_deref() != Some(report.turn_id.as_str())
        || !matches!(
            target.1.as_str(),
            "assistant_round" | "subagent_assistant_round"
        )
    {
        return Err(ContentReportError::TargetNotReportable.into());
    }
    let payload: Value = serde_json::from_str(&target.3)?;
    let data = payload.get("data").unwrap_or(&payload);
    let snapshot = reportable_snapshot(data).ok_or(ContentReportError::TargetNotReportable)?;
    let (content_snapshot, snapshot_truncated) = bounded_snapshot(&snapshot);
    let origin_json = data.get("origin").cloned().unwrap_or(Value::Null);
    let origin_json = serde_json::to_string(&origin_json)?;
    let authority_class = data
        .get("authority_class")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let recent_reports: i64 = tx.query_row(
        "SELECT COUNT(*)
         FROM content_reports
         WHERE reporter_principal = ?1
           AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)",
        params![
            report.reporter_principal,
            format!("-{REPORT_RATE_WINDOW_SECONDS} seconds")
        ],
        |row| row.get(0),
    )?;
    if recent_reports >= REPORT_RATE_LIMIT {
        return Err(ContentReportError::RateLimited.into());
    }

    let report_id = ids::runtime_id("report");
    tx.execute(
        "INSERT INTO content_reports (
            report_id, reporter_principal, agent_id, turn_id, message_id,
            category, description, content_snapshot, content_snapshot_hash,
            snapshot_truncated, source_message_created_at, source_origin_json,
            source_authority_class, status, client_request_id, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'received',
            ?14, ?15, ?15
         )",
        params![
            report_id,
            report.reporter_principal,
            report.agent_id,
            report.turn_id,
            report.message_id,
            report.category,
            report.description,
            content_snapshot,
            content_hash(&content_snapshot),
            snapshot_truncated,
            target.2,
            origin_json,
            authority_class,
            report.client_request_id,
            now,
        ],
    )?;
    Ok(ContentReportRecord {
        report_id,
        status: "received".to_string(),
        created_at: now,
    })
}

fn report_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContentReportRecord> {
    Ok(ContentReportRecord {
        report_id: row.get(0)?,
        status: row.get(1)?,
        created_at: row.get(2)?,
    })
}

fn reportable_snapshot(payload: &Value) -> Option<String> {
    if payload
        .get("visibility")
        .and_then(Value::as_str)
        .is_some_and(|visibility| visibility != "operator_visible")
    {
        return None;
    }
    if let Some(blocks) = payload.get("blocks").and_then(Value::as_array) {
        let text = blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        return (!text.is_empty()).then_some(text);
    }
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    let body = payload.get("body")?;
    match body.get("type").and_then(Value::as_str)? {
        "text" | "brief" => body.get("text").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn bounded_snapshot(snapshot: &str) -> (String, bool) {
    let mut chars = snapshot.chars();
    let bounded: String = chars.by_ref().take(MAX_SNAPSHOT_CHARS).collect();
    let truncated = chars.next().is_some();
    (bounded, truncated)
}

#[cfg(test)]
mod tests;
