use super::*;
use anyhow::Result;

fn stream_event_envelope(
    agent_id: &str,
    event_log_epoch: &str,
    event: &AuditEvent,
    emit_projection_effect: bool,
) -> StreamEventEnvelope {
    let legacy = holon_domain::runtime_event::is_legacy_event_shape(
        &event.payload_schema,
        event.contract_version,
    );
    StreamEventEnvelope {
        id: event.id.clone(),
        event_seq: event.event_seq,
        event_log_epoch: if event.event_log_epoch.is_empty() {
            event_log_epoch.to_string()
        } else {
            event.event_log_epoch.clone()
        },
        ts: event.created_at,
        agent_id: agent_id.to_string(),
        event_type: event.kind.clone(),
        // Schema-less legacy events omit constant envelope metadata.
        payload_schema: (!legacy).then(|| event.payload_schema.clone()),
        payload_schema_version: (!legacy).then_some(event.payload_schema_version),
        payload: public_event_payload(&event.data),
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

/// Keep provider diagnostics available in the durable audit record while
/// keeping them out of the default public event contract.
fn public_event_payload(payload: &Value) -> Value {
    let Value::Object(object) = payload else {
        return payload.clone();
    };
    let mut public = object.clone();
    for key in [
        "prompt_cache_key",
        "context_fingerprint",
        "compression_epoch",
        "provider_request_id",
        "provider_message_id",
        "provider_request_diagnostics",
        "provider_attempt_timeline",
        "only_sleep_tools",
    ] {
        public.remove(key);
    }
    Value::Object(public)
}

#[test]
fn borrowed_event_wire_matches_original_schema_and_top_level_privacy_without_body_copy(
) -> Result<()> {
    for payload in [
        serde_json::json!({"visible": {"provider_request_id": "nested"}, "provider_request_id": "private", "only_sleep_tools": true}),
        serde_json::json!(["opaque", {"provider_request_id": "nested"}]),
        serde_json::json!("opaque"),
    ] {
        let mut event = AuditEvent::legacy("future-unknown", payload);
        for typed in [false, true] {
            if typed {
                event.payload_schema = "holon.future.v1".to_owned();
                event.payload_schema_version = 1;
                event.contract_version =
                    holon_domain::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION;
                event.event_log_epoch = "original-epoch".to_owned();
            }
            for emit in [false, true] {
                let borrowed = borrowed_event_envelope("agent-a", "fallback-epoch", &event, emit);
                let original = stream_event_envelope("agent-a", "fallback-epoch", &event, emit);
                assert_eq!(
                    serde_json::to_value(borrowed)?,
                    serde_json::to_value(original)?
                );
            }
        }
    }
    Ok(())
}
