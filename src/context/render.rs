//! Generic message/body/header rendering helpers and label functions.

use crate::prompt::{PromptSection, PromptStability};
use crate::tool::helpers::truncate_text;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
    MessageOrigin,
};

/// Create a section with `AgentScoped` stability.
pub(super) fn section(name: &'static str, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability: PromptStability::AgentScoped,
    }
}

/// Create a section with `TurnScoped` stability.
pub(super) fn turn_section(name: &'static str, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability: PromptStability::TurnScoped,
    }
}

/// Sanitize a string for inline display: collapse whitespace, no newlines.
pub(super) fn sanitize_inline(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !sanitized.is_empty();
        } else {
            if pending_space {
                sanitized.push(' ');
                pending_space = false;
            }
            sanitized.push(ch);
        }
    }
    sanitized
}

/// Truncate and sanitize a value for inline display.
pub(super) fn bounded_inline(value: &str, max_chars: usize) -> String {
    truncate_text(&sanitize_inline(value), max_chars)
}

/// Indent every line of a text block by the given number of spaces.
pub(super) fn indent_block(text: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Get a short text preview of a message body.
pub(super) fn body_preview(body: &MessageBody) -> String {
    let text = message_body_text(body);
    if text.chars().count() <= 160 {
        text
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}

/// Extract text from a message body variant.
pub(super) fn message_body_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

/// Build the bracketed header label for a message (origin, surface, trust, etc.).
pub(super) fn message_header(message: &MessageEnvelope) -> String {
    let mut labels = vec![origin_label(&message.origin).to_string()];
    if let Some(surface) = message.delivery_surface {
        labels.push(delivery_surface_label(surface).to_string());
    }
    if let Some(context) = message.admission_context {
        labels.push(admission_context_label(context).to_string());
    }
    if let Some(trigger_kind) = message.trigger_kind {
        labels.push(format!("trigger:{}", enum_label(&trigger_kind)));
    }
    if let Some(work_item_id) = message.work_item_id.as_deref() {
        labels.push(format!("work_item:{}", header_label_value(work_item_id)));
    }
    if let Some(task_id) = message.task_id.as_deref() {
        labels.push(format!("task:{}", header_label_value(task_id)));
    }
    labels.push(authority_class_label(message.authority_class).to_string());
    labels.push(kind_label(message));
    format!("[{}]", labels.join("]["))
}

/// Render runtime-owned reply expectations without changing the message body.
pub(super) fn message_reply_expectation_context(message: &MessageEnvelope) -> Option<String> {
    if message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return None;
    }
    let expectation = message
        .metadata
        .as_ref()?
        .get("agent_message_reply_expectation")?;
    if expectation.get("mode").and_then(|value| value.as_str()) != Some("required") {
        return None;
    }
    let sender_agent_id = expectation
        .get("sender_agent_id")
        .and_then(|value| value.as_str())?;
    let waiting_task_id = expectation
        .get("waiting_task_id")
        .and_then(|value| value.as_str())?;
    let request_delivery_id = expectation
        .get("request_delivery_id")
        .and_then(|value| value.as_str())?;
    let delivery = message.metadata.as_ref()?.get("agent_message_delivery")?;
    if !matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == waiting_task_id)
        || message.correlation_id.as_deref() != Some(waiting_task_id)
        || expectation
            .get("request_message_id")
            .and_then(|value| value.as_str())
            != Some(message.id.as_str())
        || delivery
            .get("caller_agent_id")
            .and_then(|value| value.as_str())
            != Some(sender_agent_id)
        || delivery.get("delivery_id").and_then(|value| value.as_str()) != Some(request_delivery_id)
    {
        return None;
    }
    let sender_agent_id = header_label_value(sender_agent_id);
    let waiting_task_id = header_label_value(waiting_task_id);
    let request_delivery_id = header_label_value(request_delivery_id);
    Some(format!(
        "Runtime message contract:\n- This is an agent request; the sender is waiting for a reply.\n- Reply expected from this agent: yes. Use SendAgentMessage to reply to {sender_agent_id} when you have a response.\n- Waiting task: {waiting_task_id}\n- Request delivery: {request_delivery_id}\n- The current compatibility completion policy accepts the first later durable message from the target agent."
    ))
}

pub(super) fn header_label_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | ':' | '/' => ch,
            _ => '_',
        })
        .collect()
}

pub(super) fn kind_label(message: &MessageEnvelope) -> String {
    format!("{:?}", message.kind)
}

pub(super) fn origin_label(origin: &MessageOrigin) -> &'static str {
    match origin {
        MessageOrigin::Operator { .. } => "operator",
        MessageOrigin::Channel { .. } => "channel",
        MessageOrigin::Webhook { .. } => "webhook",
        MessageOrigin::Callback { .. } => "callback",
        MessageOrigin::Timer { .. } => "timer",
        MessageOrigin::System { .. } => "system",
        MessageOrigin::Task { .. } => "task",
    }
}

#[cfg(test)]
pub(super) fn trust_label(authority_class: &AuthorityClass) -> &'static str {
    match authority_class {
        AuthorityClass::OperatorInstruction => "trusted_operator",
        AuthorityClass::RuntimeInstruction => "trusted_system",
        AuthorityClass::IntegrationSignal => "trusted_integration",
        AuthorityClass::ExternalEvidence => "untrusted_external",
    }
}

pub(super) fn authority_class_label(authority_class: AuthorityClass) -> &'static str {
    match authority_class {
        AuthorityClass::OperatorInstruction => "operator_instruction",
        AuthorityClass::RuntimeInstruction => "runtime_instruction",
        AuthorityClass::IntegrationSignal => "integration_signal",
        AuthorityClass::ExternalEvidence => "external_evidence",
    }
}

pub(super) fn delivery_surface_label(surface: MessageDeliverySurface) -> &'static str {
    match surface {
        MessageDeliverySurface::CliPrompt => "cli_prompt",
        MessageDeliverySurface::RunOnce => "run_once",
        MessageDeliverySurface::HttpPublicEnqueue => "http_public_enqueue",
        MessageDeliverySurface::HttpWebhook => "http_webhook",
        MessageDeliverySurface::HttpCallbackEnqueue => "http_callback_enqueue",
        MessageDeliverySurface::HttpCallbackWake => "http_callback_wake",
        MessageDeliverySurface::HttpControlPrompt => "http_control_prompt",
        MessageDeliverySurface::RemoteOperatorTransport => "remote_operator_transport",
        MessageDeliverySurface::TimerScheduler => "timer_scheduler",
        MessageDeliverySurface::RuntimeSystem => "runtime_system",
        MessageDeliverySurface::TaskRejoin => "task_rejoin",
    }
}

pub(super) fn admission_context_label(context: AdmissionContext) -> &'static str {
    match context {
        AdmissionContext::PublicUnauthenticated => "public_unauthenticated",
        AdmissionContext::ControlAuthenticated => "control_authenticated",
        AdmissionContext::OperatorTransportAuthenticated => "operator_transport_authenticated",
        AdmissionContext::ExternalTriggerCapability => "external_trigger_capability",
        AdmissionContext::LocalProcess => "local_process",
        AdmissionContext::RuntimeOwned => "runtime_owned",
    }
}

pub(super) fn enum_label<T: serde::Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reply_expectation_context_requires_runtime_owned_consistent_metadata() {
        let mut message = MessageEnvelope::new(
            "target",
            crate::types::MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: "task-1".into(),
            },
            AuthorityClass::RuntimeInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "request".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.correlation_id = Some("task-1".into());
        message.metadata = Some(json!({
            "agent_message_delivery": {
                "delivery_id": "delivery-1",
                "caller_agent_id": "sender"
            },
            "agent_message_reply_expectation": {
                "mode": "required",
                "sender_agent_id": "sender",
                "waiting_task_id": "task-1",
                "request_message_id": message.id,
                "request_delivery_id": "delivery-1"
            }
        }));

        let rendered = message_reply_expectation_context(&message).unwrap();
        assert!(rendered.contains("sender"));
        assert!(rendered.contains("sender is waiting for a reply"));
        let current_input = super::super::render_current_input_section(&message, 256, None);
        assert!(current_input.content.contains(&rendered));
        assert!(current_input.content.contains("\n  request"));

        message.correlation_id = Some("other-task".into());
        assert!(message_reply_expectation_context(&message).is_none());
        assert!(
            !super::super::render_current_input_section(&message, 256, None)
                .content
                .contains("sender is waiting for a reply")
        );
    }

    #[test]
    fn ordinary_messages_do_not_render_reply_expectation_context() {
        let message = MessageEnvelope::new(
            "target",
            crate::types::MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            AuthorityClass::RuntimeInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "notification".into(),
            },
        );

        assert!(message_reply_expectation_context(&message).is_none());
    }
}
