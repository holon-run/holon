use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    // TODO: remove `interrupt` alias after older ledgers and request clients have migrated.
    #[serde(alias = "interrupt")]
    Interject,
    Next,
    Normal,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageOrigin {
    Operator {
        actor_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor_display_name: Option<String>,
    },
    Channel {
        channel_id: String,
        sender_id: Option<String>,
    },
    Webhook {
        source: String,
        event_type: Option<String>,
    },
    Callback {
        descriptor_id: String,
        source: Option<String>,
    },
    Timer {
        timer_id: String,
    },
    System {
        subsystem: String,
    },
    Task {
        task_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBody {
    Text {
        text: String,
    },
    Json {
        value: Value,
    },
    Brief {
        title: Option<String>,
        text: String,
        attachments: Option<Vec<BriefAttachment>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BriefAttachment {
    pub kind: String,
    pub name: String,
    pub uri: Option<String>,
    pub value: Option<Value>,
}
