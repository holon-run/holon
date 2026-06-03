use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ModelProviderAvailability, ModelProviderEntry, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "ListModelProviders";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListModelProvidersArgs {
    #[serde(default)]
    pub(crate) include_unavailable: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ListModelProvidersResult {
    pub(crate) include_unavailable: bool,
    pub(crate) providers: Vec<ModelProviderEntry>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListModelProvidersArgs>(
            NAME,
            "List configured/discovered model providers. By default unavailable providers are omitted; set include_unavailable=true to inspect blocked options.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListModelProvidersArgs = parse_tool_args(NAME, input)?;
    let mut providers = runtime.model_providers().await?;
    if !args.include_unavailable {
        providers
            .retain(|provider| provider.availability != ModelProviderAvailability::Unavailable);
    }
    serialize_success(
        NAME,
        &ListModelProvidersResult {
            include_unavailable: args.include_unavailable,
            providers,
        },
    )
}
