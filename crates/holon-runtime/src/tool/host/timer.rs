use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::runtime_error::RuntimeError;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::timer::*;

pub(crate) async fn create(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateTimerArgs = parse_tool_args(CREATE_NAME, input)?;
    if let Some(duration_ms) = args.duration_ms {
        validate_positive(CREATE_NAME, "duration_ms", duration_ms)?;
    }
    if let Some(interval_ms) = args.interval_ms {
        validate_positive(CREATE_NAME, "interval_ms", interval_ms)?;
    }
    if args.calendar_expression.is_some()
        && (args.duration_ms.is_some() || args.interval_ms.is_some())
    {
        return Err(invalid_tool_input(
            CREATE_NAME,
            "calendar_expression cannot be combined with duration_ms or interval_ms",
            json!({"fields": ["calendar_expression", "duration_ms", "interval_ms"]}),
            "provide either a calendar schedule or a relative schedule",
        )
        .into());
    }
    let timer = match (args.calendar_expression, args.timezone) {
        (Some(expression), Some(timezone)) => {
            runtime
                .schedule_calendar_timer(expression, timezone, args.summary)
                .await?
        }
        (None, None) => {
            runtime
                .schedule_timer(
                    args.duration_ms.ok_or_else(|| {
                        invalid_tool_input(
                            CREATE_NAME,
                            "duration_ms is required for relative timers",
                            json!({"field": "duration_ms"}),
                            "provide duration_ms or provide calendar_expression and timezone",
                        )
                    })?,
                    args.interval_ms,
                    args.summary,
                )
                .await?
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(invalid_tool_input(
                CREATE_NAME,
                "calendar_expression and timezone must be provided together",
                json!({"fields": ["calendar_expression", "timezone"]}),
                "provide both calendar_expression and timezone",
            )
            .into())
        }
    };
    serialize_success(CREATE_NAME, &timer)
}

pub(crate) async fn list(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListTimersArgs = parse_tool_args(LIST_NAME, input)?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let read = runtime.acquire_scheduling_records_read(limit).await?;
    let timers = read.timers;
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
    let read = runtime.acquire_timer_record_read(&timer_id).await?;
    let timer = read.record()?.ok_or_else(|| timer_not_found(&timer_id))?;
    serialize_success(GET_NAME, timer)
}

pub(crate) async fn cancel(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TimerIdArgs = parse_tool_args(CANCEL_NAME, input)?;
    let timer_id = validate_non_empty(args.timer_id, CANCEL_NAME, "timer_id")?;
    let cancellation = runtime.cancel_timer_with_evidence(&timer_id).await?;
    serialize_success(CANCEL_NAME, &cancellation)
}

fn timer_not_found(timer_id: &str) -> anyhow::Error {
    RuntimeError::not_found("timer_not_found", format!("timer {timer_id} not found"))
        .with_safe_context("timer_id", timer_id)
        .into()
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_timer_limit_is_bounded() {
        let args: ListTimersArgs = parse_tool_args(LIST_NAME, &json!({"limit": 0})).unwrap();
        assert_eq!(args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT), 1);
        let args: ListTimersArgs = parse_tool_args(LIST_NAME, &json!({"limit": 500})).unwrap();
        assert_eq!(args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT), 100);
    }
}
