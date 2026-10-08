//! Runtime-owned clock sample, independent of message and report timestamps.

use anyhow::{anyhow, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer};

/// Fixed initial-context reservation; time values never affect stable layout.
pub const RUNTIME_TIME_RESERVED_TOKENS: usize = 128;

pub fn parse_timezone(value: &str) -> Result<Tz> {
    value
        .parse()
        .map_err(|_| anyhow!("invalid IANA timezone: {value:?}"))
}

pub fn resolve_timezone(agent: Option<&str>, runtime: Option<&str>) -> Result<Tz> {
    // Validate every explicit setting, even when another setting takes precedence.
    let agent = agent.map(parse_timezone).transpose()?;
    let runtime = runtime.map(parse_timezone).transpose()?;
    Ok(agent.or(runtime).unwrap_or(chrono_tz::UTC))
}

pub fn deserialize_optional_timezone<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = &value {
        parse_timezone(value).map_err(serde::de::Error::custom)?;
    }
    Ok(value)
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
mod tests {
    use super::*;

    fn sample(instant: &str, timezone: &str) -> RuntimeTime {
        RuntimeTime::from_config(instant.parse().unwrap(), Some(timezone), None).unwrap()
    }

    #[test]
    fn all_supported_timezones_fit_initial_time_reservation() {
        for timezone in chrono_tz::TZ_VARIANTS {
            for instant in ["1900-01-01T00:00:00Z", "2026-10-08T15:59:59Z"] {
                let rendered = RuntimeTime::new(instant.parse().unwrap(), timezone).render();
                // Runtime's text estimate uses four Unicode scalars per token.
                assert!(rendered.chars().count() < RUNTIME_TIME_RESERVED_TOKENS * 4);
            }
        }
    }

    #[test]
    fn local_midnight_and_year_boundaries() {
        assert_eq!(
            sample("2025-12-31T16:00:00.999Z", "Asia/Shanghai").current_time(),
            "2026-01-01T00:00:00+08:00"
        );
        assert_eq!(
            sample("2026-01-01T00:00:00Z", "America/Los_Angeles").current_time(),
            "2025-12-31T16:00:00-08:00"
        );
    }

    #[test]
    fn los_angeles_dst_transitions() {
        for (instant, expected) in [
            ("2026-03-08T09:59:59Z", "2026-03-08T01:59:59-08:00"),
            ("2026-03-08T10:00:00Z", "2026-03-08T03:00:00-07:00"),
            ("2026-11-01T08:59:59Z", "2026-11-01T01:59:59-07:00"),
            ("2026-11-01T09:00:00Z", "2026-11-01T01:00:00-08:00"),
        ] {
            assert_eq!(
                sample(instant, "America/Los_Angeles").current_time(),
                expected
            );
        }
    }

    #[test]
    fn precedence_and_invalid_explicit_settings() {
        assert_eq!(resolve_timezone(None, None).unwrap(), chrono_tz::UTC);
        assert_eq!(
            resolve_timezone(None, Some("Asia/Shanghai")).unwrap(),
            chrono_tz::Asia::Shanghai
        );
        assert_eq!(
            resolve_timezone(Some("America/Los_Angeles"), Some("Asia/Shanghai")).unwrap(),
            chrono_tz::America::Los_Angeles
        );
        for invalid in ["", "local", "+08:00", "Mars/Olympus", " UTC "] {
            assert!(parse_timezone(invalid).is_err());
            assert!(resolve_timezone(Some("UTC"), Some(invalid)).is_err());
        }
    }

    #[test]
    fn render_only_exposes_local_clock_and_timezone() {
        let time = sample("2026-01-01T00:00:00Z", "Asia/Shanghai");
        let rendered = time.render();
        assert!(rendered
            .starts_with("current_time: 2026-01-01T08:00:00+08:00\ntimezone: Asia/Shanghai\n"));
        assert!(!rendered.contains("2026-01-01T00:00:00"));
        assert!(rendered.contains("not a message or report date"));
        assert!(rendered.contains("latest runtime time block takes precedence"));
    }
}
