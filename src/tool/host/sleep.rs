use crate::runtime::RuntimeHandle;
use crate::tool::ToolResult;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::sleep::*;

pub(crate) async fn execute(
    _runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sleep_rejects_unknown_top_level_fields() {
        let error = parse_sleep_args(&json!({
            "reason": "test",
            "duration_ms": 100,
            "summary": "should be rejected",
        }))
        .unwrap_err();
        let tool_error = crate::tool::tool_error_from_anyhow(&error);

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
        let tool_error = crate::tool::tool_error_from_anyhow(&error);

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
