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
        projection_outside_query_window, query_context_from_work_items, view_for_record,
        WorkItemQueryContext, WorkItemView,
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
    let snapshot = runtime.capture_work_query_snapshot().await?;
    let context = query_context_from_work_items(&snapshot.state, &snapshot.items);
    let projection = snapshot
        .items
        .iter()
        .find(|item| item.work_item.id == work_item_id)
        .cloned();
    let (record, projection, _historical_budget) = match projection {
        Some(projection) => (projection.work_item.clone(), projection, None),
        None => {
            // Get names an explicit historical reference. Preserve the existing
            // by-ID query (including foreign records), without adopting it or
            // changing the ordinary scheduling window.
            let record = runtime
                .latest_work_item(&work_item_id)
                .await?
                .ok_or_else(|| {
                    crate::tool::ToolError::new(
                        "unknown_work_item",
                        format!("unknown work item {work_item_id}"),
                    )
                })?;
            let projection = projection_outside_query_window(&context, &record);
            let budget = runtime.account_historical_work_query_record(&snapshot, &record)?;
            (record, projection, Some(budget))
        }
    };
    let work_item = view_for_record(
        runtime,
        &context,
        record,
        args.include_todo_list,
        None,
        Some(projection),
    )
    .await?;
    serialize_success(NAME, &GetWorkItemResult { context, work_item })
}
