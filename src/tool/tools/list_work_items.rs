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
        latest_delivery_summaries_by_work_item, lifecycle_view, query_context, view_for_record,
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
    let context = query_context(runtime).await?;
    let matching = runtime
        .storage()
        .work_queue_read_model()?
        .items
        .into_iter()
        .filter(|projection| matches_filter(projection, &filter))
        .collect::<Vec<_>>();
    let total_matching = matching.len();
    let selected = matching.into_iter().take(limit).collect::<Vec<_>>();
    let delivery_summaries = if selected
        .iter()
        .any(|projection| projection.work_item.state == WorkItemState::Completed)
    {
        Some(latest_delivery_summaries_by_work_item(runtime)?)
    } else {
        None
    };
    let mut work_items = Vec::with_capacity(selected.len());
    for projection in selected {
        let record = projection.work_item.clone();
        work_items.push(
            view_for_record(
                runtime,
                &context,
                record,
                args.include_todo_list,
                delivery_summaries.as_ref(),
                Some(projection),
            )
            .await?,
        );
    }
    serialize_success(
        NAME,
        &ListWorkItemsResult {
            context,
            filter,
            returned: work_items.len(),
            total_matching,
            limit,
            work_items,
        },
    )
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
