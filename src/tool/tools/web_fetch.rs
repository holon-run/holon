use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
    web::fetch::{fetch_with_provider, ExtractMode, WebFetchRequest},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::WEB_FETCH;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebFetchArgs {
    pub(crate) url: String,
    pub(crate) provider: Option<String>,
    pub(crate) max_chars: Option<usize>,
    #[serde(default)]
    pub(crate) extract_mode: ExtractMode,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<WebFetchArgs>(NAME, include_str!("../tool_descriptions/web_fetch.md"))?,
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_fetch_provider_is_optional_and_advertised() {
        let spec = definition().unwrap().spec;
        assert!(spec.input_schema["properties"].get("provider").is_some());
        let args: WebFetchArgs =
            serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert!(args.provider.is_none());
        let args: WebFetchArgs = serde_json::from_value(json!({
            "url": "https://example.com", "provider": "deepseek"
        }))
        .unwrap();
        assert_eq!(args.provider.as_deref(), Some("deepseek"));
    }
}
