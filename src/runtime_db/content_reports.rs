//! Durable storage for user reports of user-visible AI content.

use std::fmt;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

use crate::ids;
use crate::runtime_db::evidence::content_hash;
use crate::runtime_db::types::ContentReportRepository;

pub(crate) const MAX_DESCRIPTION_CHARS: usize = 2_000;
pub(crate) const MAX_SNAPSHOT_CHARS: usize = 64 * 1024;
pub(crate) const MAX_IDENTIFIER_CHARS: usize = 256;
pub(crate) const MAX_CLIENT_REQUEST_ID_CHARS: usize = 128;
pub(crate) const REPORT_RATE_LIMIT: i64 = 10;
pub(crate) const REPORT_RATE_WINDOW_SECONDS: i64 = 60 * 60;

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
pub(crate) enum ContentReportError {
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

impl<'a> ContentReportRepository<'a> {
    pub fn create(&self, report: NewContentReport) -> Result<ContentReportRecord> {
        self.db.transaction(|tx| create_tx(tx, &report))
    }
}

fn create_tx(tx: &Transaction<'_>, report: &NewContentReport) -> Result<ContentReportRecord> {
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
mod tests {
    use super::*;
    use crate::runtime_db::RuntimeDb;
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        MessageOrigin, TranscriptEntry, TranscriptEntryKind,
    };
    use tempfile::TempDir;

    const AGENT_ID: &str = "agent-content-report-test";

    fn test_db() -> anyhow::Result<(TempDir, RuntimeDb)> {
        let temp_dir = tempfile::tempdir()?;
        let state_dir = temp_dir.path().join("state");
        std::fs::create_dir_all(&state_dir)?;
        let db = RuntimeDb::open_and_migrate(
            state_dir.join("runtime.sqlite"),
            state_dir.join("runtime.lock"),
        )?;
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            AGENT_ID,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        Ok((temp_dir, db))
    }

    fn assistant_entry(db: &RuntimeDb, id: &str, visibility: &str) -> anyhow::Result<()> {
        let mut entry = TranscriptEntry::new(
            AGENT_ID,
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            serde_json::json!({
                "turn_id": format!("turn-{id}"),
                "visibility": visibility,
                "blocks": [
                    {"type": "thinking", "text": "private reasoning"},
                    {"type": "text", "text": "user-visible answer"},
                    {"type": "tool_use", "input": {"secret": "private input"}}
                ],
                "origin": MessageOrigin::System { subsystem: "model".into() },
                "authority_class": "external_evidence",
            }),
        );
        entry.id = id.to_string();
        db.transcript_entries().upsert(&entry)?;
        Ok(())
    }

    fn report(
        message_id: &str,
        turn_id: &str,
        client_request_id: Option<&str>,
    ) -> NewContentReport {
        NewContentReport {
            reporter_principal: "user-1".into(),
            agent_id: AGENT_ID.into(),
            turn_id: turn_id.into(),
            message_id: message_id.into(),
            category: "harmful_or_abusive".into(),
            description: Some("reported by test".into()),
            client_request_id: client_request_id.map(str::to_owned),
        }
    }

    #[test]
    fn snapshot_only_accepts_user_visible_text_bodies() {
        let text = serde_json::json!({"body": {"type": "text", "text": "hello"}});
        let brief = serde_json::json!({"body": {"type": "brief", "text": "hello"}});
        let blocks = serde_json::json!({
            "visibility": "operator_visible",
            "blocks": [
                {"type": "thinking", "text": "private"},
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "input": {"secret": "private"}}
            ]
        });
        let private = serde_json::json!({
            "visibility": "runtime_private",
            "blocks": [{"type": "text", "text": "checkpoint"}]
        });
        let json = serde_json::json!({"body": {"type": "json", "value": "secret"}});
        assert_eq!(reportable_snapshot(&text).as_deref(), Some("hello"));
        assert_eq!(reportable_snapshot(&brief).as_deref(), Some("hello"));
        assert_eq!(reportable_snapshot(&blocks).as_deref(), Some("hello"));
        assert_eq!(reportable_snapshot(&private), None);
        assert_eq!(reportable_snapshot(&json), None);
    }

    #[test]
    fn snapshot_is_bounded_by_unicode_scalars() {
        let input = "🙂".repeat(MAX_SNAPSHOT_CHARS + 1);
        let (snapshot, truncated) = bounded_snapshot(&input);
        assert_eq!(snapshot.chars().count(), MAX_SNAPSHOT_CHARS);
        assert!(truncated);
    }

    #[test]
    fn create_persists_controlled_snapshot_and_is_idempotent() -> anyhow::Result<()> {
        let (_temp_dir, db) = test_db()?;
        assistant_entry(&db, "transcript-report", "operator_visible")?;
        let first = db.content_reports().create(report(
            "transcript-report",
            "turn-transcript-report",
            Some("retry-1"),
        ))?;
        let second = db.content_reports().create(NewContentReport {
            description: Some("different retry description".into()),
            ..report(
                "transcript-report",
                "turn-transcript-report",
                Some("retry-1"),
            )
        })?;

        assert_eq!(first, second);
        let connection = db.connection()?;
        let (snapshot, hash, count): (String, String, i64) = connection.query_row(
            "SELECT content_snapshot, content_snapshot_hash, COUNT(*)
             FROM content_reports
             WHERE report_id = ?1",
            [&first.report_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(snapshot, "user-visible answer");
        assert_eq!(hash, content_hash(&snapshot));
        assert_eq!(count, 1);
        assert!(!snapshot.contains("private"));
        Ok(())
    }

    #[test]
    fn private_rounds_are_not_reportable_and_rate_limit_is_enforced() -> anyhow::Result<()> {
        let (_temp_dir, db) = test_db()?;
        assistant_entry(&db, "transcript-private", "runtime_private")?;
        let error = db
            .content_reports()
            .create(report(
                "transcript-private",
                "turn-transcript-private",
                Some("private"),
            ))
            .expect_err("runtime-private rounds must not be reportable");
        assert!(error.downcast_ref::<ContentReportError>().is_some());

        for index in 0..REPORT_RATE_LIMIT {
            let message_id = format!("transcript-rate-{index}");
            let turn_id = format!("turn-{message_id}");
            assistant_entry(&db, &message_id, "operator_visible")?;
            db.content_reports().create(report(
                &message_id,
                &turn_id,
                Some(&format!("retry-{index}")),
            ))?;
        }
        assistant_entry(&db, "transcript-rate-over", "operator_visible")?;
        let error = db
            .content_reports()
            .create(report(
                "transcript-rate-over",
                "turn-transcript-rate-over",
                Some("retry-over"),
            ))
            .expect_err("report rate limit must be enforced");
        assert!(matches!(
            error.downcast_ref::<ContentReportError>(),
            Some(ContentReportError::RateLimited)
        ));
        Ok(())
    }
}
