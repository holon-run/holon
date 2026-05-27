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
    fn default_authority_matches_current_origin_contract() {
        let cases = [
            (
                MessageOrigin::Operator {
                    actor_id: Some("operator".into()),
                },
                AuthorityClass::OperatorInstruction,
            ),
            (
                MessageOrigin::System {
                    subsystem: "scheduler".into(),
                },
                AuthorityClass::RuntimeInstruction,
            ),
            (
                MessageOrigin::Task {
                    task_id: "task-1".into(),
                },
                AuthorityClass::RuntimeInstruction,
            ),
            (
                MessageOrigin::Timer {
                    timer_id: "timer-1".into(),
                },
                AuthorityClass::RuntimeInstruction,
            ),
            (
                MessageOrigin::Webhook {
                    source: "github".into(),
                    event_type: Some("push".into()),
                },
                AuthorityClass::IntegrationSignal,
            ),
            (
                MessageOrigin::Callback {
                    descriptor_id: "trigger-1".into(),
                    source: Some("agentinbox".into()),
                },
                AuthorityClass::IntegrationSignal,
            ),
            (
                MessageOrigin::Channel {
                    channel_id: "chat".into(),
                    sender_id: Some("user".into()),
                },
                AuthorityClass::ExternalEvidence,
            ),
        ];

        for (origin, authority) in cases {
            assert_eq!(
                default_authority_for_origin(&origin),
                authority,
                "{origin:?}"
            );
        }
    }

    #[test]
    fn origin_kind_matrix_matches_current_provenance_contract() {
        let allowed_cases = [
            (
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
            ),
            (
                MessageKind::WebhookEvent,
                MessageOrigin::Webhook {
                    source: "http".into(),
                    event_type: None,
                },
            ),
            (
                MessageKind::CallbackEvent,
                MessageOrigin::Callback {
                    descriptor_id: "trigger-1".into(),
                    source: None,
                },
            ),
            (
                MessageKind::ChannelEvent,
                MessageOrigin::Channel {
                    channel_id: "chat".into(),
                    sender_id: None,
                },
            ),
            (
                MessageKind::TimerTick,
                MessageOrigin::Timer {
                    timer_id: "timer-1".into(),
                },
            ),
            (
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "scheduler".into(),
                },
            ),
            (
                MessageKind::InternalFollowup,
                MessageOrigin::System {
                    subsystem: "runtime".into(),
                },
            ),
            (
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: "task-1".into(),
                },
            ),
            (
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: "task-1".into(),
                },
            ),
            (
                MessageKind::Control,
                MessageOrigin::Operator { actor_id: None },
            ),
            (
                MessageKind::Control,
                MessageOrigin::System {
                    subsystem: "runtime".into(),
                },
            ),
        ];

        for (kind, origin) in allowed_cases {
            let decision = validate_message_kind_for_origin(&kind, &origin);
            assert!(
                decision.allowed,
                "expected {kind:?} from {origin:?} to be allowed: {}",
                decision.reason
            );
        }
    }

    #[test]
    fn mismatched_origin_is_denied() {
        let decision = validate_message_kind_for_origin(
            &MessageKind::WebhookEvent,
            &MessageOrigin::Operator { actor_id: None },
        );
        assert!(!decision.allowed);
    }
}
