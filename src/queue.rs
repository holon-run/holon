use std::collections::VecDeque;

use crate::types::{MessageEnvelope, Priority};

#[derive(Debug, Default, Clone)]
pub struct RuntimeQueue {
    interject: VecDeque<MessageEnvelope>,
    next: VecDeque<MessageEnvelope>,
    normal: VecDeque<MessageEnvelope>,
    background: VecDeque<MessageEnvelope>,
}

impl RuntimeQueue {
    pub fn push(&mut self, message: MessageEnvelope) {
        match message.priority {
            Priority::Interject => self.interject.push_back(message),
            Priority::Next => self.next.push_back(message),
            Priority::Normal => self.normal.push_back(message),
            Priority::Background => self.background.push_back(message),
        }
    }

    pub fn push_front(&mut self, message: MessageEnvelope) {
        match message.priority {
            Priority::Interject => self.interject.push_front(message),
            Priority::Next => self.next.push_front(message),
            Priority::Normal => self.normal.push_front(message),
            Priority::Background => self.background.push_front(message),
        }
    }

    pub fn pop(&mut self) -> Option<MessageEnvelope> {
        self.interject
            .pop_front()
            .or_else(|| self.next.pop_front())
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.background.pop_front())
    }

    pub fn peek(&self) -> Option<&MessageEnvelope> {
        self.interject
            .front()
            .or_else(|| self.next.front())
            .or_else(|| self.normal.front())
            .or_else(|| self.background.front())
    }

    pub fn pop_if_next(&mut self, message_id: &str) -> Option<MessageEnvelope> {
        if self.peek().is_some_and(|message| message.id == message_id) {
            self.pop()
        } else {
            None
        }
    }

    pub fn pop_next_matching(
        &mut self,
        mut predicate: impl FnMut(&MessageEnvelope) -> bool,
    ) -> Option<MessageEnvelope> {
        pop_matching_from(&mut self.interject, &mut predicate)
            .or_else(|| pop_matching_from(&mut self.next, &mut predicate))
            .or_else(|| pop_matching_from(&mut self.normal, &mut predicate))
            .or_else(|| pop_matching_from(&mut self.background, &mut predicate))
    }

    pub fn len(&self) -> usize {
        self.interject.len() + self.next.len() + self.normal.len() + self.background.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn pop_matching_from(
    queue: &mut VecDeque<MessageEnvelope>,
    predicate: &mut impl FnMut(&MessageEnvelope) -> bool,
) -> Option<MessageEnvelope> {
    let position = queue.iter().position(predicate)?;
    queue.remove(position)
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
        queue.push(msg(Priority::Interject, "i1"));
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
    fn peek_and_pop_if_next_use_priority_head() {
        let mut queue = RuntimeQueue::default();
        let normal = msg(Priority::Normal, "normal");
        let interject = msg(Priority::Interject, "interject");
        let normal_id = normal.id.clone();
        let interject_id = interject.id.clone();
        queue.push(normal);
        queue.push(interject);

        assert_eq!(queue.peek().unwrap().id, interject_id);
        assert!(queue.pop_if_next(&normal_id).is_none());
        assert_eq!(queue.pop_if_next(&interject_id).unwrap().id, interject_id);
        assert_eq!(queue.pop_if_next(&normal_id).unwrap().id, normal_id);
    }

    #[test]
    fn pop_next_matching_uses_priority_order() {
        let mut queue = RuntimeQueue::default();
        queue.push(msg(Priority::Interject, "webhook"));
        queue.push(operator_msg(Priority::Normal, "normal-operator"));
        queue.push(operator_msg(Priority::Interject, "interject-operator"));

        assert_eq!(
            queue
                .pop_next_matching(|message| {
                    matches!(
                        (&message.kind, &message.origin, &message.trust),
                        (
                            MessageKind::OperatorPrompt,
                            MessageOrigin::Operator { .. },
                            TrustLevel::TrustedOperator,
                        )
                    )
                })
                .unwrap()
                .body,
            MessageBody::Text {
                text: "interject-operator".into()
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
