use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily, WorkItemReadiness, WorkItemState},
};

use super::{
    serialize_success,
    work_item_query::{
        lifecycle_view, query_context_from_work_items, view_for_record_in_snapshot,
        WorkItemLifecycleView, WorkItemQueryContext, WorkItemView,
    },
    BuiltinToolDefinition,
};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::LIST_WORK_ITEMS;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum ListWorkItemsFilter {
    All,
    Open,
    Completing,
    Completed,
    Current,
    Queued,
    Yielded,
    Blocked,
    WaitingForOperator,
    Runnable,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListWorkItemsArgs {
    #[serde(default)]
    pub(crate) filter: Option<ListWorkItemsFilter>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_todo_list: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ListWorkItemsResult {
    pub(crate) context: WorkItemQueryContext,
    pub(crate) filter: ListWorkItemsFilter,
    pub(crate) returned: usize,
    pub(crate) total_matching: usize,
    pub(crate) limit: usize,
    pub(crate) work_items: Vec<WorkItemView>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListWorkItemsArgs>(
            NAME,
            include_str!("../tool_descriptions/list_work_items.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListWorkItemsArgs = parse_tool_args(NAME, input)?;
    let filter = args.filter.unwrap_or(ListWorkItemsFilter::All);
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let context = query_context_from_work_items(&snapshot.state, &snapshot.items);
    let _result_slots = runtime.reserve_work_item_query_slots(&snapshot, snapshot.items.len())?;
    let matching = std::mem::take(&mut snapshot.items)
        .into_iter()
        .filter(|projection| matches_filter(projection, &filter))
        .collect::<Vec<_>>();
    let total_matching = matching.len();
    let selected = matching.into_iter().take(limit).collect::<Vec<_>>();
    let mut _plan_materials = Vec::with_capacity(selected.len());
    let mut work_items = Vec::with_capacity(selected.len());
    for projection in selected {
        let plan_read = runtime.capture_work_item_plan_query(&snapshot, &projection.work_item)?;
        let record = projection.work_item.clone();
        let (view, material) = view_for_record_in_snapshot(
            runtime,
            &snapshot,
            record,
            args.include_todo_list,
            true,
            projection,
            plan_read,
        )?;
        work_items.push(view);
        _plan_materials.push(material);
    }
    let summary = format!(
        "Returned {} of {total_matching} matching work items.",
        work_items.len()
    );
    let mut result = serialize_success(
        NAME,
        &ListWorkItemsResult {
            context,
            filter,
            returned: work_items.len(),
            total_matching,
            limit,
            work_items,
        },
    )?;
    result.envelope.summary_text = Some(summary);
    drop(_plan_materials);
    Ok(result)
}

fn matches_filter(
    projection: &crate::work_item_scheduling::WorkItemSchedulingProjection,
    filter: &ListWorkItemsFilter,
) -> bool {
    match filter {
        ListWorkItemsFilter::All => true,
        ListWorkItemsFilter::Open => {
            lifecycle_view(&projection.work_item.state) == WorkItemLifecycleView::Open
        }
        ListWorkItemsFilter::Completing => {
            lifecycle_view(&projection.work_item.state) == WorkItemLifecycleView::Completing
        }
        ListWorkItemsFilter::Completed => {
            lifecycle_view(&projection.work_item.state) == WorkItemLifecycleView::Completed
        }
        ListWorkItemsFilter::Current => projection.is_current,
        ListWorkItemsFilter::Queued => {
            !projection.is_current
                && projection.work_item.state == WorkItemState::Open
                && projection.readiness == WorkItemReadiness::Runnable
        }
        ListWorkItemsFilter::Yielded => projection.readiness == WorkItemReadiness::Yielded,
        ListWorkItemsFilter::Blocked => {
            !projection.is_current && projection.readiness == WorkItemReadiness::Blocked
        }
        ListWorkItemsFilter::WaitingForOperator => {
            projection.readiness == WorkItemReadiness::WaitingForOperator
        }
        ListWorkItemsFilter::Runnable => projection.readiness == WorkItemReadiness::Runnable,
    }
}
