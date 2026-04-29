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
use crate::tool::helpers::{extract_sleep_duration_ms, extract_sleep_reason};

pub(crate) const NAME: &str = "Sleep";

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct SleepArgs {
    pub(crate) reason: Option<String>,
    pub(crate) duration_ms: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<SleepArgs>(
            NAME,
            "Mark the current loop as done so the agent can sleep when no other work remains, or wake again after a short session-local `duration_ms` delay.",
        )?,
    })
}

pub(crate) async fn execute(
    _runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<ToolResult> {
    let reason = extract_sleep_reason(input);
    let sleep_duration_ms = extract_sleep_duration_ms(input)?;
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
