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
