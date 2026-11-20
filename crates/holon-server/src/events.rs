use chrono::Utc;
use holon_domain::AuditEvent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventPageOrder {
    Asc,
    Desc,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EventsPageResponse {
    pub events: Vec<StreamEventEnvelope>,
    pub event_log_epoch: String,
    /// Stream-level envelope contract version for every event in this page.
    /// Per-event `contract_version` was removed; this is its single source.
    pub contract_version: u32,
    pub oldest_seq: Option<u64>,
    pub newest_seq: Option<u64>,
    /// Exclusive descending continuation after the last scanned candidate, if more remain.
    pub next_before_seq: Option<u64>,
    /// Exclusive ascending continuation after the last scanned candidate, if more remain.
    pub next_after_seq: Option<u64>,
    pub cursor_seq: Option<u64>,
    pub has_older: bool,
    pub has_newer: bool,
    pub order: EventPageOrder,
    pub limit: usize,
}

/// One event on the public event surface. The `payload` object is the only
/// data source: correlation ids such as `message_id`, `task_id`,
/// `work_item_id`, `correlation_id`, and `causation_id` live in the payload
/// itself and are no longer duplicated into an envelope `provenance` object.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StreamEventEnvelope {
    pub id: String,
    pub event_seq: u64,
    pub event_log_epoch: String,
    pub ts: chrono::DateTime<Utc>,
    pub agent_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    /// Registry payload schema. Present only on typed events; legacy audit
    /// events are schema-less and carry their data as-is. The envelope
    /// contract version is declared per stream, not per event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_schema_version: Option<u32>,
    pub payload: Value,
    /// Additive classification derived from the runtime event registry.
    /// Present only while `events.projection-effect.v1` is advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_effect: Option<holon_domain::runtime_event::ProjectionEffect>,
}

#[derive(Serialize)]
pub struct BorrowedEventEnvelope<'a> {
    id: &'a str,
    event_seq: u64,
    event_log_epoch: &'a str,
    ts: chrono::DateTime<Utc>,
    agent_id: &'a str,
    #[serde(rename = "type")]
    event_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_schema: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_schema_version: Option<u32>,
    payload: PublicEventPayload<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projection_effect: Option<holon_domain::runtime_event::ProjectionEffect>,
}

pub fn borrowed_event_envelope<'a>(
    agent_id: &'a str,
    event_log_epoch: &'a str,
    event: &'a AuditEvent,
    emit_projection_effect: bool,
) -> BorrowedEventEnvelope<'a> {
    let legacy = holon_domain::runtime_event::is_legacy_event_shape(
        &event.payload_schema,
        event.contract_version,
    );
    BorrowedEventEnvelope {
        id: &event.id,
        event_seq: event.event_seq,
        event_log_epoch: if event.event_log_epoch.is_empty() {
            event_log_epoch
        } else {
            &event.event_log_epoch
        },
        ts: event.created_at,
        agent_id,
        event_type: &event.kind,
        payload_schema: (!legacy).then_some(event.payload_schema.as_str()),
        payload_schema_version: (!legacy).then_some(event.payload_schema_version),
        payload: PublicEventPayload(&event.data),
        projection_effect: emit_projection_effect.then(|| {
            holon_domain::runtime_event::projection_effect_of(
                &event.kind,
                event.contract_version,
                &event.payload_schema,
                event.payload_schema_version,
            )
        }),
    }
}

struct PublicEventPayload<'a>(&'a Value);

impl Serialize for PublicEventPayload<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let Value::Object(object) = self.0 else {
            return self.0.serialize(serializer);
        };
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in object {
            if !PRIVATE_EVENT_PAYLOAD_KEYS.contains(&key.as_str()) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

const PRIVATE_EVENT_PAYLOAD_KEYS: &[&str] = &[
    "prompt_cache_key",
    "context_fingerprint",
    "compression_epoch",
    "provider_request_id",
    "provider_message_id",
    "provider_request_diagnostics",
    "provider_attempt_timeline",
    "only_sleep_tools",
];

#[cfg(test)]
mod tests;
