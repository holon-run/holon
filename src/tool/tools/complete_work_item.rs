use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty},
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{
    serialize_success,
    work_item_action::WorkItemMutationResult,
    work_item_query::{query_context, view_for_record},
    BuiltinToolDefinition,
};

pub(crate) const NAME: &str = "CompleteWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteWorkItemArgs {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) result_summary: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<CompleteWorkItemArgs>(
            NAME,
            "Mark an open work item completed and optionally record a concise result summary.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CompleteWorkItemArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let work_item = runtime
        .complete_work_item(
            work_item_id,
            normalize_optional_non_empty(args.result_summary),
        )
        .await?;
    let context = query_context(runtime).await?;
    let work_item = view_for_record(runtime, &context, work_item, true).await?;
    serialize_success(NAME, &WorkItemMutationResult::new(work_item))
}
