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
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    }
    let cursor_seq = runtime
        .storage()
        .latest_event_seq()
        .map_err(error_response)?;
    let max_level = query.max_level;
    let filter_context = match max_level {
        Some(_) => Some(
            event_filter_context(&runtime)
                .await
                .map_err(error_response)?,
        ),
        None => None,
    };
    let page = runtime
        .storage()
        .read_event_page_matching(
            query.before_seq,
            query.after_seq,
            limit,
            order.into(),
            |event| match (max_level, filter_context.as_ref()) {
                (Some(level), Some(filter_context)) => is_operator_event_in_display_mode(
                    &event.kind,
                    &event.data,
                    &event_fallback_summary(event),
                    filter_context,
                    level,
                ),
                _ => true,
            },
        )
        .map_err(error_response)?;
    let oldest_seq = oldest_seq(&page.events, order);
    let newest_seq = newest_seq(&page.events, order);
    let events = page
        .events
        .iter()
        .map(|event| stream_event_envelope(&agent_id, event))
        .collect();
    Ok(Json(EventsPageResponse {
        events,
        oldest_seq,
        newest_seq,
        cursor_seq,
        has_older: page.has_older,
        has_newer: page.has_newer,
        order,
        limit,
    }))
}

pub async fn message(
    Path((agent_id, message_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    }
    let Some(message) = runtime
        .storage()
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
    Json(request): Json<BatchGetMessagesRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    }
    let mut messages = Vec::new();
    let mut missing_message_ids = Vec::new();
    for message_id in request.message_ids {
        match runtime
            .storage()
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
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    }
    let mut live_rx = runtime
        .storage()
        .subscribe_events()
        .map_err(error_response)?
        .ok_or_else(|| error_response(anyhow!("event bus unavailable")))?;
    let events = runtime
        .storage()
        .read_recent_events(event_window_limit.saturating_add(1))
        .map_err(error_response)?;
    let buffered = initial_buffered_events(&events, after_seq)?;
    let (tx, out_rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
    let runtime_id = agent_id.clone();
    tokio::spawn(async move {
        let mut last_sent_seq = after_seq.unwrap_or(0);
        for event in buffered {
            if send_stream_event(&tx, &runtime_id, &event).await.is_err() {
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
                    if send_stream_event(&tx, &runtime_id, &published.event)
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
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let stream = ReceiverStream::new(out_rx);
    let keep_alive = KeepAlive::new()
        .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
        .text("heartbeat");
    Ok(Sse::new(stream).keep_alive(keep_alive))
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
                    if send_stream_event(&tx, agent_id, &published.event)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "global event stream receiver lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let stream = ReceiverStream::new(rx_out);
    let keep_alive = KeepAlive::new()
        .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
        .text("heartbeat");
    Ok(Sse::new(stream).keep_alive(keep_alive))
}

fn initial_buffered_events(
    events: &[AuditEvent],
    after_seq: Option<u64>,
) -> std::result::Result<VecDeque<AuditEvent>, (StatusCode, Json<Value>)> {
    let start_index = if let Some(after_seq) = after_seq {
        if after_seq == 0 {
            0
        } else {
            match events.iter().position(|event| event.event_seq == after_seq) {
                Some(position) => position + 1,
                None => return Err(event_seq_not_found(after_seq)),
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

fn stream_event_envelope(agent_id: &str, event: &AuditEvent) -> StreamEventEnvelope {
    StreamEventEnvelope {
        id: event.id.clone(),
        event_seq: event.event_seq,
        ts: event.created_at,
        agent_id: agent_id.to_string(),
        event_type: event.kind.clone(),
        provenance: event_replay_provenance(&event.data),
        payload: event.data.clone(),
    }
}

async fn send_stream_event(
    tx: &tokio::sync::mpsc::Sender<Result<Event, std::convert::Infallible>>,
    agent_id: &str,
    event: &AuditEvent,
) -> std::result::Result<
    (),
    tokio::sync::mpsc::error::SendError<Result<Event, std::convert::Infallible>>,
> {
    let envelope = stream_event_envelope(agent_id, event);
    let payload = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
    tx.send(Ok(Event::default()
        .id(envelope.event_seq.to_string())
        .event(envelope.event_type)
        .data(payload)))
        .await
}

async fn event_filter_context(
    runtime: &crate::runtime::RuntimeHandle,
) -> Result<OperatorPresentationContext> {
    let agent = runtime.agent_summary().await?;
    let completed_work_item_ids = runtime
        .latest_work_items()
        .await?
        .into_iter()
        .filter(|item| item.state == WorkItemState::Completed)
        .map(|item| item.id)
        .collect();
    Ok(OperatorPresentationContext {
        awaiting_operator_input: agent.closure.waiting_reason
            == Some(WaitingReason::AwaitingOperatorInput),
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

fn event_replay_provenance(payload: &Value) -> EventReplayProvenance {
    EventReplayProvenance {
        origin: clone_payload_field(payload, "origin"),
        authority_class: clone_payload_field(payload, "authority_class"),
        delivery_surface: clone_payload_field(payload, "delivery_surface"),
        admission_context: clone_payload_field(payload, "admission_context"),
        transport: clone_payload_field(payload, "transport"),
        source: clone_payload_field(payload, "source"),
        reply_route: clone_payload_field(payload, "reply_route"),
        message_id: clone_payload_field(payload, "message_id"),
        task_id: clone_payload_field(payload, "task_id"),
        work_item_id: clone_payload_field(payload, "work_item_id"),
        correlation_id: clone_payload_field(payload, "correlation_id"),
        causation_id: clone_payload_field(payload, "causation_id"),
    }
}

fn clone_payload_field(payload: &Value, field: &str) -> Option<Value> {
    payload.get(field).filter(|value| !value.is_null()).cloned()
}

fn event_seq_not_found(after_seq: u64) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new(format!(
            "after_seq {after_seq} was not found in the replay window"
        ))
        .code("cursor_not_found")
        .extension("after_seq", after_seq)
        .extension("event_seq", after_seq),
    )
}
