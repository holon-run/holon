use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::{RuntimeHandle, WorkItemContinuationSummary, WorkItemFocusTransition},
    tool::helpers::{
        invalid_tool_input, normalize_optional_non_empty, parse_tool_args, validate_non_empty,
    },
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
    #[serde(default)]
    pub(crate) clear_blocker: bool,
}

#[derive(Serialize)]
pub(crate) struct PickWorkItemResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) previous_work_item: Option<WorkItemRecord>,
    pub(crate) current_work_item: WorkItemRecord,
    pub(crate) current_work_item_id: String,
    pub(crate) transition: WorkItemFocusTransition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_created: Option<WorkItemContinuationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_resolved: Option<WorkItemContinuationSummary>,
    pub(crate) binding_note: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<PickWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/pick_work_item.md"),
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
    let reason = normalize_optional_non_empty(args.reason);
    if args.clear_blocker && reason.is_none() {
        return Err(invalid_tool_input(
            NAME,
            "PickWorkItem clear_blocker requires a non-empty `reason`",
            serde_json::json!({
                "field": "reason",
                "clear_blocker": true,
                "validation_error": "must be provided when clear_blocker is true",
            }),
            "provide `reason` explaining why the blocker is resolved, or omit clear_blocker for inspection focus",
        ));
    }
    let picked = runtime
        .pick_work_item_with_reason_and_clear_blocker(work_item_id, reason, args.clear_blocker)
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
            continuation_created: picked.continuation_created,
            continuation_resolved: picked.continuation_resolved,
            binding_note: "subsequent tool calls in this turn are bound to the new current work item unless they explicitly specify another work_item_id".into(),
        },
    )
}
