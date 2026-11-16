use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::web::fetch::fetch_with_provider;
use crate::web::fetch::WebFetchRequest;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::web_fetch::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: WebFetchArgs = parse_tool_args(NAME, input)?;
    let url = validate_non_empty(args.url, NAME, "url")?;
    let web_config = runtime.web_config();
    let result = fetch_with_provider(
        WebFetchRequest {
            url,
            max_chars: args.max_chars,
            extract_mode: args.extract_mode,
        },
        args.provider.as_deref(),
        &web_config,
    )
    .await?;
    serialize_success(NAME, &result)
}
