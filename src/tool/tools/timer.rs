use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::RuntimeHandle,
    runtime_error::RuntimeError,
    tool::{
        helpers::{invalid_tool_input, parse_tool_args, validate_non_empty},
        spec::typed_spec,
    },
    types::{TimerRecord, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const CREATE_NAME: &str = crate::tool::names::CREATE_TIMER;
pub(crate) const LIST_NAME: &str = crate::tool::names::LIST_TIMERS;
pub(crate) const GET_NAME: &str = crate::tool::names::GET_TIMER;
pub(crate) const CANCEL_NAME: &str = crate::tool::names::CANCEL_TIMER;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateTimerArgs {
    #[schemars(range(min = 1))]
    pub(crate) duration_ms: u64,
    #[schemars(range(min = 1))]
    pub(crate) interval_ms: Option<u64>,
    pub(crate) summary: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTimersArgs {
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimerIdArgs {
    pub(crate) timer_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ListTimersResult {
    pub(crate) returned: usize,
    pub(crate) limit: usize,
    pub(crate) timers: Vec<TimerRecord>,
}

pub(crate) fn create_definition() -> Result<BuiltinToolDefinition> {
    definition::<CreateTimerArgs>(
        CREATE_NAME,
        include_str!("../tool_descriptions/create_timer.md"),
    )
}

pub(crate) fn list_definition() -> Result<BuiltinToolDefinition> {
    definition::<ListTimersArgs>(
        LIST_NAME,
        include_str!("../tool_descriptions/list_timers.md"),
    )
}

pub(crate) fn get_definition() -> Result<BuiltinToolDefinition> {
    definition::<TimerIdArgs>(GET_NAME, include_str!("../tool_descriptions/get_timer.md"))
}

pub(crate) fn cancel_definition() -> Result<BuiltinToolDefinition> {
    definition::<TimerIdArgs>(
        CANCEL_NAME,
        include_str!("../tool_descriptions/cancel_timer.md"),
    )
}

fn definition<T: JsonSchema + 'static>(
    name: &str,
    description: &str,
) -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<T>(name, description)?,
    })
}

pub(crate) async fn create(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateTimerArgs = parse_tool_args(CREATE_NAME, input)?;
    validate_positive(CREATE_NAME, "duration_ms", args.duration_ms)?;
    if let Some(interval_ms) = args.interval_ms {
        validate_positive(CREATE_NAME, "interval_ms", interval_ms)?;
    }
    let timer = runtime
        .schedule_timer(args.duration_ms, args.interval_ms, args.summary)
        .await?;
    serialize_success(CREATE_NAME, &timer)
}

pub(crate) async fn list(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListTimersArgs = parse_tool_args(LIST_NAME, input)?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let timers = runtime.recent_timers(limit).await?;
    serialize_success(
        LIST_NAME,
        &ListTimersResult {
            returned: timers.len(),
            limit,
            timers,
        },
    )
}

pub(crate) async fn get(runtime: &RuntimeHandle, input: &Value) -> Result<crate::tool::ToolResult> {
    let args: TimerIdArgs = parse_tool_args(GET_NAME, input)?;
    let timer_id = validate_non_empty(args.timer_id, GET_NAME, "timer_id")?;
    let agent_id = runtime.agent_state().await?.id;
    let timer = runtime
        .latest_timer(&timer_id)
        .await?
        .filter(|timer| timer.agent_id == agent_id)
        .ok_or_else(|| timer_not_found(&timer_id))?;
    serialize_success(GET_NAME, &timer)
}

pub(crate) async fn cancel(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TimerIdArgs = parse_tool_args(CANCEL_NAME, input)?;
    let timer_id = validate_non_empty(args.timer_id, CANCEL_NAME, "timer_id")?;
    let timer = runtime.cancel_timer(&timer_id).await?;
    serialize_success(CANCEL_NAME, &timer)
}

fn validate_positive(tool_name: &str, field: &str, value: u64) -> Result<()> {
    if value > 0 {
        return Ok(());
    }
    Err(invalid_tool_input(
        tool_name,
        format!("{tool_name} `{field}` must be a positive integer"),
        json!({
            "field": field,
            "validation_error": "must be greater than 0",
        }),
        format!("provide a positive integer millisecond value for `{field}`"),
    )
    .into())
}

fn timer_not_found(timer_id: &str) -> anyhow::Error {
    RuntimeError::not_found("timer_not_found", format!("timer {timer_id} not found"))
        .with_safe_context("timer_id", timer_id)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;

    #[test]
    fn create_timer_rejects_unknown_fields_and_zero_durations() {
        let error = serde_json::from_value::<CreateTimerArgs>(json!({
            "duration_ms": 1,
            "agent_id": "other",
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field `agent_id`"));

        let error = validate_positive(CREATE_NAME, "duration_ms", 0).unwrap_err();
        assert_eq!(ToolError::from_anyhow(&error).kind, "invalid_tool_input");
    }

    #[test]
    fn list_timer_limit_is_bounded() {
        let args: ListTimersArgs = parse_tool_args(LIST_NAME, &json!({"limit": 0})).unwrap();
        assert_eq!(args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT), 1);
        let args: ListTimersArgs = parse_tool_args(LIST_NAME, &json!({"limit": 500})).unwrap();
        assert_eq!(args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT), 100);
    }
}
