use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{
    serialize_success,
    work_item_query::{
        projection_outside_query_window, query_context_from_work_items,
        view_for_recalled_record_observed, view_for_record_in_snapshot, WorkItemQueryContext,
        WorkItemView,
    },
    BuiltinToolDefinition,
};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::GET_WORK_ITEM;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetWorkItemArgs {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) include_todo_list: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct GetWorkItemResult {
    pub(crate) context: WorkItemQueryContext,
    pub(crate) work_item: WorkItemView,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetWorkItemArgs>(
            NAME,
            include_str!("../tool_descriptions/get_work_item.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: GetWorkItemArgs = parse_tool_args(NAME, input)?;
    let work_item_id = validate_non_empty(args.work_item_id, NAME, "work_item_id")?;
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let context = query_context_from_work_items(&snapshot.state, &snapshot.items);
    let projection = snapshot
        .items
        .iter()
        .find(|item| item.work_item.id == work_item_id);
    let (record, projection, _historical_budget, plan_read) =
        if let Some(selected) = snapshot.selected_work_item(&work_item_id) {
            // Owning closure can select a closed Work body outside display heads.
            let plan = runtime.capture_work_item_plan_query(&snapshot, selected)?;
            let projection = projection
                .cloned()
                .unwrap_or_else(|| projection_outside_query_window(&context, selected));
            (selected.clone(), projection, None, plan)
        } else {
            // Original by-ID recall only when this publication did not select it.
            let read = runtime
                .inspect_historical_work_item(&snapshot, &work_item_id)?
                .ok_or_else(|| {
                    crate::tool::ToolError::new(
                        "unknown_work_item",
                        format!("unknown work item {work_item_id}"),
                    )
                })?;
            let (record, budget) = read.into_parts();
            let projection = projection_outside_query_window(&context, &record);
            let plan = runtime.inspect_historical_work_item_plan(&snapshot, &record)?;
            (record, projection, Some(budget), plan)
        };
    let (work_item, _plan_material) = if _historical_budget.is_none() {
        view_for_record_in_snapshot(
            runtime,
            &snapshot,
            record,
            args.include_todo_list,
            false,
            projection,
            plan_read,
        )?
    } else {
        // Explicit recall keeps its separate named original inspection cause.
        view_for_recalled_record_observed(
            runtime,
            record,
            args.include_todo_list,
            _historical_budget
                .as_ref()
                .expect("explicit recall material retained"),
            projection,
            plan_read,
        )?
    };
    let result = serialize_success(NAME, &GetWorkItemResult { context, work_item });
    drop(_plan_material);
    result
}
