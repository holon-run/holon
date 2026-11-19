use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::WEB_SEARCH;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebSearchArgs {
    pub query: String,
    pub max_results: Option<usize>,
    pub provider: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<WebSearchArgs>(
            NAME,
            include_str!("../tool_descriptions/web_search.md"),
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{helpers::parse_tool_args, ToolError};
    use serde_json::json;

    #[test]
    fn web_search_uses_provider_not_a_separate_mode_argument() {
        let args: WebSearchArgs =
            parse_tool_args(NAME, &json!({"query": "fixture", "provider": "aggregate"}))
                .expect("aggregate is an accepted provider selector");
        assert_eq!(args.provider.as_deref(), Some("aggregate"));

        let error = parse_tool_args::<WebSearchArgs>(
            NAME,
            &json!({"query": "fixture", "provider": "aggregate", "mode": "aggregate"}),
        )
        .err()
        .expect("unknown fields must be rejected");
        assert_eq!(
            error.downcast_ref::<ToolError>().unwrap().kind,
            "invalid_tool_input"
        );
    }
}
