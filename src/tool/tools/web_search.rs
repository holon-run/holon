use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
    web::search::{search, WebSearchRequest},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "WebSearch";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebSearchArgs {
    pub(crate) query: String,
    pub(crate) max_results: Option<usize>,
    pub(crate) provider: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<WebSearchArgs>(
            NAME,
            include_str!("../tool_descriptions/web_search.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: WebSearchArgs = parse_tool_args(NAME, input)?;
    let query = validate_non_empty(args.query, NAME, "query")?;
    let web_config = runtime.web_config();
    let result = search(
        WebSearchRequest {
            query,
            max_results: args.max_results,
            provider: args.provider,
        },
        &web_config,
    )
    .await?;
    serialize_success(NAME, &result)
}
