use crate::types::{AuthorityClass, MessageKind, MessageOrigin};

#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: String,
}

impl PolicyDecision {
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: reason.into(),
        }
    }
}

pub fn validate_message_kind_for_origin(
    kind: &MessageKind,
    origin: &MessageOrigin,
) -> PolicyDecision {
    match (kind, origin) {
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. }) => {
            PolicyDecision::allow("operator prompt allowed")
        }
        (MessageKind::WebhookEvent, MessageOrigin::Webhook { .. }) => {
            PolicyDecision::allow("webhook event allowed")
        }
        (MessageKind::CallbackEvent, MessageOrigin::Callback { .. }) => {
            PolicyDecision::allow("callback event allowed")
        }
        (MessageKind::ChannelEvent, MessageOrigin::Channel { .. }) => {
            PolicyDecision::allow("channel event allowed")
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { .. }) => {
            PolicyDecision::allow("timer tick allowed")
        }
        (MessageKind::SystemTick | MessageKind::InternalFollowup, MessageOrigin::System { .. }) => {
            PolicyDecision::allow("system event allowed")
        }
        (MessageKind::TaskStatus | MessageKind::TaskResult, MessageOrigin::Task { .. }) => {
            PolicyDecision::allow("task event allowed")
        }
        (MessageKind::Control, MessageOrigin::Operator { .. })
        | (MessageKind::Control, MessageOrigin::System { .. }) => {
            PolicyDecision::allow("control event allowed")
        }
        _ => PolicyDecision::deny("message kind does not match origin"),
    }
}

pub fn default_authority_for_origin(origin: &MessageOrigin) -> AuthorityClass {
    match origin {
        MessageOrigin::Operator { .. } => AuthorityClass::OperatorInstruction,
        MessageOrigin::System { .. } | MessageOrigin::Task { .. } | MessageOrigin::Timer { .. } => {
            AuthorityClass::RuntimeInstruction
        }
        MessageOrigin::Webhook { .. } => AuthorityClass::IntegrationSignal,
        MessageOrigin::Callback { .. } => AuthorityClass::IntegrationSignal,
        MessageOrigin::Channel { .. } => AuthorityClass::ExternalEvidence,
    }
}

#[cfg(test)]
mod tests {
    use crate::types::MessageOrigin;

    use super::*;

    #[test]
    fn mismatched_origin_is_denied() {
        let decision = validate_message_kind_for_origin(
            &MessageKind::WebhookEvent,
            &MessageOrigin::Operator { actor_id: None },
        );
        assert!(!decision.allowed);
    }
}
