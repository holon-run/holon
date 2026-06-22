use crate::tool::helpers::truncate_text;
use crate::types::AdmissionContext;
use crate::types::AuthorityClass;
use crate::types::MessageBody;
use crate::types::MessageDeliverySurface;
use crate::types::MessageEnvelope;
use crate::types::MessageOrigin;
use crate::types::SkillActivationSource;
use crate::types::SkillActivationState;
use crate::types::SkillScope;

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

pub(super) fn header_label_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | ':' | '/' => ch,
            _ => '_',
        })
        .collect()
}

pub(super) fn render_message(message: &MessageEnvelope) -> String {
    format!(
        "- {} {}",
        message_header(message),
        body_preview(&message.body)
    )
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

pub(super) fn scope_label(scope: &SkillScope) -> &'static str {
    match scope {
        SkillScope::User => "user",
        SkillScope::Agent => "agent",
        SkillScope::Workspace => "workspace",
    }
}

pub(super) fn activation_source_label(source: SkillActivationSource) -> &'static str {
    match source {
        SkillActivationSource::Explicit => "explicit",
        SkillActivationSource::ImplicitFromCatalog => "implicit_from_catalog",
        SkillActivationSource::Restored => "restored",
        SkillActivationSource::Inherited => "inherited",
    }
}

pub(super) fn activation_state_label(state: SkillActivationState) -> &'static str {
    match state {
        SkillActivationState::TurnActive => "turn_active",
        SkillActivationState::SessionActive => "session_active",
    }
}

pub(super) fn body_preview(body: &MessageBody) -> String {
    let text = message_body_text(body);
    if text.chars().count() <= 160 {
        text
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}

pub(super) fn message_body_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

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

pub(super) fn bounded_inline(value: &str, max_chars: usize) -> String {
    truncate_text(&sanitize_inline(value), max_chars)
}

pub(super) fn indent_block(text: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn enum_label<T: serde::Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{value:?}"))
}
