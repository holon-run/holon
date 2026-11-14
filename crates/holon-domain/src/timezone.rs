use anyhow::{anyhow, Result};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer};

pub fn parse_timezone(value: &str) -> Result<Tz> {
    value
        .parse()
        .map_err(|_| anyhow!("invalid IANA timezone: {value:?}"))
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
