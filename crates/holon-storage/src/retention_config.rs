//! Configuration shared by the runtime retention policy and schema.
use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DEFAULT_AUDIT_EVENTS_DAYS: u64 = 30;
pub const DEFAULT_TRANSCRIPT_ENTRIES_DAYS: u64 = 90;
pub const DEFAULT_TOOL_EXECUTIONS_DAYS: u64 = 90;
pub const DEFAULT_AUDIT_EVENTS_MIN_ROWS_PER_SCOPE: usize = 4096;
pub const DEFAULT_TRANSCRIPT_ENTRIES_MIN_ROWS: usize = 20_000;
pub const DEFAULT_TOOL_EXECUTIONS_MIN_ROWS: usize = 15_000;
pub const DEFAULT_RETENTION_INTERVAL_HOURS: u64 = 6;
pub const DEFAULT_INCREMENTAL_VACUUM_PAGES: u32 = 256;

pub const SCHEDULER_EVENT_WINDOW: usize = 64;

const MAX_RETENTION_DAYS: u64 = 3650;
const MAX_RETENTION_ROWS: usize = 10_000_000;
const MAX_RETENTION_INTERVAL_HOURS: u64 = 24 * 30;
const MAX_INCREMENTAL_VACUUM_PAGES: u32 = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeDbRetentionPolicy {
    pub enabled: bool,
    pub audit_events_days: u64,
    pub transcript_entries_days: u64,
    pub tool_executions_days: u64,
    pub audit_events_min_rows_per_scope: usize,
    pub transcript_entries_min_rows: usize,
    pub tool_executions_min_rows: usize,
    pub interval_hours: u64,
    pub incremental_vacuum_pages: u32,
}

impl Default for RuntimeDbRetentionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            audit_events_days: DEFAULT_AUDIT_EVENTS_DAYS,
            transcript_entries_days: DEFAULT_TRANSCRIPT_ENTRIES_DAYS,
            tool_executions_days: DEFAULT_TOOL_EXECUTIONS_DAYS,
            audit_events_min_rows_per_scope: DEFAULT_AUDIT_EVENTS_MIN_ROWS_PER_SCOPE,
            transcript_entries_min_rows: DEFAULT_TRANSCRIPT_ENTRIES_MIN_ROWS,
            tool_executions_min_rows: DEFAULT_TOOL_EXECUTIONS_MIN_ROWS,
            interval_hours: DEFAULT_RETENTION_INTERVAL_HOURS,
            incremental_vacuum_pages: DEFAULT_INCREMENTAL_VACUUM_PAGES,
        }
    }
}

impl RuntimeDbRetentionPolicy {
    pub fn validate(self) -> Result<Self> {
        validate_positive_bounded(
            "runtime.retention.audit_events_days",
            self.audit_events_days,
            MAX_RETENTION_DAYS,
        )?;
        validate_positive_bounded(
            "runtime.retention.transcript_entries_days",
            self.transcript_entries_days,
            MAX_RETENTION_DAYS,
        )?;
        validate_positive_bounded(
            "runtime.retention.tool_executions_days",
            self.tool_executions_days,
            MAX_RETENTION_DAYS,
        )?;
        let event_window = holon_domain::MAX_EVENT_STREAM_WINDOW.max(SCHEDULER_EVENT_WINDOW);
        if self.audit_events_min_rows_per_scope < event_window {
            bail!(
                "runtime.retention.audit_events_min_rows_per_scope must be at least {event_window}"
            );
        }
        validate_positive_bounded(
            "runtime.retention.audit_events_min_rows_per_scope",
            self.audit_events_min_rows_per_scope,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.transcript_entries_min_rows",
            self.transcript_entries_min_rows,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.tool_executions_min_rows",
            self.tool_executions_min_rows,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.interval_hours",
            self.interval_hours,
            MAX_RETENTION_INTERVAL_HOURS,
        )?;
        validate_positive_bounded(
            "runtime.retention.incremental_vacuum_pages",
            self.incremental_vacuum_pages,
            MAX_INCREMENTAL_VACUUM_PAGES,
        )?;
        Ok(self)
    }
}

fn validate_positive_bounded<T>(key: &str, value: T, max: T) -> Result<()>
where
    T: Copy + Ord + Default + std::fmt::Display,
{
    if value == T::default() || value > max {
        bail!("{key} must be between 1 and {max}");
    }
    Ok(())
}
