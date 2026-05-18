use crate::tool::helpers::{invalid_tool_input, parse_tool_args_with_recovery_hint};
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
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("sleep requested");
    let sleep_duration_ms = args.duration_ms.filter(|duration| *duration > 0);
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
    let args: SleepArgs = parse_tool_args_with_recovery_hint(NAME, input, || {
        "provide only `reason` and optional `duration_ms` that match the Sleep tool schema"
            .to_string()
    })?;
    if let Some(0) = args.duration_ms {
        return Err(invalid_tool_input(
            NAME,
            "Sleep `duration_ms` must be a positive integer",
            json!({
                "field": "duration_ms",
                "validation_error": "must be greater than 0",
            }),
            "omit `duration_ms` for ordinary sleep, or provide a positive integer millisecond delay",
        ));
    }
    Ok(SleepArgs {
        reason: args.reason,
        duration_ms: args.duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;

    #[test]
    fn parse_sleep_args_rejects_unknown_fields() {
        let error = parse_sleep_args(&serde_json::json!({
            "reason": "done",
            "summary": "should be rejected",
        }))
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error.details.as_ref().unwrap()["parse_error"]
            .as_str()
            .unwrap()
            .contains("unknown field"));
    }

    #[test]
    fn parse_sleep_args_rejects_zero_duration() {
        let error = parse_sleep_args(&serde_json::json!({
            "reason": "pause",
            "duration_ms": 0,
        }))
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert_eq!(tool_error.details.as_ref().unwrap()["field"], "duration_ms");
        assert!(tool_error
            .recovery_hint
            .as_deref()
            .unwrap_or_default()
            .contains("positive integer"));
    }

    #[test]
    fn parse_sleep_args_accepts_defaulted_duration() {
        let args = parse_sleep_args(&serde_json::json!({
            "reason": "pause",
        }))
        .unwrap();
        assert_eq!(args.reason.as_deref(), Some("pause"));
        assert_eq!(args.duration_ms, None);
    }
}
