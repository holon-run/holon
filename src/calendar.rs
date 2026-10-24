//! Calendar-based timer scheduling.
//!
//! The runtime uses a deliberately small cron contract: five standard fields
//! (minute, hour, day of month, month, day of week) and an explicit IANA
//! timezone.  The parser crate uses a seconds-first expression, so the
//! normalized representation always prefixes the user expression with `0`.

use std::{fmt, str::FromStr};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_EXPRESSION_LEN: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CalendarSchedule {
    /// A standard five-field cron expression.
    pub expression: String,
    /// An IANA timezone name, for example `America/Los_Angeles`.
    pub timezone: String,
}

impl CalendarSchedule {
    pub fn new(expression: impl Into<String>, timezone: impl Into<String>) -> Result<Self> {
        let expression = normalize_expression(&expression.into())?;
        let timezone = timezone.into();
        parse_timezone(&timezone)?;
        Ok(Self {
            expression,
            timezone,
        })
    }

    pub fn timezone(&self) -> Result<Tz> {
        parse_timezone(&self.timezone)
    }

    pub fn schedule(&self) -> Result<Schedule> {
        Schedule::from_str(&parser_expression(&self.expression)?)
            .context("invalid normalized calendar expression")
    }

    /// Return the next match strictly after `after`, expressed in UTC.
    ///
    /// Matching is performed in the configured local timezone, which keeps
    /// calendar rules independent from the daemon's host timezone.
    pub fn next_after(&self, after: DateTime<Utc>) -> Result<CalendarOccurrence> {
        let timezone = self.timezone()?;
        let schedule = self.schedule()?;
        let local_after = after.with_timezone(&timezone);
        let local = schedule
            .after(&local_after)
            .next()
            .context("calendar expression has no future occurrence")?;
        let utc = local.with_timezone(&Utc);
        Ok(CalendarOccurrence {
            local: local.to_rfc3339(),
            utc,
            offset_seconds: local.offset().fix().local_minus_utc(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CalendarOccurrence {
    pub local: String,
    pub utc: DateTime<Utc>,
    pub offset_seconds: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarExpressionError(String);

impl fmt::Display for CalendarExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CalendarExpressionError {}

fn normalize_expression(expression: &str) -> Result<String> {
    let trimmed = expression.split_whitespace().collect::<Vec<_>>();
    ensure!(!trimmed.is_empty(), "calendar expression must not be empty");
    ensure!(
        expression.len() <= MAX_EXPRESSION_LEN,
        "calendar expression exceeds {MAX_EXPRESSION_LEN} bytes"
    );
    ensure!(
        trimmed.len() == 5,
        "calendar expression must contain exactly five fields (minute hour day-of-month month day-of-week)"
    );
    let normalized = format!("0 {}", trimmed.join(" "));
    Schedule::from_str(&parser_expression(&normalized)?).map_err(|err| anyhow::anyhow!(err))?;
    Ok(normalized)
}

fn parser_expression(expression: &str) -> Result<String> {
    let mut fields = expression
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure!(
        fields.len() == 6,
        "normalized calendar expression must contain six fields"
    );
    fields[5] = translate_day_of_week(&fields[5])?;
    Ok(fields.join(" "))
}

fn translate_day_of_week(field: &str) -> Result<String> {
    field
        .split(',')
        .map(|part| {
            let (base, step) = part
                .split_once('/')
                .map_or((part, None), |(base, step)| (base, Some(step)));
            let translated = if base == "*" || base == "?" {
                base.to_owned()
            } else if let Some((start, end)) = base.split_once('-') {
                if start == "0" && end == "7" {
                    "*".to_owned()
                } else {
                    format!(
                        "{}-{}",
                        translate_day_of_week_value(start)?,
                        translate_day_of_week_value(end)?
                    )
                }
            } else {
                translate_day_of_week_value(base)?
            };
            Ok(match step {
                Some(step) => format!("{translated}/{step}"),
                None => translated,
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(","))
}

fn translate_day_of_week_value(value: &str) -> Result<String> {
    if let Ok(day) = value.parse::<u8>() {
        ensure!(day <= 7, "day of week must be between 0 and 7");
        return Ok(match day {
            0 | 7 => "SUN",
            1 => "MON",
            2 => "TUE",
            3 => "WED",
            4 => "THU",
            5 => "FRI",
            6 => "SAT",
            _ => unreachable!(),
        }
        .to_owned());
    }
    Ok(value.to_ascii_uppercase())
}

fn parse_timezone(timezone: &str) -> Result<Tz> {
    ensure!(
        !timezone.trim().is_empty(),
        "calendar timezone must not be empty"
    );
    timezone
        .parse::<Tz>()
        .map_err(|_| anyhow::anyhow!("unknown IANA calendar timezone: {timezone}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn normalizes_five_field_expression_and_uses_timezone() {
        let schedule = CalendarSchedule::new("30 9 * * 1-5", "America/Los_Angeles").unwrap();
        assert_eq!(schedule.expression, "0 30 9 * * 1-5");
        let after = Utc.with_ymd_and_hms(2026, 1, 2, 18, 0, 0).unwrap();
        let occurrence = schedule.next_after(after).unwrap();
        assert_eq!(occurrence.local, "2026-01-05T09:30:00-08:00");
        assert_eq!(
            occurrence.utc,
            Utc.with_ymd_and_hms(2026, 1, 5, 17, 30, 0).unwrap()
        );
    }

    #[test]
    fn rejects_non_five_field_cron_and_unknown_timezone() {
        assert!(CalendarSchedule::new("* * * *", "UTC").is_err());
        assert!(CalendarSchedule::new("0 0 1 * * *", "UTC").is_err());
        assert!(CalendarSchedule::new("0 0 * * *", "Mars/Olympus").is_err());
    }

    #[test]
    fn rejects_sub_minute_rules() {
        let schedule = CalendarSchedule::new("* * * * *", "UTC").unwrap();
        let occurrence = schedule
            .next_after(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .unwrap();
        assert_eq!(occurrence.utc.second(), 0);
    }
}
