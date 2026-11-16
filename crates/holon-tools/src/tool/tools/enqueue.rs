use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::ENQUEUE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum EnqueuePriority {
    // TODO: remove `interrupt` alias after older prompt/tool-call contexts have migrated.
    #[serde(alias = "interrupt")]
    Interject,
    Next,
    Normal,
    Background,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnqueueArgs {
    pub text: String,
    pub priority: Option<EnqueuePriority>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<EnqueueArgs>(NAME, include_str!("../tool_descriptions/enqueue.md"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_interrupt_priority_deserializes_as_interject() {
        let args: EnqueueArgs = serde_json::from_value(serde_json::json!({
            "text": "follow up",
            "priority": "interrupt"
        }))
        .unwrap();

        assert!(matches!(args.priority, Some(EnqueuePriority::Interject)));
    }
}
