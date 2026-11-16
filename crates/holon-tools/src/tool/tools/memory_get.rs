use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

pub const NAME: &str = crate::tool::names::MEMORY_GET;

pub const MAX_CHARS_MAX: usize = 50_000;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryGetArgs {
    pub source_ref: String,
    #[schemars(range(min = 1, max = 50000))]
    pub max_chars: Option<usize>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<MemoryGetArgs>(
            NAME,
            include_str!("../tool_descriptions/memory_get.md"),
        )?,
    })
}

pub fn validate_max_chars(max_chars: Option<usize>) -> Result<Option<usize>> {
    let Some(max_chars) = max_chars else {
        return Ok(None);
    };
    if !(1..=MAX_CHARS_MAX).contains(&max_chars) {
        return Err(invalid_tool_input(
            NAME,
            "MemoryGet `max_chars` must be between 1 and 50000 when provided",
            json!({
                "field": "max_chars",
                "max_chars": max_chars,
                "validation_error": "out of range",
                "minimum": 1,
                "maximum": MAX_CHARS_MAX,
            }),
            "omit `max_chars` for the default bound, or provide an integer from 1 through 50000",
        ));
    }
    Ok(Some(max_chars))
}
