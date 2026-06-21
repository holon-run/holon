use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    model_discovery::ModelDiscoveryCacheStatus,
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
    #[schemars(
        description = "Diagnostic-only. Defaults to false so normal selection paths only see usable providers; set true only to inspect blocked provider options and unavailable reasons."
    )]
    #[serde(default)]
    pub(crate) include_unavailable: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ListModelProvidersResult {
    pub(crate) include_unavailable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) model_discovery_cache: Vec<ModelDiscoveryCacheStatus>,
    pub(crate) providers: Vec<ModelProviderEntry>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListModelProvidersArgs>(
            NAME,
            include_str!("../tool_descriptions/list_model_providers.md"),
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
    let model_discovery_cache = runtime.model_discovery_status().await?;
    if !args.include_unavailable {
        providers
            .retain(|provider| provider.availability != ModelProviderAvailability::Unavailable);
    }
    serialize_success(
        NAME,
        &ListModelProvidersResult {
            include_unavailable: args.include_unavailable,
            model_discovery_cache,
            providers,
        },
    )
}
