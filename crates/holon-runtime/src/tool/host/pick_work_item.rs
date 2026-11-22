use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::pick_work_item::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
    context: &crate::tool::spec::ToolExecutionContext,
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
        .pick_work_item_in_lane(
            work_item_id,
            reason,
            args.clear_blocker,
            Some(crate::runtime_db::transitions::WorkCommandLane::from_tool_context(context)),
        )
        .await?;
    let previous_work_item = picked.previous_work_item;
    let current_work_item = picked.current_work_item;
    let current_work_item_id = current_work_item.id.clone();
    let terminal_transition = picked.transition.terminal_transition;
    let mut result = serialize_success(
        NAME,
        &PickWorkItemResult {
            previous_work_item,
            current_work_item,
            current_work_item_id,
            transition: picked.transition,
            continuation_created: picked.continuation_created,
            continuation_resolved: picked.continuation_resolved,
            terminal_transition,
            binding_note: if terminal_transition {
                "the current activation yielded; the target work item becomes effective in the next activation".into()
            } else {
                "the selected work item is now the durable current focus".into()
            },
        },
    )?;
    result.envelope.summary_text = Some(format!(
        "Selected work item {}; terminal_transition={terminal_transition}.",
        result
            .envelope
            .result
            .as_ref()
            .and_then(|value| value["current_work_item_id"].as_str())
            .unwrap_or("unknown")
    ));
    if terminal_transition {
        result.should_sleep = true;
        result.terminal_transition = true;
    }
    Ok(result)
}
