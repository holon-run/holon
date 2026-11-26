//! Attempt-level usage ledger persistence (`usage_attempts`).
//!
//! Contract (see `docs/rfcs/runtime-usage-ledger.md`):
//! - one row per provider attempt that was actually dispatched to a transport;
//!   rounds and turns never add a second row, so the attempt is the only
//!   counting unit and `SUM(...)` over this table is not double counted;
//! - missing wire usage is stored as `NULL` token columns with
//!   `token_usage_source = 'missing'`, never as `0`, so an underestimated
//!   total stays detectable;
//! - dimensions come from the resolved `ModelRouteRef`, so the endpoint
//!   segment keeps routes such as `opencode-go@default` and
//!   `opencode-go@messages` distinct;
//! - `transport` stays `NULL` when the attempt failed before a transport
//!   reported one; it is never folded into an `unknown` string;
//! - `day` is derived from `started_at` in UTC at insert time.
//!
//! The ledger is runtime state: it lives in `state/runtime.sqlite` and is
//! retained through the shared runtime retention pass, not through trace
//! sampling or the trace retention policy.

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub const TOKEN_USAGE_SOURCE_REPORTED: &str = "reported";
pub const TOKEN_USAGE_SOURCE_MISSING: &str = "missing";

/// Terminal outcome of one provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAttemptOutcome {
    Success,
    FailedRetryable,
    FailedTerminal,
    Cancelled,
}

impl UsageAttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::FailedRetryable => "failed_retryable",
            Self::FailedTerminal => "failed_terminal",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "failed_retryable" => Some(Self::FailedRetryable),
            "failed_terminal" => Some(Self::FailedTerminal),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Normalized token counts for one attempt (`attempt_token_usage.v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTokenCounts {
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: Option<u64>,
}

/// One provider attempt row.
///
/// `token_usage: None` is the only way to express "the attempt reported no
/// usage"; callers must not substitute zeroed counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageAttemptRecord {
    pub attempt_id: String,
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub work_item_id: Option<String>,
    pub task_id: Option<String>,
    pub round_index: Option<u64>,
    pub attempt_index: u64,
    pub started_at: DateTime<Utc>,
    pub provider: String,
    pub endpoint: String,
    pub model_id: String,
    pub model_ref: String,
    pub transport: Option<String>,
    pub service_tier: Option<String>,
    pub outcome: UsageAttemptOutcome,
    pub failure_category: Option<String>,
    pub token_usage: Option<UsageTokenCounts>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl UsageAttemptRecord {
    pub fn day(&self) -> NaiveDate {
        self.started_at.date_naive()
    }

    pub fn token_usage_source(&self) -> &'static str {
        match self.token_usage {
            Some(_) => TOKEN_USAGE_SOURCE_REPORTED,
            None => TOKEN_USAGE_SOURCE_MISSING,
        }
    }
}

/// Append attempt rows. `attempt_id` is the primary key, so a repeated write of
/// the same attempt is ignored instead of duplicating the ledger entry.
pub fn append_usage_attempts(
    connection: &Connection,
    records: &[UsageAttemptRecord],
) -> Result<()> {
    for record in records {
        insert_usage_attempt(connection, record)?;
    }
    Ok(())
}

pub fn insert_usage_attempt(connection: &Connection, record: &UsageAttemptRecord) -> Result<()> {
    let usage = record.token_usage;
    connection.execute(
        "INSERT OR IGNORE INTO usage_attempts (
           attempt_id, agent_id, turn_id, work_item_id, task_id, round_index,
           attempt_index, started_at, day, provider, endpoint, model_id,
           model_ref, transport, service_tier, outcome, failure_category,
           uncached_input_tokens, cache_read_tokens, cache_write_tokens,
           output_tokens, reasoning_tokens, token_usage_source, trace_id, span_id
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
           ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
         )",
        params![
            record.attempt_id,
            record.agent_id,
            record.turn_id,
            record.work_item_id,
            record.task_id,
            record.round_index.map(|value| value as i64),
            record.attempt_index as i64,
            crate::migrations::timestamp(record.started_at),
            record.day().format("%Y-%m-%d").to_string(),
            record.provider,
            record.endpoint,
            record.model_id,
            record.model_ref,
            record.transport,
            record.service_tier,
            record.outcome.as_str(),
            record.failure_category,
            usage.map(|usage| usage.uncached_input_tokens as i64),
            usage.map(|usage| usage.cache_read_tokens as i64),
            usage.map(|usage| usage.cache_write_tokens as i64),
            usage.map(|usage| usage.output_tokens as i64),
            usage.and_then(|usage| usage.reasoning_tokens.map(|value| value as i64)),
            record.token_usage_source(),
            record.trace_id,
            record.span_id,
        ],
    )?;
    Ok(())
}

/// Daily usage totals.
///
/// Token sums only cover `token_usage_source = 'reported'` rows;
/// `missing_attempts` counts the rows whose usage is unknown, so a query can
/// never quietly present an underestimated total as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDayTotal {
    pub day: NaiveDate,
    pub attempts: u64,
    pub reported_attempts: u64,
    pub missing_attempts: u64,
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

pub fn usage_daily_totals(
    connection: &Connection,
    agent_id: Option<&str>,
    from_day: NaiveDate,
    to_day: NaiveDate,
) -> Result<Vec<UsageDayTotal>> {
    let mut statement = connection.prepare(
        "SELECT day,
                COUNT(*),
                SUM(CASE WHEN token_usage_source = 'reported' THEN 1 ELSE 0 END),
                SUM(CASE WHEN token_usage_source = 'missing' THEN 1 ELSE 0 END),
                COALESCE(SUM(CASE WHEN token_usage_source = 'reported'
                                  THEN uncached_input_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN token_usage_source = 'reported'
                                  THEN cache_read_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN token_usage_source = 'reported'
                                  THEN cache_write_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN token_usage_source = 'reported'
                                  THEN output_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN token_usage_source = 'reported'
                                  THEN reasoning_tokens ELSE 0 END), 0)
           FROM usage_attempts
          WHERE day >= ?1 AND day <= ?2 AND (?3 IS NULL OR agent_id = ?3)
          GROUP BY day
          ORDER BY day ASC",
    )?;
    let rows = statement.query_map(
        params![
            from_day.format("%Y-%m-%d").to_string(),
            to_day.format("%Y-%m-%d").to_string(),
            agent_id,
        ],
        |row| {
            let day: String = row.get(0)?;
            Ok(UsageDayTotal {
                day: NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap_or(NaiveDate::MIN),
                attempts: row.get::<_, i64>(1)? as u64,
                reported_attempts: row.get::<_, i64>(2)? as u64,
                missing_attempts: row.get::<_, i64>(3)? as u64,
                uncached_input_tokens: row.get::<_, i64>(4)? as u64,
                cache_read_tokens: row.get::<_, i64>(5)? as u64,
                cache_write_tokens: row.get::<_, i64>(6)? as u64,
                output_tokens: row.get::<_, i64>(7)? as u64,
                reasoning_tokens: row.get::<_, i64>(8)? as u64,
            })
        },
    )?;
    let mut totals = Vec::new();
    for row in rows {
        totals.push(row?);
    }
    Ok(totals)
}

pub fn usage_attempt_count(connection: &Connection, agent_id: Option<&str>) -> Result<u64> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM usage_attempts WHERE (?1 IS NULL OR agent_id = ?1)",
        params![agent_id],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usage_attempts (
                   attempt_id TEXT PRIMARY KEY,
                   agent_id TEXT NOT NULL,
                   turn_id TEXT,
                   work_item_id TEXT,
                   task_id TEXT,
                   round_index INTEGER,
                   attempt_index INTEGER NOT NULL,
                   started_at TEXT NOT NULL,
                   day TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   endpoint TEXT NOT NULL,
                   model_id TEXT NOT NULL,
                   model_ref TEXT NOT NULL,
                   transport TEXT,
                   service_tier TEXT,
                   outcome TEXT NOT NULL,
                   failure_category TEXT,
                   uncached_input_tokens INTEGER,
                   cache_read_tokens INTEGER,
                   cache_write_tokens INTEGER,
                   output_tokens INTEGER,
                   reasoning_tokens INTEGER,
                   token_usage_source TEXT NOT NULL,
                   trace_id TEXT,
                   span_id TEXT
                 );",
            )
            .unwrap();
        connection
    }

    fn record(
        attempt_id: &str,
        endpoint: &str,
        usage: Option<UsageTokenCounts>,
    ) -> UsageAttemptRecord {
        UsageAttemptRecord {
            attempt_id: attempt_id.into(),
            agent_id: "agent-1".into(),
            turn_id: Some("turn-1".into()),
            work_item_id: None,
            task_id: None,
            round_index: Some(1),
            attempt_index: 1,
            started_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
            provider: "opencode-go".into(),
            endpoint: endpoint.into(),
            model_id: "kimi-k2".into(),
            model_ref: format!("opencode-go@{endpoint}/kimi-k2"),
            transport: Some("openai_chat".into()),
            service_tier: None,
            outcome: UsageAttemptOutcome::Success,
            failure_category: None,
            token_usage: usage,
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn missing_usage_rows_stay_distinct_from_zero_usage() {
        let connection = connection();
        let reported = UsageTokenCounts {
            uncached_input_tokens: 100,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
            output_tokens: 20,
            reasoning_tokens: None,
        };
        append_usage_attempts(
            &connection,
            &[
                record("a", "default", Some(reported)),
                record("b", "default", None),
            ],
        )
        .unwrap();

        let day = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        let totals = usage_daily_totals(&connection, None, day, day).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].attempts, 2);
        assert_eq!(totals[0].reported_attempts, 1);
        assert_eq!(totals[0].missing_attempts, 1);
        assert_eq!(totals[0].uncached_input_tokens, 100);
        assert_eq!(totals[0].output_tokens, 20);
    }

    #[test]
    fn repeated_attempt_id_does_not_duplicate() {
        let connection = connection();
        let usage = UsageTokenCounts {
            uncached_input_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
        };
        let record = record("a", "default", Some(usage));
        append_usage_attempts(&connection, std::slice::from_ref(&record)).unwrap();
        append_usage_attempts(&connection, &[record]).unwrap();
        assert_eq!(usage_attempt_count(&connection, None).unwrap(), 1);
    }

    #[test]
    fn endpoints_stay_separate() {
        let connection = connection();
        append_usage_attempts(
            &connection,
            &[record("a", "default", None), record("b", "messages", None)],
        )
        .unwrap();
        let endpoints: Vec<String> = connection
            .prepare("SELECT endpoint FROM usage_attempts ORDER BY attempt_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            endpoints,
            vec!["default".to_string(), "messages".to_string()]
        );
    }
}
