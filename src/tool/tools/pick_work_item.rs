use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::{RuntimeHandle, WorkItemFocusTransition},
    tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty},
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily, WorkItemRecord},
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const NAME: &str = "PickWorkItem";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PickWorkItemArgs {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PickWorkItemResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) previous_work_item: Option<WorkItemRecord>,
    pub(crate) current_work_item: WorkItemRecord,
    pub(crate) current_work_item_id: String,
    pub(crate) transition: WorkItemFocusTransition,
    pub(crate) binding_note: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<PickWorkItemArgs>(
            NAME,
            "Make an existing open work item the current work-item focus for this agent. Include reason when switching away from runnable current work; blocked work items may be picked for inspection but remain non-runnable.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: PickWorkItemArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let picked = runtime
        .pick_work_item_with_reason(work_item_id, normalize_optional_non_empty(args.reason))
        .await?;
    let previous_work_item = picked.previous_work_item;
    let current_work_item = picked.current_work_item;
    let current_work_item_id = current_work_item.id.clone();
    serialize_success(
        NAME,
        &PickWorkItemResult {
            previous_work_item,
            current_work_item,
            current_work_item_id,
            transition: picked.transition,
            binding_note: "subsequent tool calls in this turn are bound to the new current work item unless they explicitly specify another work_item_id".into(),
        },
    )
}
