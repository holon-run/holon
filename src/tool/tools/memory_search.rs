use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "MemorySearch";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemorySearchArgs {
    pub(crate) query: String,
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_all_workspaces: bool,
}

#[derive(Serialize)]
struct MemorySearchResponse {
    query: String,
    results: Vec<crate::memory::MemorySearchResult>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<MemorySearchArgs>(
            NAME,
            "Search Holon memory sources, including agent memory markdown and runtime evidence. Normal workspace markdown is not included.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemorySearchArgs = parse_tool_args(NAME, input)?;
    let query = validate_non_empty(args.query, NAME, "query")?;
    let results = runtime
        .search_memory(
            &query,
            args.limit.unwrap_or(10),
            args.include_all_workspaces,
        )
        .await?;
    serialize_success(NAME, &MemorySearchResponse { query, results })
}
