use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

pub(crate) use holon_tools::builtins::memory_search::*;

#[derive(Serialize)]
struct MemorySearchResponse {
    query: String,
    results: Vec<crate::memory::MemorySearchResult>,
    index_status: crate::memory::MemorySearchIndexStatus,
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemorySearchArgs = parse_tool_args(NAME, input)?;
    let query = validate_non_empty(args.query, NAME, "query")?;
    let response = runtime
        .search_memory(
            &query,
            args.limit.unwrap_or(10),
            args.include_all_workspaces,
        )
        .await?;
    serialize_success(
        NAME,
        &MemorySearchResponse {
            query,
            results: response.results,
            index_status: response.index_status,
        },
    )
}
