use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::SLEEP;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SleepArgs {
    pub reason: Option<String>,
    #[schemars(range(min = 1))]
    pub duration_ms: Option<u64>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<SleepArgs>(NAME, include_str!("../tool_descriptions/sleep.md"))?,
    })
}

pub fn parse_sleep_args(input: &Value) -> Result<SleepArgs> {
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
