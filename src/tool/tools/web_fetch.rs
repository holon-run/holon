use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
    web::fetch::{fetch, ExtractMode, WebFetchRequest},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "WebFetch";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebFetchArgs {
    pub(crate) url: String,
    pub(crate) max_chars: Option<usize>,
    #[serde(default)]
    pub(crate) extract_mode: ExtractMode,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<WebFetchArgs>(
            NAME,
            "Fetch a specific http or https URL through Holon's web policy, extract readable text, wrap it as untrusted external content, and return provenance including final URL, status, content type, truncation, and hash.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: WebFetchArgs = parse_tool_args(NAME, input)?;
    let url = validate_non_empty(args.url, NAME, "url")?;
    let result = fetch(
        WebFetchRequest {
            url,
            max_chars: args.max_chars,
            extract_mode: args.extract_mode,
        },
        &runtime.web_config().fetch,
    )
    .await?;
    serialize_success(NAME, &result)
}
