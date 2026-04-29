use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel, WorkItemStatus},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "UpdateWorkItem";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WorkItemStatusArgs {
    Active,
    Queued,
    Waiting,
    Completed,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateWorkItemArgs {
    pub(crate) id: Option<String>,
    pub(crate) delivery_target: String,
    pub(crate) status: WorkItemStatusArgs,
    pub(crate) summary: Option<String>,
    pub(crate) progress_note: Option<String>,
    pub(crate) parent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<UpdateWorkItemArgs>(
            NAME,
            "Create or replace the latest snapshot for one work item. Omit id to create a new work item.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: UpdateWorkItemArgs = parse_tool_args(NAME, input)?;
    let delivery_target = validate_non_empty(args.delivery_target, NAME, "delivery_target")?;
    let record = runtime
        .update_work_item(
            normalize_optional_non_empty(args.id),
            delivery_target,
            match args.status {
                WorkItemStatusArgs::Active => WorkItemStatus::Active,
                WorkItemStatusArgs::Queued => WorkItemStatus::Queued,
                WorkItemStatusArgs::Waiting => WorkItemStatus::Waiting,
                WorkItemStatusArgs::Completed => WorkItemStatus::Completed,
            },
            normalize_optional_non_empty(args.summary),
            normalize_optional_non_empty(args.progress_note),
            normalize_optional_non_empty(args.parent_id),
        )
        .await?;
    serialize_success(NAME, &record)
}
