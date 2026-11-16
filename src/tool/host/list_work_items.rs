use super::serialize_success;
use super::work_item_query::latest_delivery_summaries_by_work_item;
use super::work_item_query::lifecycle_view;
use super::work_item_query::query_context_from_work_items;
use super::work_item_query::view_for_record;
use super::work_item_query::WorkItemLifecycleView;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use crate::types::WorkItemReadiness;
use crate::types::WorkItemState;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::list_work_items::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListWorkItemsArgs = parse_tool_args(NAME, input)?;
    let filter = args.filter.unwrap_or(ListWorkItemsFilter::All);
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut snapshot = runtime.capture_work_query_snapshot().await?;
    let context = query_context_from_work_items(&snapshot.state, &snapshot.items);
    let matching = std::mem::take(&mut snapshot.items)
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
