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
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let emit_projection_effect = projection_effect_emission_enabled(&state);
    let cursor_seq = storage.latest_event_seq().map_err(error_response)?;
    let event_log_epoch = storage.event_log_epoch().map_err(error_response)?;
    let max_level = query.max_level;
    let event_kind = query.event_kind.as_deref();
    let filter_context = match max_level {
        Some(_) => Some(event_filter_context(&storage).map_err(error_response)?),
        None => None,
    };
    let page = storage
        .read_event_page_matching(
            query.before_seq,
            query.after_seq,
            limit,
            order.into(),
            |event| {
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
            },
        )
        .map_err(error_response)?;
    let oldest_seq = oldest_seq(&page.events, order);
    let newest_seq = newest_seq(&page.events, order);
    let events = page
        .events
        .iter()
        .map(|event| {
            stream_event_envelope(&agent_id, &event_log_epoch, event, emit_projection_effect)
        })
        .collect();
    Ok((
        [(
            axum::http::HeaderName::from_static(
                crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
            ),
            crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
        )],
        Json(EventsPageResponse {
            events,
            event_log_epoch,
            contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
            oldest_seq,
            newest_seq,
            cursor_seq,
            has_older: page.has_older,
            has_newer: page.has_newer,
            order,
            limit,
        }),
    ))
}

pub async fn message(
    Path((agent_id, message_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
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
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
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
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let mut live_rx = state.host.subscribe_events();
    let event_log_epoch = storage.event_log_epoch().map_err(error_response)?;
    // Capability advertisement is captured once at stream open and applies
    // to every event in the stream: the underlying verification only
    // changes on migration/restart, and a lagging stream is closed instead
    // of being resumed with different emission semantics mid-stream.
    let emit_projection_effect = projection_effect_emission_enabled(&state);
    let recovery_window = storage
        .agent_event_recovery_window()
        .map_err(error_response)?;
    let events = storage
        .read_recent_events(event_window_limit.saturating_add(1))
        .map_err(error_response)?;
    let buffered = initial_buffered_events(&events, after_seq, &recovery_window)?;
    let (tx, out_rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
    let runtime_id = agent_id.clone();
    tokio::spawn(async move {
        let mut last_sent_seq = after_seq.unwrap_or(0);
        for event in buffered {
            if send_stream_event(
                &tx,
                &runtime_id,
                &event_log_epoch,
                &event,
                emit_projection_effect,
            )
            .await
            .is_err()
            {
                return;
            }
            last_sent_seq = last_sent_seq.max(event.event_seq);
        }
        loop {
            match live_rx.recv().await {
                Ok(published) if published.agent_id.as_deref() == Some(runtime_id.as_str()) => {
                    if published.event.event_seq <= last_sent_seq {
                        continue;
                    }
                    if send_stream_event(
                        &tx,
                        &runtime_id,
                        &event_log_epoch,
                        &published.event,
                        emit_projection_effect,
                    )
                    .await
                    .is_err()
                    {
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
    let stream = ReceiverStream::new(out_rx);
    let keep_alive = KeepAlive::new()
        .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
        .text("heartbeat");
    Ok((
        [(
            axum::http::HeaderName::from_static(
                crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
            ),
            crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
        )],
        Sse::new(stream).keep_alive(keep_alive),
    ))
}

pub async fn global_events_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    }
    let mut rx = state.host.subscribe_events();
    let (tx, rx_out) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(published) => {
                    let Some(agent_id) = published.agent_id.as_deref() else {
                        continue;
                    };
                    let payload = serde_json::json!({ "agent_id": agent_id }).to_string();
                    if tx
                        .send(Ok(Event::default()
                            .event("agent_roster_hint")
                            .data(payload)))
                        .await
                        .is_err()
                    {
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
    let stream = ReceiverStream::new(rx_out);
    let keep_alive = KeepAlive::new()
        .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
        .text("heartbeat");
    Ok((
        [(
            axum::http::HeaderName::from_static(
                crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
            ),
            crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
        )],
        Sse::new(stream).keep_alive(keep_alive),
    ))
}

fn initial_buffered_events(
    events: &[AuditEvent],
    after_seq: Option<u64>,
    recovery_window: &crate::runtime_db::AgentEventRecoveryWindow,
) -> std::result::Result<VecDeque<AuditEvent>, (StatusCode, Json<Value>)> {
    let start_index = if let Some(after_seq) = after_seq {
        if after_seq == 0 {
            0
        } else {
            match events.iter().position(|event| event.event_seq == after_seq) {
                Some(position) => position + 1,
                None => return Err(event_seq_not_found(after_seq, recovery_window)),
            }
        }
    } else {
        events.len()
    };
    Ok(events.iter().skip(start_index).cloned().collect())
}

fn oldest_seq(events: &[AuditEvent], order: EventPageOrder) -> Option<u64> {
    match order {
        EventPageOrder::Asc => events.first(),
        EventPageOrder::Desc => events.last(),
    }
    .map(|event| event.event_seq)
}

fn newest_seq(events: &[AuditEvent], order: EventPageOrder) -> Option<u64> {
    match order {
        EventPageOrder::Asc => events.last(),
        EventPageOrder::Desc => events.first(),
    }
    .map(|event| event.event_seq)
}

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

async fn send_stream_event(
    tx: &tokio::sync::mpsc::Sender<Result<Event, std::convert::Infallible>>,
    agent_id: &str,
    event_log_epoch: &str,
    event: &AuditEvent,
    emit_projection_effect: bool,
) -> std::result::Result<
    (),
    tokio::sync::mpsc::error::SendError<Result<Event, std::convert::Infallible>>,
> {
    let envelope = stream_event_envelope(agent_id, event_log_epoch, event, emit_projection_effect);
    let payload = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
    tx.send(Ok(Event::default()
        .id(envelope.event_seq.to_string())
        .event(envelope.event_type)
        .data(payload)))
        .await
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

fn event_fallback_summary(event: &AuditEvent) -> String {
    event
        .data
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or(event.kind.as_str())
        .to_string()
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
