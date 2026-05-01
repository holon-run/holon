use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel, WorkItemState},
};

use super::{
    serialize_success,
    work_item_query::{
        lifecycle_view, query_context, view_for_record, WorkItemFocusView, WorkItemLifecycleView,
        WorkItemQueryContext, WorkItemView,
    },
    BuiltinToolDefinition,
};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "ListWorkItems";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum ListWorkItemsFilter {
    All,
    Open,
    Done,
    Current,
    Queued,
    Blocked,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListWorkItemsArgs {
    #[serde(default)]
    pub(crate) filter: Option<ListWorkItemsFilter>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_plan: bool,
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
            "List recent work items with explicit current, open, done, queued, and blocked views. Use this before relying on memory briefs for work-item focus.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListWorkItemsArgs = parse_tool_args(NAME, input)?;
    let filter = args.filter.unwrap_or(ListWorkItemsFilter::All);
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let context = query_context(runtime).await?;
    let mut records = runtime.latest_work_items().await?;
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    let matching = records
        .into_iter()
        .filter(|record| matches_filter(record, &context, &filter))
        .collect::<Vec<_>>();
    let total_matching = matching.len();
    let selected = matching.into_iter().take(limit).collect::<Vec<_>>();
    let mut work_items = Vec::with_capacity(selected.len());
    for record in selected {
        work_items.push(view_for_record(runtime, &context, record, args.include_plan).await?);
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
    record: &crate::types::WorkItemRecord,
    context: &WorkItemQueryContext,
    filter: &ListWorkItemsFilter,
) -> bool {
    let is_current = context.current_work_item_id.as_deref() == Some(record.id.as_str())
        && record.state == WorkItemState::Open;
    match filter {
        ListWorkItemsFilter::All => true,
        ListWorkItemsFilter::Open => lifecycle_view(&record.state) == WorkItemLifecycleView::Open,
        ListWorkItemsFilter::Done => lifecycle_view(&record.state) == WorkItemLifecycleView::Done,
        ListWorkItemsFilter::Current => is_current,
        ListWorkItemsFilter::Queued => {
            !is_current
                && super::work_item_query::focus_view(record, is_current)
                    == WorkItemFocusView::Queued
        }
        ListWorkItemsFilter::Blocked => {
            !is_current
                && super::work_item_query::focus_view(record, is_current)
                    == WorkItemFocusView::Blocked
        }
    }
}
