use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::{spec::typed_spec, ToolResult},
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::BuiltinToolDefinition;
use crate::tool::helpers::{invalid_tool_input, parse_tool_args};

pub(crate) const NAME: &str = "Sleep";

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SleepArgs {
    pub(crate) reason: Option<String>,
    #[schemars(range(min = 1))]
    pub(crate) duration_ms: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<SleepArgs>(
            NAME,
            "Mark the current loop as done so the agent can sleep when no other work remains. Omit `duration_ms` for ordinary rest, or provide a positive short session-local delay to wake again.",
        )?,
    })
}

pub(crate) async fn execute(
    _runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<ToolResult> {
    let args = parse_sleep_args(input)?;
    let reason = args
        .reason
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "sleep requested".to_string());
    let sleep_duration_ms = args.duration_ms;

    Ok(ToolResult::sleep(
        NAME,
        json!({
            "reason": reason,
            "duration_ms": sleep_duration_ms,
        }),
        Some(
            sleep_duration_ms
                .map(|duration| format!("sleep requested for {duration} ms"))
                .unwrap_or_else(|| "sleep requested".to_string()),
        ),
        sleep_duration_ms,
    ))
}

fn parse_sleep_args(input: &Value) -> Result<SleepArgs> {
    let args: SleepArgs = parse_tool_args(NAME, input)?;
    if args.duration_ms == Some(0) {
        return Err(invalid_tool_input(
            NAME,
            "Sleep `duration_ms` must be a positive integer when provided",
            json!({
                "field": "duration_ms",
                "validation_error": "must be greater than 0",
            }),
            "omit `duration_ms` for ordinary terminal rest, or provide a positive integer millisecond delay",
        )
        .into());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;
    use serde_json::json;

    #[test]
    fn sleep_rejects_unknown_top_level_fields() {
        let error = parse_sleep_args(&json!({
            "reason": "test",
            "duration_ms": 100,
            "summary": "should be rejected",
        }))
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("parse_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|error| error.contains("unknown field `summary`")));
    }

    #[test]
    fn sleep_rejects_zero_duration_ms() {
        let error = parse_sleep_args(&json!({
            "reason": "pause briefly",
            "duration_ms": 0
        }))
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert_eq!(
            tool_error.message,
            "Sleep `duration_ms` must be a positive integer when provided"
        );
        assert!(tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("validation_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|error| error == "must be greater than 0"));
        assert!(tool_error
            .recovery_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("positive integer")));
    }
}
