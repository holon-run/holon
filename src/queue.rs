use std::{collections::VecDeque, sync::Arc};

use crate::types::{MessageEnvelope, Priority};

#[derive(Debug, Default, Clone)]
pub struct RuntimeQueue {
    interject: VecDeque<Arc<MessageEnvelope>>,
    next: VecDeque<Arc<MessageEnvelope>>,
    normal: VecDeque<Arc<MessageEnvelope>>,
    background: VecDeque<Arc<MessageEnvelope>>,
}

impl RuntimeQueue {
    pub(crate) fn shared_messages(&self) -> impl Iterator<Item = &Arc<MessageEnvelope>> {
        self.interject
            .iter()
            .chain(&self.next)
            .chain(&self.normal)
            .chain(&self.background)
    }

    pub fn push(&mut self, message: MessageEnvelope) {
        let message = Arc::new(message);
        match message.priority {
            Priority::Interject => self.interject.push_back(message),
            Priority::Next => self.next.push_back(message),
            Priority::Normal => self.normal.push_back(message),
            Priority::Background => self.background.push_back(message),
        }
    }

    pub fn push_front(&mut self, message: MessageEnvelope) {
        let message = Arc::new(message);
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
            .map(into_owned_message)
    }

    pub fn peek(&self) -> Option<&MessageEnvelope> {
        self.interject
            .front()
            .or_else(|| self.next.front())
            .or_else(|| self.normal.front())
            .or_else(|| self.background.front())
            .map(Arc::as_ref)
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

    pub fn peek_next_matching(
        &self,
        mut predicate: impl FnMut(&MessageEnvelope) -> bool,
    ) -> Option<&MessageEnvelope> {
        self.interject
            .iter()
            .find(|message| predicate(message))
            .or_else(|| self.next.iter().find(|message| predicate(message)))
            .or_else(|| self.normal.iter().find(|message| predicate(message)))
            .or_else(|| self.background.iter().find(|message| predicate(message)))
            .map(Arc::as_ref)
    }

    pub fn pop_selected(
        &mut self,
        message_id: &str,
        allow_non_head: bool,
    ) -> Option<MessageEnvelope> {
        if !allow_non_head {
            return self.pop_if_next(message_id);
        }
        self.pop_next_matching(|message| message.id == message_id)
    }

    pub fn len(&self) -> usize {
        self.interject.len() + self.next.len() + self.normal.len() + self.background.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.interject.clear();
        self.next.clear();
        self.normal.clear();
        self.background.clear();
    }
}

fn pop_matching_from(
    queue: &mut VecDeque<Arc<MessageEnvelope>>,
    predicate: &mut impl FnMut(&MessageEnvelope) -> bool,
) -> Option<MessageEnvelope> {
    let position = queue.iter().position(|message| predicate(message))?;
    queue.remove(position).map(into_owned_message)
}

fn into_owned_message(message: Arc<MessageEnvelope>) -> MessageEnvelope {
    Arc::try_unwrap(message).unwrap_or_else(|message| message.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use crate::types::{AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin};

    use super::*;

    fn msg(priority: Priority, text: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            AuthorityClass::IntegrationSignal,
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
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            priority,
            MessageBody::Text { text: text.into() },
        )
    }

    #[test]
    fn candidate_queue_shares_pending_bodies_and_keeps_private_membership() {
        let mut current = RuntimeQueue::default();
        let message = msg(Priority::Normal, &"x".repeat(16 * 1024));
        let id = message.id.clone();
        current.push(message);
        let mut candidate = current.clone();
        assert!(Arc::ptr_eq(
            current.normal.front().unwrap(),
            candidate.normal.front().unwrap()
        ));
        let old = Arc::downgrade(current.normal.front().unwrap());
        assert_eq!(candidate.pop().unwrap().id, id);
        assert!(candidate.is_empty());
        assert_eq!(current.len(), 1);
        drop(candidate);
        assert!(old.upgrade().is_some());
        drop(current);
        assert!(old.upgrade().is_none());
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
                        (&message.kind, &message.origin, &message.authority_class),
                        (
                            MessageKind::OperatorPrompt,
                            MessageOrigin::Operator { .. },
                            AuthorityClass::OperatorInstruction,
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
