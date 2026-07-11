use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::{helpers::parse_tool_args, spec::typed_spec},
    types::{AuthorityClass, ToolCapabilityFamily},
    x_search::{search, XSearchRequest},
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const NAME: &str = crate::tool::names::X_SEARCH;
const MAX_HANDLES: usize = 10;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct XSearchArgs {
    query: String,
    #[serde(default)]
    allowed_x_handles: Vec<String>,
    #[serde(default)]
    excluded_x_handles: Vec<String>,
    from_date: Option<String>,
    to_date: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<XSearchArgs>(NAME, include_str!("../tool_descriptions/x_search.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: XSearchArgs = parse_tool_args(NAME, input)?;
    let query = required_string(args.query, "query")?;
    let allowed_x_handles = normalize_handles(args.allowed_x_handles, "allowed_x_handles")?;
    let excluded_x_handles = normalize_handles(args.excluded_x_handles, "excluded_x_handles")?;
    let from_date = normalize_date(args.from_date, "from_date")?;
    let to_date = normalize_date(args.to_date, "to_date")?;
    if from_date
        .as_deref()
        .zip(to_date.as_deref())
        .is_some_and(|(from_date, to_date)| from_date > to_date)
    {
        return Err(anyhow!("from_date must not be later than to_date"));
    }
    let config = runtime.x_search_config().ok_or_else(|| {
        anyhow!("x_search_unavailable: xAI is not configured or XSearch is disabled")
    })?;
    serialize_success(
        NAME,
        &search(
            XSearchRequest {
                query,
                allowed_x_handles,
                excluded_x_handles,
                from_date,
                to_date,
            },
            &config,
        )
        .await?,
    )
}

fn required_string(value: String, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        Err(anyhow!("{field} must not be empty"))
    } else {
        Ok(value.to_string())
    }
}

fn normalize_handles(values: Vec<String>, field: &str) -> Result<Vec<String>> {
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

fn normalize_date(value: Option<String>, field: &str) -> Result<Option<String>> {
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
