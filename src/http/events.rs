use super::*;

pub async fn events(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_STREAM_WINDOW)
        .clamp(1, MAX_EVENT_STREAM_WINDOW);
    let order = query.order.unwrap_or(EventPageOrder::Desc);
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let emit_projection_effect = projection_effect_emission_enabled(&state);
    let max_level = query.max_level;
    let event_kind = query.event_kind.as_deref();
    let page = state
        .host
        .operator_event_page(
            &agent_id,
            query.before_seq,
            query.after_seq,
            limit,
            order.into(),
            |storage| {
                let filter_context = max_level
                    .map(|_| event_filter_context(storage))
                    .transpose()?;
                Ok(move |event: &AuditEvent| {
                    let level_matches = match (max_level, filter_context.as_ref()) {
                        (Some(level), Some(filter_context)) => is_operator_event_in_display_mode(
                            &event.kind,
                            &event.data,
                            &event_fallback_summary(event),
                            filter_context,
                            level,
                        ),
                        _ => true,
                    };
                    level_matches && event_kind.is_none_or(|kind| event.kind == kind)
                })
            },
        )
        .await
        .map_err(agent_access_error)?;
    let first = page.events().next().map(|event| event.event_seq);
    let last = page.events().next_back().map(|event| event.event_seq);
    let (oldest_seq, newest_seq) = match order {
        EventPageOrder::Asc => (first, last),
        EventPageOrder::Desc => (last, first),
    };
    let response = BorrowedEventsPage {
        events: BorrowedEventEnvelopes {
            page: &page,
            agent_id: &agent_id,
            emit_projection_effect,
        },
        event_log_epoch: &page.recovery.event_log_epoch,
        contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
        oldest_seq,
        newest_seq,
        cursor_seq: page.cursor_seq,
        has_older: page.has_older,
        has_newer: page.has_newer,
        order,
        limit,
    };
    let bytes = page.json_bytes(&response).map_err(error_response)?;
    Ok((
        [
            (
                axum::http::HeaderName::from_static(
                    crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
                ),
                crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
            ),
            (
                axum::http::header::CONTENT_TYPE,
                "application/json".to_owned(),
            ),
        ],
        bytes,
    ))
}

#[derive(Serialize)]
struct BorrowedEventsPage<'a> {
    events: BorrowedEventEnvelopes<'a>,
    event_log_epoch: &'a str,
    contract_version: u32,
    oldest_seq: Option<u64>,
    newest_seq: Option<u64>,
    cursor_seq: Option<u64>,
    has_older: bool,
    has_newer: bool,
    order: EventPageOrder,
    limit: usize,
}

struct BorrowedEventEnvelopes<'a> {
    page: &'a crate::host::OwnedEventPageRead,
    agent_id: &'a str,
    emit_projection_effect: bool,
}

impl Serialize for BorrowedEventEnvelopes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.page.events().len()))?;
        for event in self.page.events() {
            sequence.serialize_element(&borrowed_event_envelope(
                self.agent_id,
                &self.page.recovery.event_log_epoch,
                event,
                self.emit_projection_effect,
            ))?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct BorrowedEventEnvelope<'a> {
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
    projection_effect: Option<crate::runtime_event::ProjectionEffect>,
}

fn borrowed_event_envelope<'a>(
    agent_id: &'a str,
    event_log_epoch: &'a str,
    event: &'a AuditEvent,
    emit_projection_effect: bool,
) -> BorrowedEventEnvelope<'a> {
    let legacy =
        crate::runtime_event::is_legacy_event_shape(&event.payload_schema, event.contract_version);
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
            crate::runtime_event::projection_effect_of(
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

pub async fn message(
    Path((agent_id, message_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let Some(message) = storage
        .read_message_by_id(&message_id)
        .map_err(error_response)?
    else {
        return Err(not_found(format!("message {message_id} not found")));
    };
    if message.agent_id != agent_id {
        return Err(not_found(format!("message {message_id} not found")));
    }
    Ok(Json(message))
}

pub async fn messages_batch_get(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<BatchGetMessagesRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let mut messages = Vec::new();
    let mut missing_message_ids = Vec::new();
    for message_id in request.message_ids {
        match storage
            .read_message_by_id(&message_id)
            .map_err(error_response)?
        {
            Some(message) if message.agent_id == agent_id => messages.push(message),
            _ => missing_message_ids.push(message_id),
        }
    }
    Ok(Json(BatchGetMessagesResponse {
        messages,
        missing_message_ids,
    }))
}

pub async fn events_stream(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventStreamQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let event_window_limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_STREAM_WINDOW)
        .clamp(1, MAX_EVENT_STREAM_WINDOW);
    let after_seq = query.after_seq;
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    // Subscribe before the committed replay cut so concurrent ACKs are either
    // replayed or received live; sequence de-duplication resolves the overlap.
    let mut live_rx = state.host.subscribe_events();
    // Capability advertisement is captured once at stream open and applies
    // to every event in the stream: the underlying verification only
    // changes on migration/restart, and a lagging stream is closed instead
    // of being resumed with different emission semantics mid-stream.
    let emit_projection_effect = projection_effect_emission_enabled(&state);
    let page = state
        .host
        .operator_event_page(
            &agent_id,
            None,
            None,
            event_window_limit.saturating_add(1),
            EventLogPageOrder::Desc,
            |_| Ok(|_: &AuditEvent| true),
        )
        .await
        .map_err(agent_access_error)?;
    let start_index = initial_buffered_events(&page, after_seq)?;
    let scope = state
        .host
        .event_stream_scope(&page)
        .map_err(error_response)?;
    let (tx, out_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::convert::Infallible>>(32);
    let runtime_id = agent_id.clone();
    tokio::spawn(async move {
        let mut last_sent_seq = after_seq.unwrap_or(0);
        for offset in start_index..page.events().len() {
            let frame = scope
                .history_frame(&page, page.events().len() - 1 - offset)
                .and_then(|read| {
                    let event = read.event();
                    let seq = event.event_seq;
                    let envelope = borrowed_event_envelope(
                        &runtime_id,
                        &page.recovery.event_log_epoch,
                        event,
                        emit_projection_effect,
                    );
                    Ok((seq, read.sse_bytes(&envelope)?))
                });
            let Ok((seq, bytes)) = frame else {
                warn!(agent_id = %runtime_id, "qualified Event replay encoding failed");
                return;
            };
            if tx.send(Ok(bytes)).await.is_err() {
                return;
            }
            last_sent_seq = last_sent_seq.max(seq);
        }
        drop(page);
        loop {
            let next = tokio::select! {
                _ = tx.closed() => break,
                next = live_rx.recv() => next,
            };
            match next {
                Ok(published) if published.agent_id.as_deref() == Some(runtime_id.as_str()) => {
                    if published.event.event_seq <= last_sent_seq {
                        continue;
                    }
                    let frame = scope.live_frame(&published).and_then(|read| {
                        let event = read.event();
                        let envelope = borrowed_event_envelope(
                            &runtime_id,
                            &event.event_log_epoch,
                            event,
                            emit_projection_effect,
                        );
                        read.sse_bytes(&envelope)
                    });
                    let bytes = match frame {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            warn!(agent_id = %runtime_id, %error, "Event notification is not a qualified stream body");
                            break;
                        }
                    };
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                    last_sent_seq = published.event.event_seq;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(agent_id = %runtime_id, skipped, "event stream receiver lagged");
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let stream = futures_util::stream::unfold(out_rx, |mut receiver| async move {
        match tokio::time::timeout(EVENT_STREAM_HEARTBEAT_INTERVAL, receiver.recv()).await {
            Ok(Some(frame)) => Some((frame, receiver)),
            Ok(None) => None,
            Err(_) => Some((Ok(Bytes::from_static(b": heartbeat\n\n")), receiver)),
        }
    });
    Ok((
        [
            (
                axum::http::HeaderName::from_static(
                    crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
                ),
                crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
            ),
            (CONTENT_TYPE, "text/event-stream".into()),
            (CACHE_CONTROL, "no-cache".into()),
        ],
        Body::from_stream(stream),
    ))
}

pub async fn global_events_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(authentication_error_response)?;
    }
    let host = state.host.clone();
    let mut rx = host.subscribe_events();
    let (tx, rx_out) = tokio::sync::mpsc::channel::<Result<Bytes, std::convert::Infallible>>(32);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(published) => {
                    let Some(agent_id) = published.agent_id.as_deref() else {
                        continue;
                    };
                    let frame = host
                        .observer_inventory_material()
                        .and_then(|material| crate::host::observer_hint_bytes(agent_id, material));
                    let Ok(bytes) = frame else {
                        warn!("global metadata hint material refused");
                        break;
                    };
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "global event stream receiver lagged");
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let stream = futures_util::stream::unfold(rx_out, |mut receiver| async move {
        match tokio::time::timeout(EVENT_STREAM_HEARTBEAT_INTERVAL, receiver.recv()).await {
            Ok(Some(frame)) => Some((frame, receiver)),
            Ok(None) => None,
            Err(_) => Some((Ok(Bytes::from_static(b": heartbeat\n\n")), receiver)),
        }
    });
    Ok((
        [
            (
                axum::http::HeaderName::from_static(
                    crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
                ),
                crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
            ),
            (CONTENT_TYPE, "text/event-stream".into()),
            (CACHE_CONTROL, "no-cache".into()),
        ],
        Body::from_stream(stream),
    ))
}

fn initial_buffered_events(
    page: &crate::host::OwnedEventPageRead,
    after_seq: Option<u64>,
) -> std::result::Result<usize, (StatusCode, Json<Value>)> {
    let start_index = if let Some(after_seq) = after_seq {
        if after_seq == 0 {
            0
        } else {
            match page
                .events()
                .rev()
                .position(|event| event.event_seq == after_seq)
            {
                Some(position) => position + 1,
                None => return Err(event_seq_not_found(after_seq, &page.recovery)),
            }
        }
    } else {
        page.events().len()
    };
    Ok(start_index)
}

#[cfg(test)]
fn stream_event_envelope(
    agent_id: &str,
    event_log_epoch: &str,
    event: &AuditEvent,
    emit_projection_effect: bool,
) -> StreamEventEnvelope {
    let legacy =
        crate::runtime_event::is_legacy_event_shape(&event.payload_schema, event.contract_version);
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
            crate::runtime_event::projection_effect_of(
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
#[cfg(test)]
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

/// Whether event pages and SSE should emit the additive `projection_effect`
/// field: only while the durable `event_projection_effect_complete`
/// verification advertises `events.projection-effect.v1`.
fn projection_effect_emission_enabled(state: &AppState) -> bool {
    let verification = load_observer_sync_verification(state);
    advertised_observer_sync_capabilities(&verification).contains(&PROJECTION_EFFECT_CAPABILITY)
}

fn event_filter_context(
    storage: &crate::storage::AppStorage,
) -> Result<OperatorPresentationContext> {
    let work_queue = storage.work_queue_read_model()?;
    let completed_work_item_ids = storage
        .latest_work_items()?
        .into_iter()
        .filter(|item| item.state == WorkItemState::Completed)
        .map(|item| item.id)
        .collect();
    Ok(OperatorPresentationContext {
        awaiting_operator_input: !work_queue.waiting_for_operator.is_empty(),
        completed_work_item_ids,
    })
}

fn event_fallback_summary(event: &AuditEvent) -> &str {
    event
        .data
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or(event.kind.as_str())
}

fn event_seq_not_found(
    after_seq: u64,
    recovery_window: &crate::runtime_db::AgentEventRecoveryWindow,
) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new(
            "cursor_not_found",
            format!("after_seq {after_seq} was not found in the replay window"),
        )
        .extension("after_seq", after_seq)
        .extension("event_seq", after_seq)
        .extension("event_log_epoch", recovery_window.event_log_epoch.clone())
        .extension("oldest_retained_seq", recovery_window.oldest_retained_seq)
        .extension("event_head_seq", recovery_window.event_head_seq),
    )
}

#[cfg(test)]
mod material_tests {
    use super::*;

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
                    event.contract_version = crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION;
                    event.event_log_epoch = "original-epoch".to_owned();
                }
                for emit in [false, true] {
                    let borrowed =
                        borrowed_event_envelope("agent-a", "fallback-epoch", &event, emit);
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
}
