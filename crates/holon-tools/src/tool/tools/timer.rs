use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::spec::typed_spec;
use crate::types::TimerRecord;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

pub const CREATE_NAME: &str = crate::tool::names::CREATE_TIMER;

pub const LIST_NAME: &str = crate::tool::names::LIST_TIMERS;

pub const GET_NAME: &str = crate::tool::names::GET_TIMER;

pub const CANCEL_NAME: &str = crate::tool::names::CANCEL_TIMER;

pub const DEFAULT_LIMIT: usize = 50;

pub const MAX_LIMIT: usize = 100;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTimerArgs {
    #[schemars(range(min = 1))]
    pub duration_ms: Option<u64>,
    #[schemars(range(min = 1))]
    pub interval_ms: Option<u64>,
    pub calendar_expression: Option<String>,
    pub timezone: Option<String>,
    /// Optional display title; not used in the trigger message.
    pub title: Option<String>,
    /// Text for each new timer wake. Do not combine with `summary`.
    pub instruction: Option<String>,
    /// Legacy alias for `instruction`.
    pub summary: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTimersArgs {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerIdArgs {
    pub timer_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListTimersResult<Timer = TimerRecord> {
    pub returned: usize,
    pub limit: usize,
    pub timers: Vec<Timer>,
}

pub fn create_definition() -> Result<BuiltinToolDefinition> {
    definition::<CreateTimerArgs>(
        CREATE_NAME,
        include_str!("../tool_descriptions/create_timer.md"),
    )
}

pub fn list_definition() -> Result<BuiltinToolDefinition> {
    definition::<ListTimersArgs>(
        LIST_NAME,
        include_str!("../tool_descriptions/list_timers.md"),
    )
}

pub fn get_definition() -> Result<BuiltinToolDefinition> {
    definition::<TimerIdArgs>(GET_NAME, include_str!("../tool_descriptions/get_timer.md"))
}

pub fn cancel_definition() -> Result<BuiltinToolDefinition> {
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

pub fn validate_positive(tool_name: &str, field: &str, value: u64) -> Result<()> {
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
