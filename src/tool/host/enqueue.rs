use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::EnqueueResult;
use crate::types::MessageBody;
use crate::types::MessageDeliverySurface;
use crate::types::MessageEnvelope;
use crate::types::MessageKind;
use crate::types::MessageOrigin;
use crate::types::Priority;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::enqueue::*;

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
    let state = runtime.agent_state().await?;
    let work_item_id = state
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.work_item_id.clone())
        .or(state.current_turn_work_item_id);
    let mut message = MessageEnvelope::new(
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
    message.work_item_id = work_item_id;
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
