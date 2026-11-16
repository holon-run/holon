use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use crate::x_search::search;
use crate::x_search::XSearchRequest;
use anyhow::anyhow;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::x_search::*;

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
