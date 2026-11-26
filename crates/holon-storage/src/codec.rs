use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

pub fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

pub fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("parsing timestamp: {value}"))
}

pub fn parse_optional_timestamp(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    value.map(parse_timestamp).transpose()
}

/// Borrow SQLite text until admission completes, preserving Row::get<String>
/// conversion errors without allocating the successful column first.
pub fn borrowed_sql_text<'a>(
    row: &'a rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<&'a str> {
    let value = row.get_ref(index)?;
    value.as_str().map_err(|error| match error {
        rusqlite::types::FromSqlError::InvalidType => rusqlite::Error::InvalidColumnType(
            index,
            row.as_ref().column_name(index).unwrap_or("").to_owned(),
            value.data_type(),
        ),
        rusqlite::types::FromSqlError::Other(error) => {
            rusqlite::Error::FromSqlConversionFailure(index, value.data_type(), error)
        }
        error => {
            rusqlite::Error::FromSqlConversionFailure(index, value.data_type(), Box::new(error))
        }
    })
}
