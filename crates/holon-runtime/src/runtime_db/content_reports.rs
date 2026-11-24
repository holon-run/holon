//! Runtime entry point for content reports.

use anyhow::Result;

pub use holon_storage::content_reports::{
    ContentReportError, ContentReportRecord, NewContentReport, MAX_CLIENT_REQUEST_ID_CHARS,
    MAX_DESCRIPTION_CHARS, MAX_IDENTIFIER_CHARS, REPORT_CATEGORIES,
};

use super::types::ContentReportRepository;

impl<'a> ContentReportRepository<'a> {
    pub fn create(&self, report: NewContentReport) -> Result<ContentReportRecord> {
        self.db
            .transaction(|tx| holon_storage::content_reports::create_tx(tx, &report))
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::RuntimeDb;
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        MessageOrigin, TranscriptEntry, TranscriptEntryKind,
    };
    use holon_storage::{content_reports::REPORT_RATE_LIMIT, evidence::content_hash};
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
