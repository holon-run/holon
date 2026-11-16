use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::anyhow;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::X_SEARCH;

const MAX_HANDLES: usize = 10;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct XSearchArgs {
    pub query: String,
    #[serde(default)]
    pub allowed_x_handles: Vec<String>,
    #[serde(default)]
    pub excluded_x_handles: Vec<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<XSearchArgs>(NAME, include_str!("../tool_descriptions/x_search.md"))?,
    })
}

pub fn required_string(value: String, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        Err(anyhow!("{field} must not be empty"))
    } else {
        Ok(value.to_string())
    }
}

pub fn normalize_handles(values: Vec<String>, field: &str) -> Result<Vec<String>> {
    if values.len() > MAX_HANDLES {
        return Err(anyhow!("{field} supports at most {MAX_HANDLES} handles"));
    }
    values
        .into_iter()
        .map(|value| {
            let value = value.trim().trim_start_matches('@');
            if value.is_empty()
                || !value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                Err(anyhow!("{field} contains an invalid X handle"))
            } else {
                Ok(value.to_string())
            }
        })
        .collect()
}

pub fn normalize_date(value: Option<String>, field: &str) -> Result<Option<String>> {
    value
        .map(|value| {
            let value = required_string(value, field)?;
            let valid = value.len() == 10
                && value.as_bytes()[4] == b'-'
                && value.as_bytes()[7] == b'-'
                && chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d").is_ok();
            if valid {
                Ok(value)
            } else {
                Err(anyhow!("{field} must use YYYY-MM-DD format"))
            }
        })
        .transpose()
}
