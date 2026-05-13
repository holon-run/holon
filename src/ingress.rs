use serde::{Deserialize, Serialize};

use crate::types::{
    AdmissionContext, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, Priority, TrustLevel,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboundRequest {
    pub agent_id: String,
    pub kind: MessageKind,
    pub priority: Priority,
    pub origin: MessageOrigin,
    pub trust: TrustLevel,
    pub body: MessageBody,
    pub delivery_surface: MessageDeliverySurface,
    pub admission_context: AdmissionContext,
    pub metadata: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

impl InboundRequest {
    pub fn into_message(self) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            self.agent_id,
            self.kind,
            self.origin,
            self.trust,
            self.priority,
            self.body,
        )
        .with_admission(self.delivery_surface, self.admission_context);
        message.metadata = self.metadata;
        message.correlation_id = self.correlation_id;
        message.causation_id = self.causation_id;
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
    pub waiting_intent_id: Option<String>,
    pub external_trigger_id: Option<String>,
    pub resource: Option<String>,
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}
