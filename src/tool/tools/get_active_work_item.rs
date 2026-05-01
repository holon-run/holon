use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{
    serialize_success,
    work_item_query::{
        latest_current_record, query_context, view_for_record, WorkItemQueryContext, WorkItemView,
    },
    BuiltinToolDefinition,
};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "GetActiveWorkItem";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetActiveWorkItemArgs {
    #[serde(default)]
    pub(crate) include_plan: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct GetActiveWorkItemResult {
    pub(crate) context: WorkItemQueryContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item: Option<WorkItemView>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetActiveWorkItemArgs>(
            NAME,
            "Read the agent's current work-item focus. The result is empty when no open current work item exists.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: GetActiveWorkItemArgs = parse_tool_args(NAME, input)?;
    let context = query_context(runtime).await?;
    let work_item = match latest_current_record(runtime, &context).await? {
        Some(record) => Some(view_for_record(runtime, &context, record, args.include_plan).await?),
        None => None,
    };
    serialize_success(NAME, &GetActiveWorkItemResult { context, work_item })
}
