use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::RuntimeHandle,
    tool::{spec::typed_spec, ToolError},
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "MemoryGet";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryGetArgs {
    pub(crate) source_ref: String,
    pub(crate) max_chars: Option<usize>,
}

#[derive(Serialize)]
struct MemoryGetResponse {
    memory: crate::memory::MemoryGetResult,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<MemoryGetArgs>(
            NAME,
            "Fetch exact bounded Holon memory content by a source_ref returned from MemorySearch.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemoryGetArgs = parse_tool_args(NAME, input)?;
    let source_ref = validate_non_empty(args.source_ref, NAME, "source_ref")?;
    let Some(memory) = runtime.get_memory(&source_ref, args.max_chars).await? else {
        return Err(ToolError::new(
            "memory_source_not_found",
            format!("memory source `{source_ref}` was not found"),
        )
        .with_details(json!({ "source_ref": source_ref }))
        .with_recovery_hint(
            "call MemorySearch first and pass one of its returned source_ref values",
        )
        .into());
    };
    serialize_success(NAME, &MemoryGetResponse { memory })
}
