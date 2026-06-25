use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{
        AuthorityClass, EnqueueResult, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, ToolCapabilityFamily,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::ENQUEUE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum EnqueuePriority {
    // TODO: remove `interrupt` alias after older prompt/tool-call contexts have migrated.
    #[serde(alias = "interrupt")]
    Interject,
    Next,
    Normal,
    Background,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnqueueArgs {
    pub(crate) text: String,
    pub(crate) priority: Option<EnqueuePriority>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<EnqueueArgs>(NAME, include_str!("../tool_descriptions/enqueue.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: EnqueueArgs = parse_tool_args(NAME, input)?;
    let text = validate_non_empty(args.text, NAME, "text")?;
    let priority = match args.priority.unwrap_or(EnqueuePriority::Next) {
        EnqueuePriority::Interject => Priority::Interject,
        EnqueuePriority::Next => Priority::Next,
        EnqueuePriority::Normal => Priority::Normal,
        EnqueuePriority::Background => Priority::Background,
    };
    let message = MessageEnvelope::new(
        agent_id.to_string(),
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "tool_enqueue".into(),
        },
        authority_class.clone(),
        priority.clone(),
        MessageBody::Text { text: text.clone() },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    runtime.enqueue(message).await?;
    serialize_success(
        NAME,
        &EnqueueResult {
            enqueued: true,
            priority,
            follow_up_text: text.clone(),
            summary_text: Some(format!("enqueued follow-up: {text}")),
        },
    )
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
