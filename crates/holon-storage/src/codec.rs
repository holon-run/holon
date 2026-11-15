use anyhow::{anyhow, Result};
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
