//! Attempt-level usage ledger facade over `usage_attempts`.

use anyhow::Result;
use chrono::NaiveDate;

pub use holon_storage::usage::{
    UsageAttemptOutcome, UsageAttemptRecord, UsageDayTotal, UsageTokenCounts,
};

use super::RuntimeDb;

impl RuntimeDb {
    /// Append attempt rows to the usage ledger.
    ///
    /// The ledger is runtime state shared with the rest of `state/runtime.sqlite`,
    /// so it reuses the same writer, WAL, backup, and retention paths instead of
    /// the diagnostics store.
    pub fn append_usage_attempts(&self, records: &[UsageAttemptRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let connection = self.connection()?;
        holon_storage::usage::append_usage_attempts(&connection, records)
    }

    pub fn usage_daily_totals(
        &self,
        agent_id: Option<&str>,
        from_day: NaiveDate,
        to_day: NaiveDate,
    ) -> Result<Vec<UsageDayTotal>> {
        let connection = self.connection()?;
        holon_storage::usage::usage_daily_totals(&connection, agent_id, from_day, to_day)
    }

    pub fn usage_attempt_count(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.connection()?;
        holon_storage::usage::usage_attempt_count(&connection, agent_id)
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use holon_storage::usage::UsageTokenCounts;

    fn runtime_db() -> anyhow::Result<(tempfile::TempDir, RuntimeDb)> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("state/runtime.sqlite"),
            directory.path().join("state/runtime.lock"),
        )?;
        Ok((directory, db))
    }

    fn attempt(
        area: &mut Vec<UsageAttemptRecord>,
        id: &str,
        model_ref: &str,
        endpoint: &str,
        outcome: UsageAttemptOutcome,
        token_usage: Option<u64>,
        started_at: chrono::DateTime<Utc>,
    ) {
        area.push(UsageAttemptRecord {
            attempt_id: id.into(),
            agent_id: "agent-a".into(),
            turn_id: Some("turn-a".into()),
            work_item_id: Some("work-a".into()),
            task_id: None,
            round_index: Some(0),
            attempt_index: 1,
            started_at,
            provider: "opencode-go".into(),
            endpoint: endpoint.into(),
            model_id: "kimi-k2".into(),
            model_ref: model_ref.into(),
            transport: (outcome == UsageAttemptOutcome::Success).then(|| "openai_chat".to_string()),
            service_tier: None,
            outcome,
            failure_category: (outcome != UsageAttemptOutcome::Success)
                .then(|| "server_error".to_string()),
            token_usage: token_usage.map(|tokens| UsageTokenCounts {
                uncached_input_tokens: tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
            }),
            trace_id: None,
            span_id: None,
        });
    }

    #[test]
    fn ledger_records_each_attempt_once_and_keeps_missing_usage_visible() -> anyhow::Result<()> {
        let (directory, db) = runtime_db()?;
        let now = Utc::now();
        let mut records = Vec::new();
        attempt(
            &mut records,
            "attempt-success",
            "opencode-go@default/kimi-k2",
            "default",
            UsageAttemptOutcome::Success,
            Some(100),
            now,
        );
        attempt(
            &mut records,
            "attempt-fallback-missing",
            "opencode-go@default/kimi-k2",
            "default",
            UsageAttemptOutcome::FailedRetryable,
            None,
            now,
        );
        attempt(
            &mut records,
            "attempt-transport-failure",
            "opencode-go@messages/kimi-k2",
            "messages",
            UsageAttemptOutcome::FailedTerminal,
            None,
            now,
        );
        db.append_usage_attempts(&records)?;

        // One row per attempt, never a second row per round or turn.
        assert_eq!(db.usage_attempt_count(None)?, 3);
        assert_eq!(db.usage_attempt_count(Some("agent-a"))?, 3);
        assert_eq!(db.usage_attempt_count(Some("agent-b"))?, 0);

        let day = now.date_naive();
        let totals = db.usage_daily_totals(None, day, day)?;
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].attempts, 3);
        assert_eq!(totals[0].reported_attempts, 1);
        assert_eq!(totals[0].missing_attempts, 2);
        assert_eq!(totals[0].uncached_input_tokens, 100);

        // Billing identity keeps folded provider ids apart.
        let connection = db.connection()?;
        let endpoints: Vec<String> = connection
            .prepare("SELECT endpoint FROM usage_attempts ORDER BY attempt_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            endpoints,
            vec![
                "default".to_string(),
                "default".to_string(),
                "messages".to_string()
            ]
        );

        // The ledger lives in `state/runtime.sqlite`, so the supported backup
        // path (copying the runtime database) already carries it.
        db.run_wal_checkpoint_pass()?;
        let copied = directory.path().join("state/runtime.sqlite");
        let restored_dir = tempfile::tempdir()?;
        std::fs::create_dir_all(restored_dir.path().join("state"))?;
        std::fs::copy(copied, restored_dir.path().join("state/runtime.sqlite"))?;
        drop(db);
        let restored = RuntimeDb::open_and_migrate(
            restored_dir.path().join("state/runtime.sqlite"),
            restored_dir.path().join("state/runtime.lock"),
        )?;
        assert_eq!(restored.usage_attempt_count(None)?, 3);
        Ok(())
    }
}
