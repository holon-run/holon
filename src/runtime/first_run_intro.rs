use anyhow::Result;
use serde_json::json;

use crate::{
    config::AppConfig,
    types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority,
    },
};

use super::RuntimeHandle;

const FIRST_RUN_INTRO_SUBSYSTEM: &str = "first_run_intro";

pub async fn maybe_enqueue_first_run_intro(
    config: &AppConfig,
    runtime: &RuntimeHandle,
) -> Result<()> {
    if !config.default_provider_ready() {
        return Ok(());
    }
    let state = runtime.agent_state().await?;
    if state.total_message_count > 0 {
        return Ok(());
    }
    runtime
        .enqueue(first_run_intro_message(state.id.clone()))
        .await?;
    Ok(())
}

fn first_run_intro_message(agent_id: impl Into<String>) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: FIRST_RUN_INTRO_SUBSYSTEM.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: first_run_intro_prompt().to_string(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.metadata = Some(json!({
        "first_run_intro": true,
        "reason": "provider_configured",
        "one_time": true,
    }));
    message
}

fn first_run_intro_prompt() -> &'static str {
    "This is the first run of Holon with a model provider configured. \
        Introduce yourself to the user: greet them, briefly explain what you can do as a Holon \
        agent, and suggest a few starter actions they could try. Keep it friendly and concise — \
        this is a one-time welcome, not a generic bot template. Choose the user's most likely \
        language from trusted preferences and environment hints such as locale or timezone; do not \
        claim certainty, and adapt if the user replies in another language."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContinuationTriggerKind, MessageBody};

    #[test]
    fn first_run_intro_is_runtime_owned_internal_followup() {
        let mut message = first_run_intro_message("default");
        message.normalize_admission_fields();

        assert_eq!(message.kind, MessageKind::InternalFollowup);
        assert_eq!(
            message.origin,
            MessageOrigin::System {
                subsystem: FIRST_RUN_INTRO_SUBSYSTEM.into()
            }
        );
        assert_eq!(message.authority_class, AuthorityClass::RuntimeInstruction);
        assert_eq!(
            message.delivery_surface,
            Some(MessageDeliverySurface::RuntimeSystem)
        );
        assert_eq!(
            message.admission_context,
            Some(AdmissionContext::RuntimeOwned)
        );
        assert_eq!(
            message.trigger_kind,
            Some(ContinuationTriggerKind::InternalFollowup)
        );
        assert_eq!(
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("first_run_intro"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn first_run_intro_prompt_includes_language_hint() {
        let MessageBody::Text { text } = first_run_intro_message("default").body else {
            panic!("first-run intro should be text");
        };

        assert!(text.contains("most likely language"));
        assert!(text.contains("locale or timezone"));
        assert!(text.contains("adapt if the user replies in another language"));
    }
}
