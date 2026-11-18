//! Runtime-owned clock sample, independent of message and report timestamps.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;

/// Fixed initial-context reservation; time values never affect stable layout.
pub const RUNTIME_TIME_RESERVED_TOKENS: usize = 128;

pub use holon_domain::timezone::{deserialize_optional_timezone, parse_timezone};

pub fn resolve_timezone(agent: Option<&str>, runtime: Option<&str>) -> Result<Tz> {
    // Validate every explicit setting, even when another setting takes precedence.
    let agent = agent.map(parse_timezone).transpose()?;
    let runtime = runtime.map(parse_timezone).transpose()?;
    Ok(agent.or(runtime).unwrap_or(chrono_tz::UTC))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTime {
    instant: DateTime<Utc>,
    timezone: Tz,
}

impl RuntimeTime {
    pub fn new(instant: DateTime<Utc>, timezone: Tz) -> Self {
        Self { instant, timezone }
    }

    pub fn from_config(
        instant: DateTime<Utc>,
        agent_timezone: Option<&str>,
        runtime_timezone: Option<&str>,
    ) -> Result<Self> {
        Ok(Self::new(
            instant,
            resolve_timezone(agent_timezone, runtime_timezone)?,
        ))
    }

    pub fn instant(&self) -> DateTime<Utc> {
        self.instant
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    pub fn current_time(&self) -> String {
        self.instant
            .with_timezone(&self.timezone)
            .to_rfc3339_opts(SecondsFormat::Secs, false)
    }

    pub fn render(&self) -> String {
        format!(
            "current_time: {}\ntimezone: {}\nRuntime clock sample, not a message or report date. The latest runtime time block takes precedence.",
            self.current_time(),
            self.timezone
        )
    }
}

#[cfg(test)]
mod tests;
