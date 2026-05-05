use std::collections::VecDeque;

use crate::types::{MessageEnvelope, MessageKind, MessageOrigin, Priority, TrustLevel};

#[derive(Debug, Default, Clone)]
pub struct RuntimeQueue {
    interrupt: VecDeque<MessageEnvelope>,
    next: VecDeque<MessageEnvelope>,
    normal: VecDeque<MessageEnvelope>,
    background: VecDeque<MessageEnvelope>,
}

impl RuntimeQueue {
    pub fn push(&mut self, message: MessageEnvelope) {
        match message.priority {
            Priority::Interrupt => self.interrupt.push_back(message),
            Priority::Next => self.next.push_back(message),
            Priority::Normal => self.normal.push_back(message),
            Priority::Background => self.background.push_back(message),
        }
    }

    pub fn push_front(&mut self, message: MessageEnvelope) {
        match message.priority {
            Priority::Interrupt => self.interrupt.push_front(message),
            Priority::Next => self.next.push_front(message),
            Priority::Normal => self.normal.push_front(message),
            Priority::Background => self.background.push_front(message),
        }
    }

    pub fn pop(&mut self) -> Option<MessageEnvelope> {
        self.interrupt
            .pop_front()
            .or_else(|| self.next.pop_front())
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.background.pop_front())
    }

    pub fn pop_interrupt_operator_prompt(&mut self) -> Option<MessageEnvelope> {
        let position = self.interrupt.iter().position(|message| {
            matches!(
                (
                    &message.kind,
                    &message.origin,
                    &message.trust,
                    &message.priority,
                ),
                (
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator { .. },
                    TrustLevel::TrustedOperator,
                    Priority::Interrupt,
                )
            )
        })?;
        self.interrupt.remove(position)
    }

    pub fn len(&self) -> usize {
        self.interrupt.len() + self.next.len() + self.normal.len() + self.background.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{MessageBody, MessageEnvelope, MessageKind, MessageOrigin, TrustLevel};

    use super::*;

    fn msg(priority: Priority, text: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            TrustLevel::TrustedIntegration,
            priority,
            MessageBody::Text { text: text.into() },
        )
    }

    fn operator_msg(priority: Priority, text: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("test".into()),
            },
            TrustLevel::TrustedOperator,
            priority,
            MessageBody::Text { text: text.into() },
        )
    }

    #[test]
    fn queue_respects_priority_and_fifo() {
        let mut queue = RuntimeQueue::default();
        queue.push(msg(Priority::Normal, "n1"));
        queue.push(msg(Priority::Interrupt, "i1"));
        queue.push(msg(Priority::Normal, "n2"));
        queue.push(msg(Priority::Next, "x1"));

        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text { text: "i1".into() }
        );
        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text { text: "x1".into() }
        );
        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text { text: "n1".into() }
        );
        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text { text: "n2".into() }
        );
    }

    #[test]
    fn pop_interrupt_operator_prompt_only_drains_trusted_operator_interrupts() {
        let mut queue = RuntimeQueue::default();
        queue.push(msg(Priority::Interrupt, "webhook"));
        queue.push(operator_msg(Priority::Normal, "normal-operator"));
        queue.push(operator_msg(Priority::Interrupt, "interrupt-operator"));

        assert_eq!(
            queue.pop_interrupt_operator_prompt().unwrap().body,
            MessageBody::Text {
                text: "interrupt-operator".into()
            }
        );
        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text {
                text: "webhook".into()
            }
        );
        assert_eq!(
            queue.pop().unwrap().body,
            MessageBody::Text {
                text: "normal-operator".into()
            }
        );
    }
}
