use holon_server::events::borrowed_event_envelope;

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
    let event_kind = query.event_kind;
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let read_state = Arc::clone(&state);
    let read_agent_id = agent_id.clone();
    let handle = tokio::runtime::Handle::current();
    let before_seq = query.before_seq;
    let after_seq = query.after_seq;
    let page = storage
        .read_block("http.events.page", move |storage| {
            let scope = crate::runtime_db::read_block::current_scope()
                .ok_or_else(|| anyhow::anyhow!("Event read requires blocking read scope"))?;
            let db = storage
                .runtime_db()?
                .ok_or_else(|| anyhow::anyhow!("Event read requires runtime database"))?
                .with_read_scope(scope);
            Ok(read_state.host.operator_event_page_scoped(
                &read_agent_id,
                before_seq,
                after_seq,
                limit,
                order.into(),
                Some(crate::runtime_db::current_query::EventPageScan {
                    kind: event_kind.as_deref(),
                }),
                &db,
                &handle,
                |_| Ok(|_: &AuditEvent| true),
            ))
        })
        .await
        .map_err(error_response)?
        .map_err(|error| agent_read_access_error(&agent_id, AgentReadSurface::EventPage, error))?;
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
        next_before_seq: page.next_before_seq,
        next_after_seq: page.next_after_seq,
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
    next_before_seq: Option<u64>,
    next_after_seq: Option<u64>,
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
    let read_message_id = message_id.clone();
    let Some(message) = storage
        .read_block("http.events.message", move |storage| {
            Ok(storage.read_message_by_id(&read_message_id)?)
        })
        .await
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
    let (messages, missing_message_ids) = storage
        .read_block("http.events.messages_batch", move |storage| {
            let mut messages = Vec::new();
            let mut missing_message_ids = Vec::new();
            for message_id in request.message_ids {
                match storage.read_message_by_id(&message_id)? {
                    Some(message) if message.agent_id == agent_id => messages.push(message),
                    _ => missing_message_ids.push(message_id),
                }
            }
            Ok((messages, missing_message_ids))
        })
        .await
        .map_err(error_response)?;
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
    let storage = state
        .host
        .operator_agent_read_storage(&agent_id)
        .map_err(agent_access_error)?;
    let read_state = Arc::clone(&state);
    let read_agent_id = agent_id.clone();
    let handle = tokio::runtime::Handle::current();
    let page = storage
        .read_block("http.events.stream", move |storage| {
            let scope = crate::runtime_db::read_block::current_scope()
                .ok_or_else(|| anyhow::anyhow!("Event read requires blocking read scope"))?;
            let db = storage
                .runtime_db()?
                .ok_or_else(|| anyhow::anyhow!("Event read requires runtime database"))?
                .with_read_scope(scope);
            Ok(read_state.host.operator_event_page_scoped(
                &read_agent_id,
                None,
                None,
                event_window_limit.saturating_add(1),
                EventLogPageOrder::Desc,
                None,
                &db,
                &handle,
                |_| Ok(|_: &AuditEvent| true),
            ))
        })
        .await
        .map_err(error_response)?
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
    Ok(holon_server::sse::runtime_event_stream(out_rx))
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
    // Keep the existing roster lane available to its existing readers, but
    // never establish an inbox baseline without control authorization.
    let recipient = super::notifications::recipient(&headers, &state).ok();
    let mut last_inbox = if let Some(recipient) = recipient.clone() {
        Some(
            super::notifications::snapshot(&host, recipient)
                .await
                .map_err(error_response)?,
        )
    } else {
        None
    };
    let (tx, rx_out) = tokio::sync::mpsc::channel::<Result<Bytes, std::convert::Infallible>>(32);
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = tx.closed() => break,
                event = rx.recv() => event,
            };
            match next {
                Ok(published) => {
                    if let (Some(recipient), Some(previous)) = (&recipient, &mut last_inbox) {
                        if super::notifications::could_change(&published.event.kind) {
                            let Ok(current) =
                                super::notifications::snapshot(&host, recipient.clone()).await
                            else {
                                break;
                            };
                            if current != *previous {
                                let (headers, state) = (headers.clone(), state.clone());
                                let authorized = tokio::task::spawn_blocking(move || {
                                    super::notifications::recipient(&headers, &state).ok()
                                })
                                .await;
                                if !matches!(authorized, Ok(Some(actor))
                                    if actor.principal_id == recipient.principal_id
                                        && actor.local_control == recipient.local_control)
                                {
                                    break;
                                }
                                *previous = current;
                                if tx
                                    .send(Ok(Bytes::from_static(
                                        super::notifications::CHANGED_FRAME,
                                    )))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
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
    Ok(holon_server::sse::runtime_event_stream(rx_out))
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

/// Whether event pages and SSE should emit the additive `projection_effect`
/// field: only while the durable `event_projection_effect_complete`
/// verification advertises `events.projection-effect.v1`.
fn projection_effect_emission_enabled(state: &AppState) -> bool {
    let verification = load_observer_sync_verification(state);
    advertised_observer_sync_capabilities(&verification).contains(&PROJECTION_EFFECT_CAPABILITY)
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
