use serde::{Deserialize, Serialize};

use crate::observability::TraceContext;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
    MessageKind, MessageOrigin, Priority,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboundRequest {
    pub agent_id: String,
    pub kind: MessageKind,
    pub priority: Priority,
    pub origin: MessageOrigin,
    pub authority_class: AuthorityClass,
    pub body: MessageBody,
    pub delivery_surface: MessageDeliverySurface,
    pub admission_context: AdmissionContext,
    pub work_item_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub trace_context: Option<TraceContext>,
}

impl InboundRequest {
    pub fn into_message(self) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            self.agent_id,
            self.kind,
            self.origin,
            self.authority_class,
            self.priority,
            self.body,
        )
        .with_admission(self.delivery_surface, self.admission_context);
        message.work_item_id = self.work_item_id;
        message.metadata = self.metadata;
        message.correlation_id = self.correlation_id;
        message.causation_id = self.causation_id;
        message.trace_context = self.trace_context;
        message
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WakeDisposition {
    Triggered,
    Coalesced,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WakeHint {
    pub agent_id: String,
    pub reason: String,
    pub description: Option<String>,
    pub source: Option<String>,
    pub scope: Option<crate::types::ExternalTriggerScope>,
    pub external_trigger_id: Option<String>,
    pub resource: Option<String>,
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_request_preserves_exact_routing_and_provenance_facts() {
        let trace_context = TraceContext {
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            span_id: "0123456789abcdef".into(),
            trace_flags: 1,
            trace_state: Some("vendor=value".into()),
        };
        let request = InboundRequest {
            agent_id: "agent-alpha".into(),
            kind: MessageKind::WebhookEvent,
            priority: Priority::Next,
            origin: MessageOrigin::Webhook {
                source: "github".into(),
                event_type: Some("pull_request".into()),
            },
            authority_class: AuthorityClass::IntegrationSignal,
            body: MessageBody::Text {
                text: "updated".into(),
            },
            delivery_surface: MessageDeliverySurface::HttpWebhook,
            admission_context: AdmissionContext::ExternalTriggerCapability,
            work_item_id: Some("work-123".into()),
            metadata: Some(serde_json::json!({"delivery": "abc"})),
            correlation_id: Some("correlation-1".into()),
            causation_id: Some("causation-1".into()),
            trace_context: Some(trace_context.clone()),
        };

        let message = request.into_message();

        assert_eq!(message.agent_id, "agent-alpha");
        assert_eq!(message.work_item_id.as_deref(), Some("work-123"));
        assert_eq!(message.kind, MessageKind::WebhookEvent);
        assert_eq!(
            message.origin,
            MessageOrigin::Webhook {
                source: "github".into(),
                event_type: Some("pull_request".into()),
            }
        );
        assert_eq!(message.authority_class, AuthorityClass::IntegrationSignal);
        assert_eq!(message.priority, Priority::Next);
        assert_eq!(
            message.delivery_surface,
            Some(MessageDeliverySurface::HttpWebhook)
        );
        assert_eq!(
            message.admission_context,
            Some(AdmissionContext::ExternalTriggerCapability)
        );
        assert_eq!(
            message.metadata,
            Some(serde_json::json!({"delivery": "abc"}))
        );
        assert_eq!(message.correlation_id.as_deref(), Some("correlation-1"));
        assert_eq!(message.causation_id.as_deref(), Some("causation-1"));
        assert_eq!(message.trace_context, Some(trace_context));
    }
}
