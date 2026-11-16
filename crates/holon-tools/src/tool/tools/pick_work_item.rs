use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use crate::types::WorkItemRecord;
use anyhow::Result;
use holon_domain::WorkItemContinuationSummary;
use holon_domain::WorkItemFocusTransition;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::PICK_WORK_ITEM;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PickWorkItemArgs {
    pub work_item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub clear_blocker: bool,
}

#[derive(Serialize)]
pub struct PickWorkItemResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_work_item: Option<WorkItemRecord>,
    pub current_work_item: WorkItemRecord,
    pub current_work_item_id: String,
    pub transition: WorkItemFocusTransition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_created: Option<WorkItemContinuationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_resolved: Option<WorkItemContinuationSummary>,
    pub terminal_transition: bool,
    pub binding_note: String,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<PickWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/pick_work_item.md"),
        )?,
    })
}
