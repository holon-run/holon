//! Bounded conversation snapshot reads and projected change streaming.

use super::*;
use crate::domain::conversation::{
    ConversationActivity, ConversationChange, ConversationShadowDiagnostics,
    ConversationTurnSummary, DetailCoverage, PendingInput, CONVERSATION_QUERY_VERSION,
    CONVERSATION_SCHEMA_VERSION,
};
use crate::runtime_db::conversation::{
    ConversationChangeBatch, ConversationIdentity, ConversationReadError, ConversationResetReason,
    ConversationSnapshot, MAX_CONVERSATION_CHANGE_ACTIVITIES, MAX_CONVERSATION_CHANGE_EVENTS,
};

pub(crate) const CONVERSATION_SUMMARY_DEFAULT_LIMIT: usize = 30;
pub(crate) const CONVERSATION_ACTIVITY_DEFAULT_LIMIT: usize = 50;
pub(crate) const CONVERSATION_SHADOW_DEFAULT_LIMIT: usize = 30;
pub(crate) const CONVERSATION_TURN_MAX_SERIALIZED_BYTES: usize = 64 * 1024;
pub(crate) const CONVERSATION_ACTIVITY_ITEM_MAX_SERIALIZED_BYTES: usize = 256 * 1024;
pub(crate) const CONVERSATION_SUMMARY_MAX_SERIALIZED_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const CONVERSATION_ACTIVITY_MAX_SERIALIZED_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const CONVERSATION_READ_TIMEOUT: Duration = Duration::from_secs(10);
const CONVERSATION_STREAM_QUEUE_CAPACITY: usize = 32;
const CONVERSATION_STREAM_SEND_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn identity_changed() -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::CONFLICT,
        HttpErrorEnvelope::new(
            "conversation_identity_changed",
            "The conversation agent identity is no longer available.",
        )
        .retryable(false),
    )
}

pub(crate) fn request_incarnation(
    headers: &HeaderMap,
) -> Result<Option<u64>, (StatusCode, Json<Value>)> {
    headers
        .get("x-holon-expected-incarnation")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(identity_changed)
        })
        .transpose()
}

/// Admission is against the identity captured with the data, never a second read.
pub(crate) fn bind_response(
    request: &HeaderMap,
    identity: &ConversationIdentity,
    mut response: AxumResponse,
) -> AxumResponse {
    if let Some(expected) = request.get("x-holon-expected-incarnation") {
        if expected
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            != Some(identity.agent_incarnation)
        {
            return identity_changed().into_response();
        }
    }
    let Ok(agent_id) = HeaderValue::from_str(&identity.agent_id) else {
        return identity_changed().into_response();
    };
    response.headers_mut().insert("x-holon-agent-id", agent_id);
    response.headers_mut().insert(
        "x-holon-agent-incarnation",
        HeaderValue::from(identity.agent_incarnation),
    );
    response
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationReadLimits {
    pub max_turn_serialized_bytes: usize,
    pub max_activity_item_serialized_bytes: usize,
    pub max_summary_serialized_bytes: usize,
    pub max_activity_serialized_bytes: usize,
    pub timeout: Duration,
}

impl Default for ConversationReadLimits {
    fn default() -> Self {
        Self {
            max_turn_serialized_bytes: CONVERSATION_TURN_MAX_SERIALIZED_BYTES,
            max_activity_item_serialized_bytes: CONVERSATION_ACTIVITY_ITEM_MAX_SERIALIZED_BYTES,
            max_summary_serialized_bytes: CONVERSATION_SUMMARY_MAX_SERIALIZED_BYTES,
            max_activity_serialized_bytes: CONVERSATION_ACTIVITY_MAX_SERIALIZED_BYTES,
            timeout: CONVERSATION_READ_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationReadQuery {
    pub limit: Option<usize>,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationLocateRequest {
    pub source_ref: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ConversationLocateResponse {
    pub schema_version: u32,
    pub query_version: u32,
    pub agent_id: String,
    pub status: String,
    pub source_ref: String,
    pub target: Option<crate::runtime_db::conversation::ConversationLocateTarget>,
    pub reason: Option<String>,
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub window: Option<ConversationSummaryResponse>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ConversationHistoryCoverage {
    pub first_turn_key: Option<crate::domain::conversation::TurnKey>,
    pub last_turn_key: Option<crate::domain::conversation::TurnKey>,
    pub membership_upper_bound: Option<crate::domain::conversation::TurnKey>,
    pub has_older: bool,
    pub has_newer: bool,
    pub reaches_oldest: bool,
    pub reaches_latest: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationStreamQuery {
    pub after: Option<String>,
    pub limit: Option<usize>,
    pub activity_limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationShadowQuery {
    pub turn_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ConversationSummaryResponse {
    pub schema_version: u32,
    pub query_version: u32,
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub snapshot_through_seq: u64,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub snapshot_cursor: String,
    pub turns: Vec<ConversationTurnSummary>,
    pub active_turns: Vec<ConversationTurnSummary>,
    pub pending_inputs: Vec<PendingInput>,
    pub next_before_cursor: Option<String>,
    pub next_after_cursor: Option<String>,
    pub coverage: ConversationHistoryCoverage,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ConversationActivityResponse {
    pub schema_version: u32,
    pub query_version: u32,
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub snapshot_through_seq: u64,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub snapshot_cursor: String,
    pub turn: ConversationTurnSummary,
    pub detail_revision: u64,
    pub activities: Vec<ConversationActivity>,
    pub coverage: DetailCoverage,
    pub next_before_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ConversationStreamMessage {
    BatchBegin {
        batch_id: String,
        schema_version: u32,
        query_version: u32,
        runtime_id: String,
        event_log_epoch: String,
        visibility_scope_id: String,
        from_seq: u64,
        through_seq: u64,
    },
    OperatorUpsert {
        input: PendingInput,
    },
    OperatorRemove {
        message_id: String,
        revision: u64,
    },
    TurnSummaryUpsert {
        turn: ConversationTurnSummary,
    },
    ActivityUpsert {
        turn_id: String,
        activity: ConversationActivity,
    },
    DetailInvalidated {
        turn_id: String,
        detail_revision: u64,
    },
    Checkpoint {
        batch_id: String,
        event_log_epoch: String,
        visibility_scope_id: String,
        through_seq: u64,
        checkpoint: String,
    },
    ResetRequired {
        reason: ConversationStreamResetReason,
        oldest_retained_seq: Option<u64>,
        event_head_seq: Option<u64>,
        hint: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationStreamResetReason {
    RetentionExpired,
    CursorAhead,
    ReplayLimitExceeded,
    SchemaVersionMismatch,
    QueryVersionMismatch,
    EventLogEpochMismatch,
    CursorRejected,
    AgentNotFound,
    AgentIdentityChanged,
    StreamRecoveryFailed,
    SlowConsumer,
}

impl ConversationStreamMessage {
    fn event_name(&self) -> &'static str {
        match self {
            Self::BatchBegin { .. } => "batch_begin",
            Self::OperatorUpsert { .. } => "operator_upsert",
            Self::OperatorRemove { .. } => "operator_remove",
            Self::TurnSummaryUpsert { .. } => "turn_summary_upsert",
            Self::ActivityUpsert { .. } => "activity_upsert",
            Self::DetailInvalidated { .. } => "detail_invalidated",
            Self::Checkpoint { .. } => "checkpoint",
            Self::ResetRequired { .. } => "reset_required",
        }
    }
}

#[derive(Debug)]
enum BoundedBlockingReadError {
    Timeout,
    Join(tokio::task::JoinError),
}

async fn bounded_blocking_read<T, F>(
    timeout: Duration,
    read: F,
) -> std::result::Result<T, BoundedBlockingReadError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(read)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(BoundedBlockingReadError::Join(error)),
        Err(_) => Err(BoundedBlockingReadError::Timeout),
    }
}

pub async fn stream(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationStreamQuery>,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let expected_incarnation = match request_incarnation(&headers) {
        Ok(expected) => expected,
        Err(error) => return error.into_response(),
    };
    let header_cursor = match headers.get("last-event-id") {
        Some(value) => match value.to_str() {
            Ok(value) if !value.is_empty() => Some(value.to_string()),
            Ok(_) => None,
            Err(_) => {
                crate::diagnostics::record_conversation_cursor_failure();
                return http_error(
                    StatusCode::BAD_REQUEST,
                    HttpErrorEnvelope::new(
                        "conversation_cursor_malformed",
                        "Last-Event-ID is not valid UTF-8",
                    ),
                )
                .into_response();
            }
        },
        None => None,
    };
    let after = header_cursor.or(query.after);
    let event_limit = query.limit.unwrap_or(MAX_CONVERSATION_CHANGE_EVENTS);
    let activity_limit = query
        .activity_limit
        .unwrap_or(MAX_CONVERSATION_CHANGE_ACTIVITIES);
    let (scope_principal, scope_entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(authority) => authority,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let host = state.host.clone();
    let mut live_rx = host.subscribe_events();
    let initial_agent_id = agent_id.clone();
    let initial_after = after.clone();
    let initial_principal = scope_principal.clone();
    let read_timeout = state.conversation_read_limits.timeout;
    let recovery_started_at = std::time::Instant::now();
    let initial = match bounded_blocking_read(read_timeout, move || {
        host.runtime_db()
            .conversation()
            .with_expected_incarnation(expected_incarnation)
            .change_batch(
                &initial_agent_id,
                initial_after.as_deref(),
                event_limit,
                activity_limit,
                &initial_principal,
                scope_entitlement,
            )
    })
    .await
    {
        Ok(Ok(Some(batch))) => {
            crate::diagnostics::record_conversation_stream_recovery(recovery_started_at.elapsed());
            batch
        }
        Ok(Ok(None)) => return agent_not_found().into_response(),
        Ok(Err(error)) => return conversation_stream_error(error).into_response(),
        Err(BoundedBlockingReadError::Join(error)) => {
            return error_response(error.into()).into_response()
        }
        Err(BoundedBlockingReadError::Timeout) => {
            return timeout_error("stream recovery", read_timeout).into_response()
        }
    };

    let identity = initial.identity.clone();
    let admission = bind_response(&headers, &identity, StatusCode::OK.into_response());
    if admission.status() != StatusCode::OK {
        return admission;
    }
    let (tx, rx) = tokio::sync::mpsc::channel(CONVERSATION_STREAM_QUEUE_CAPACITY);
    let tx = BoundStreamSender {
        tx,
        identity: identity.clone(),
        identity_bound: expected_incarnation.is_some(),
    };
    let host = state.host.clone();
    tokio::spawn(async move {
        let mut checkpoint = initial.checkpoint.clone();
        let mut through_seq = initial.through_seq;
        if !send_change_batch(&tx, initial).await {
            return;
        }
        let expected_incarnation = tx.identity.agent_incarnation;
        let mut lifecycle_check = tokio::time::interval(EVENT_STREAM_HEARTBEAT_INTERVAL);
        lifecycle_check.tick().await;
        loop {
            let published = tokio::select! {
                _ = tx.tx.closed() => return,
                _ = lifecycle_check.tick() => Ok(true),
                published = live_rx.recv() => published.map(|published|
                    published.agent_id.as_deref() == Some(agent_id.as_str())
                        && published.event.event_seq > through_seq),
            };
            match published {
                Ok(true) => {
                    let host = host.clone();
                    let agent_id_for_read = agent_id.clone();
                    let after = checkpoint.clone();
                    let scope_principal = scope_principal.clone();
                    let recovery_started_at = std::time::Instant::now();
                    let batch = bounded_blocking_read(read_timeout, move || {
                        host.runtime_db()
                            .conversation()
                            .with_expected_incarnation(Some(expected_incarnation))
                            .change_batch(
                                &agent_id_for_read,
                                Some(&after),
                                event_limit,
                                activity_limit,
                                &scope_principal,
                                scope_entitlement,
                            )
                    })
                    .await;
                    match batch {
                        Ok(Ok(Some(batch))) => {
                            crate::diagnostics::record_conversation_stream_recovery(
                                recovery_started_at.elapsed(),
                            );
                            if batch.through_seq == through_seq && batch.changes.is_empty() {
                                continue;
                            }
                            checkpoint = batch.checkpoint.clone();
                            through_seq = batch.through_seq;
                            if !send_change_batch(&tx, batch).await {
                                return;
                            }
                        }
                        Ok(Ok(None)) => {
                            crate::diagnostics::record_conversation_reset(
                                crate::diagnostics::ConversationResetMetricReason::AgentNotFound,
                            );
                            let _ = send_stream_message(
                                &tx,
                                identity_changed_reset(tx.identity_bound),
                                None,
                            )
                            .await;
                            return;
                        }
                        Ok(Err(error)) => {
                            record_background_stream_error(&error);
                            let _ = send_stream_message(
                                &tx,
                                reset_message_for_error(&error, tx.identity_bound),
                                None,
                            )
                            .await;
                            return;
                        }
                        Err(BoundedBlockingReadError::Join(error)) => {
                            warn!(%error, "conversation stream recovery task failed");
                            return;
                        }
                        Err(BoundedBlockingReadError::Timeout) => {
                            warn!(?read_timeout, %agent_id, "conversation stream recovery timed out");
                            crate::diagnostics::record_conversation_timeout();
                            crate::diagnostics::record_conversation_reset(
                                crate::diagnostics::ConversationResetMetricReason::StreamRecoveryFailed,
                            );
                            let _ = send_stream_message(
                                &tx,
                                ConversationStreamMessage::ResetRequired {
                                    reason: ConversationStreamResetReason::StreamRecoveryFailed,
                                    oldest_retained_seq: None,
                                    event_head_seq: None,
                                    hint: "bootstrap a fresh conversation snapshot".to_string(),
                                },
                                None,
                            )
                            .await;
                            return;
                        }
                    }
                }
                Ok(false) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, %agent_id, "conversation stream receiver lagged");
                    crate::diagnostics::record_conversation_slow_consumer();
                    crate::diagnostics::record_conversation_reset(
                        crate::diagnostics::ConversationResetMetricReason::SlowConsumer,
                    );
                    let _ = send_stream_message(
                        &tx,
                        ConversationStreamMessage::ResetRequired {
                            reason: ConversationStreamResetReason::SlowConsumer,
                            oldest_retained_seq: None,
                            event_head_seq: None,
                            hint: "bootstrap a fresh conversation snapshot".to_string(),
                        },
                        None,
                    )
                    .await;
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    bind_response(
        &headers,
        &identity,
        Sse::new(ReceiverStream::new(rx))
            .keep_alive(
                KeepAlive::new()
                    .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
                    .text("heartbeat"),
            )
            .into_response(),
    )
}

struct BoundStreamSender {
    tx: tokio::sync::mpsc::Sender<std::result::Result<Event, anyhow::Error>>,
    identity: ConversationIdentity,
    identity_bound: bool,
}

async fn send_change_batch(tx: &BoundStreamSender, batch: ConversationChangeBatch) -> bool {
    send_change_batch_with_timeout(tx, batch, CONVERSATION_STREAM_SEND_TIMEOUT).await
}

async fn send_change_batch_with_timeout(
    tx: &BoundStreamSender,
    batch: ConversationChangeBatch,
    send_timeout: Duration,
) -> bool {
    if batch.identity != tx.identity {
        let _ = send_stream_message(tx, identity_changed_reset(tx.identity_bound), None).await;
        return false;
    }
    let batch_id = format!("conversation:{}:{}", batch.from_seq, batch.through_seq);
    if !send_stream_message_with_timeout(
        tx,
        ConversationStreamMessage::BatchBegin {
            batch_id: batch_id.clone(),
            schema_version: CONVERSATION_SCHEMA_VERSION,
            query_version: CONVERSATION_QUERY_VERSION,
            runtime_id: batch.runtime_id,
            event_log_epoch: batch.event_log_epoch.clone(),
            visibility_scope_id: batch.visibility_scope_id.clone(),
            from_seq: batch.from_seq,
            through_seq: batch.through_seq,
        },
        None,
        send_timeout,
    )
    .await
    {
        return false;
    }
    for change in batch.changes {
        let message = match change {
            ConversationChange::OperatorUpsert { input } => {
                ConversationStreamMessage::OperatorUpsert { input }
            }
            ConversationChange::OperatorRemove {
                message_id,
                revision,
            } => ConversationStreamMessage::OperatorRemove {
                message_id,
                revision,
            },
            ConversationChange::TurnSummaryUpsert { turn } => {
                ConversationStreamMessage::TurnSummaryUpsert { turn }
            }
            ConversationChange::ActivityUpsert { turn_id, activity } => {
                ConversationStreamMessage::ActivityUpsert { turn_id, activity }
            }
            ConversationChange::DetailInvalidated {
                turn_id,
                detail_revision,
            } => ConversationStreamMessage::DetailInvalidated {
                turn_id,
                detail_revision,
            },
        };
        if !send_stream_message_with_timeout(tx, message, None, send_timeout).await {
            return false;
        }
    }
    let checkpoint = batch.checkpoint;
    send_stream_message_with_timeout(
        tx,
        ConversationStreamMessage::Checkpoint {
            batch_id,
            event_log_epoch: batch.event_log_epoch,
            visibility_scope_id: batch.visibility_scope_id,
            through_seq: batch.through_seq,
            checkpoint: checkpoint.clone(),
        },
        Some(checkpoint),
        send_timeout,
    )
    .await
}

async fn send_stream_message(
    tx: &BoundStreamSender,
    message: ConversationStreamMessage,
    id: Option<String>,
) -> bool {
    send_stream_message_with_timeout(tx, message, id, CONVERSATION_STREAM_SEND_TIMEOUT).await
}

async fn send_stream_message_with_timeout(
    tx: &BoundStreamSender,
    message: ConversationStreamMessage,
    id: Option<String>,
    send_timeout: Duration,
) -> bool {
    let event_name = message.event_name();
    #[derive(Serialize)]
    struct BoundMessage<'a> {
        #[serde(flatten)]
        identity: &'a ConversationIdentity,
        #[serde(flatten)]
        message: ConversationStreamMessage,
    }
    let mut event = match Event::default().event(event_name).json_data(BoundMessage {
        identity: &tx.identity,
        message,
    }) {
        Ok(event) => event,
        Err(error) => {
            warn!(%error, event_name, "failed to serialize conversation stream message");
            return false;
        }
    };
    if let Some(id) = id {
        event = event.id(id);
    }
    match tokio::time::timeout(send_timeout, tx.tx.send(Ok(event))).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            crate::diagnostics::record_conversation_slow_consumer();
            false
        }
    }
}

fn stream_reset_reason(reason: ConversationResetReason) -> ConversationStreamResetReason {
    match reason {
        ConversationResetReason::RetentionExpired => {
            ConversationStreamResetReason::RetentionExpired
        }
        ConversationResetReason::CursorAhead => ConversationStreamResetReason::CursorAhead,
        ConversationResetReason::ReplayLimitExceeded => {
            ConversationStreamResetReason::ReplayLimitExceeded
        }
    }
}

fn identity_changed_reset(identity_bound: bool) -> ConversationStreamMessage {
    ConversationStreamMessage::ResetRequired {
        reason: if identity_bound {
            ConversationStreamResetReason::AgentIdentityChanged
        } else {
            ConversationStreamResetReason::AgentNotFound
        },
        oldest_retained_seq: None,
        event_head_seq: None,
        hint: "resolve the agent identity before opening another conversation".into(),
    }
}

fn reset_message_for_error(
    error: &anyhow::Error,
    identity_bound: bool,
) -> ConversationStreamMessage {
    if matches!(
        error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::IdentityChanged)
    ) {
        return identity_changed_reset(identity_bound);
    }
    if let Some(ConversationReadError::ResetRequired {
        reason,
        oldest_retained_seq,
        event_head_seq,
        ..
    }) = error.downcast_ref::<ConversationReadError>()
    {
        return ConversationStreamMessage::ResetRequired {
            reason: stream_reset_reason(*reason),
            oldest_retained_seq: Some(*oldest_retained_seq),
            event_head_seq: Some(*event_head_seq),
            hint: "bootstrap a fresh conversation snapshot".to_string(),
        };
    }
    if let Some(ConversationReadError::Cursor(cursor_error)) =
        error.downcast_ref::<ConversationReadError>()
    {
        let reason = match cursor_error {
            crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch { .. } => {
                ConversationStreamResetReason::SchemaVersionMismatch
            }
            crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. } => {
                ConversationStreamResetReason::QueryVersionMismatch
            }
            crate::domain::conversation::CursorDecodeError::EventLogEpochMismatch => {
                ConversationStreamResetReason::EventLogEpochMismatch
            }
            _ => ConversationStreamResetReason::CursorRejected,
        };
        return ConversationStreamMessage::ResetRequired {
            reason,
            oldest_retained_seq: None,
            event_head_seq: None,
            hint: "bootstrap a fresh conversation snapshot".to_string(),
        };
    }
    ConversationStreamMessage::ResetRequired {
        reason: ConversationStreamResetReason::StreamRecoveryFailed,
        oldest_retained_seq: None,
        event_head_seq: None,
        hint: "bootstrap a fresh conversation snapshot".to_string(),
    }
}

pub async fn locate(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ConversationLocateRequest>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let expected_incarnation = match request_incarnation(&headers) {
        Ok(expected) => expected,
        Err(error) => return error.into_response(),
    };
    let (principal, entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(authority) => authority,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let limits = state.conversation_read_limits.clone();
    let host = state.host.clone();
    let response_agent_id = agent_id.clone();
    let result = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db()
                .conversation()
                .with_expected_incarnation(expected_incarnation)
                .locate_source(
                    &agent_id,
                    &request.source_ref,
                    request.limit.unwrap_or(CONVERSATION_SUMMARY_DEFAULT_LIMIT),
                    &principal,
                    entitlement,
                )
        }),
    )
    .await
    {
        Ok(Ok(Ok(Some(result)))) => result,
        Ok(Ok(Ok(None))) => return agent_not_found().into_response(),
        Ok(Ok(Err(error))) => return conversation_error(error).into_response(),
        Ok(Err(error)) => return error_response(error.into()).into_response(),
        Err(_) => return timeout_error("locate", limits.timeout).into_response(),
    };
    let identity = result.identity.clone();
    let response = ConversationLocateResponse {
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
        agent_id: response_agent_id,
        status: result.status.into(),
        source_ref: result.source_ref,
        target: result.target,
        reason: result.reason.map(str::to_owned),
        runtime_id: result.runtime_id,
        event_log_epoch: result.event_log_epoch,
        visibility_scope_id: result.visibility_scope_id,
        window: result.window.map(summary_response),
    };
    if let Some(window) = &response.window {
        if let Some(error) = oversized_turn(
            window.turns.iter().chain(window.active_turns.iter()),
            limits.max_turn_serialized_bytes,
        ) {
            return error.into_response();
        }
    }
    let bytes = match serialize_json("/agents/{agent_id}/conversation/locate", &response) {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    if bytes.len() > limits.max_summary_serialized_bytes {
        return payload_too_large(
            "conversation_snapshot_too_large",
            "conversation locate window exceeds the maximum serialized response size",
            bytes.len(),
            limits.max_summary_serialized_bytes,
        )
        .into_response();
    }
    bind_response(
        &headers,
        &identity,
        traced_json_bytes("/agents/{agent_id}/conversation/locate", started_at, bytes),
    )
}

pub async fn summary(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationReadQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let expected_incarnation = match request_incarnation(&headers) {
        Ok(expected) => expected,
        Err(error) => return error.into_response(),
    };
    let limits = state.conversation_read_limits.clone();
    let limit = query.limit.unwrap_or(CONVERSATION_SUMMARY_DEFAULT_LIMIT);
    let before = query.before;
    let after = query.after;
    let host = state.host.clone();
    let (scope_principal, scope_entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(authority) => authority,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let snapshot = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db()
                .conversation()
                .with_expected_incarnation(expected_incarnation)
                .summary_window_snapshot(
                    &agent_id,
                    limit,
                    before.as_deref(),
                    after.as_deref(),
                    &scope_principal,
                    scope_entitlement,
                )
        }),
    )
    .await
    {
        Ok(Ok(Ok(Some(snapshot)))) => snapshot,
        Ok(Ok(Ok(None))) => return agent_not_found().into_response(),
        Ok(Ok(Err(error))) => return conversation_error(error).into_response(),
        Ok(Err(error)) => return error_response(error.into()).into_response(),
        Err(_) => return timeout_error("summary", limits.timeout).into_response(),
    };
    let identity = snapshot.identity.clone();
    let response = summary_response(snapshot);
    if let Some(error) = oversized_turn(
        response.turns.iter().chain(response.active_turns.iter()),
        limits.max_turn_serialized_bytes,
    ) {
        return error.into_response();
    }
    let bytes = match serialize_json("/agents/{agent_id}/conversation", &response) {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    if bytes.len() > limits.max_summary_serialized_bytes {
        return payload_too_large(
            "conversation_snapshot_too_large",
            "conversation summary snapshot exceeds the maximum serialized response size",
            bytes.len(),
            limits.max_summary_serialized_bytes,
        )
        .into_response();
    }
    let etag = etag_for_bytes(&bytes);
    if if_none_match_satisfied(&headers, &etag) {
        return bind_response(&headers, &identity, not_modified_response(etag));
    }
    crate::diagnostics::record_conversation_summary(started_at.elapsed(), bytes.len());
    bind_response(
        &headers,
        &identity,
        traced_json_bytes_with_etag("/agents/{agent_id}/conversation", started_at, bytes, etag),
    )
}

pub async fn activities(
    Path((agent_id, turn_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationReadQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let expected_incarnation = match request_incarnation(&headers) {
        Ok(expected) => expected,
        Err(error) => return error.into_response(),
    };
    if query.after.is_some() {
        return conversation_error(
            ConversationReadError::InvalidRequest(
                "after is supported only for conversation history".into(),
            )
            .into(),
        )
        .into_response();
    }
    let limits = state.conversation_read_limits.clone();
    let limit = query.limit.unwrap_or(CONVERSATION_ACTIVITY_DEFAULT_LIMIT);
    let before = query.before;
    let host = state.host.clone();
    let (scope_principal, scope_entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(authority) => authority,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let snapshot = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db()
                .conversation()
                .with_expected_incarnation(expected_incarnation)
                .activity_snapshot(
                    &agent_id,
                    &turn_id,
                    limit,
                    before.as_deref(),
                    &scope_principal,
                    scope_entitlement,
                )
        }),
    )
    .await
    {
        Ok(Ok(Ok(Some(snapshot)))) => snapshot,
        Ok(Ok(Ok(None))) => return agent_not_found().into_response(),
        Ok(Ok(Err(error))) => return conversation_error(error).into_response(),
        Ok(Err(error)) => return error_response(error.into()).into_response(),
        Err(_) => return timeout_error("activity", limits.timeout).into_response(),
    };
    let identity = snapshot.identity.clone();
    let Some(response) = activity_response(snapshot) else {
        return http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new(
                "conversation_turn_not_found",
                "no conversation turn for this request",
            ),
        )
        .into_response();
    };
    if let Some(error) = oversized_turn(
        std::iter::once(&response.turn),
        limits.max_turn_serialized_bytes,
    ) {
        return error.into_response();
    }
    for activity in &response.activities {
        let serialized_bytes = match serde_json::to_vec(activity) {
            Ok(bytes) => bytes.len(),
            Err(error) => return error_response(error.into()).into_response(),
        };
        if serialized_bytes > limits.max_activity_item_serialized_bytes {
            return payload_too_large(
                "conversation_activity_too_large",
                "conversation activity item exceeds the maximum serialized size",
                serialized_bytes,
                limits.max_activity_item_serialized_bytes,
            )
            .into_response();
        }
    }
    let bytes = match serialize_json("/agents/{agent_id}/turns/{turn_id}/activities", &response) {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    if bytes.len() > limits.max_activity_serialized_bytes {
        return payload_too_large(
            "conversation_activity_page_too_large",
            "conversation activity page exceeds the maximum serialized response size",
            bytes.len(),
            limits.max_activity_serialized_bytes,
        )
        .into_response();
    }
    crate::diagnostics::record_conversation_activity(started_at.elapsed(), bytes.len());
    bind_response(
        &headers,
        &identity,
        traced_json_bytes(
            "/agents/{agent_id}/turns/{turn_id}/activities",
            started_at,
            bytes,
        ),
    )
}

pub async fn shadow_diagnostics(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationShadowQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_control(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let limits = state.conversation_read_limits.clone();
    let turn_limit = query
        .turn_limit
        .unwrap_or(CONVERSATION_SHADOW_DEFAULT_LIMIT);
    let host = state.host.clone();
    let (scope_principal, scope_entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(authority) => authority,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let diagnostics: ConversationShadowDiagnostics = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db().conversation().shadow_diagnostics(
                &agent_id,
                turn_limit,
                &scope_principal,
                scope_entitlement,
            )
        }),
    )
    .await
    {
        Ok(Ok(Ok(Some(diagnostics)))) => diagnostics,
        Ok(Ok(Ok(None))) => return agent_not_found().into_response(),
        Ok(Ok(Err(error))) => return conversation_error(error).into_response(),
        Ok(Err(error)) => return error_response(error.into()).into_response(),
        Err(_) => return timeout_error("shadow diagnostics", limits.timeout).into_response(),
    };
    crate::diagnostics::record_conversation_shadow(
        started_at.elapsed(),
        diagnostics.mismatch_count,
        diagnostics.legacy_unattributed_briefs,
    );
    let bytes = match serialize_json(
        "/control/agents/{agent_id}/conversation/shadow-diagnostics",
        &diagnostics,
    ) {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    if bytes.len() > limits.max_summary_serialized_bytes {
        return payload_too_large(
            "conversation_shadow_diagnostics_too_large",
            "conversation shadow diagnostics exceeds the maximum serialized response size",
            bytes.len(),
            limits.max_summary_serialized_bytes,
        )
        .into_response();
    }
    traced_json_bytes(
        "/control/agents/{agent_id}/conversation/shadow-diagnostics",
        started_at,
        bytes,
    )
}

fn summary_response(
    snapshot: ConversationSnapshot<crate::domain::conversation::ConversationSummaryPage>,
) -> ConversationSummaryResponse {
    let page = snapshot.value;
    let has_older = snapshot.next_before_cursor.is_some();
    let has_newer = snapshot.next_after_cursor.is_some();
    let first_turn_key = page.turns.first().map(|turn| turn.key.clone());
    let last_turn_key = page.turns.last().map(|turn| turn.key.clone());
    ConversationSummaryResponse {
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
        runtime_id: snapshot.runtime_id,
        event_log_epoch: snapshot.event_log_epoch,
        visibility_scope_id: snapshot.visibility_scope_id,
        snapshot_through_seq: snapshot.event_head_seq,
        event_head_seq: snapshot.event_head_seq,
        oldest_retained_seq: snapshot.oldest_retained_seq,
        snapshot_cursor: snapshot.snapshot_cursor,
        turns: page.turns,
        active_turns: page.active_turns,
        pending_inputs: page.pending_inputs,
        next_before_cursor: snapshot.next_before_cursor,
        next_after_cursor: snapshot.next_after_cursor,
        coverage: ConversationHistoryCoverage {
            first_turn_key,
            last_turn_key,
            membership_upper_bound: page.membership_upper_bound,
            has_older,
            has_newer,
            reaches_oldest: !has_older,
            reaches_latest: !has_newer,
        },
        has_more: page.has_more,
    }
}

fn activity_response(
    snapshot: ConversationSnapshot<Option<crate::domain::conversation::ConversationActivityPage>>,
) -> Option<ConversationActivityResponse> {
    let page = snapshot.value?;
    Some(ConversationActivityResponse {
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
        runtime_id: snapshot.runtime_id,
        event_log_epoch: snapshot.event_log_epoch,
        visibility_scope_id: snapshot.visibility_scope_id,
        snapshot_through_seq: snapshot.event_head_seq,
        event_head_seq: snapshot.event_head_seq,
        oldest_retained_seq: snapshot.oldest_retained_seq,
        snapshot_cursor: snapshot.snapshot_cursor,
        turn: page.turn,
        detail_revision: page.detail_revision,
        activities: page.activities,
        coverage: page.coverage,
        next_before_cursor: snapshot.next_before_cursor,
        has_more: page.has_more,
    })
}

fn agent_not_found() -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new("agent_not_found", "no accessible Agent for this request"),
    )
}

pub(crate) fn conversation_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let Some(error) = error.downcast_ref::<ConversationReadError>() else {
        return error_response(error);
    };
    record_conversation_read_error(error);
    match error {
        ConversationReadError::IdentityChanged => identity_changed(),
        ConversationReadError::InvalidRequest(_) => http_error(
            StatusCode::BAD_REQUEST,
            HttpErrorEnvelope::new("conversation_invalid_request", error.to_string()),
        ),
        ConversationReadError::SourceNotFound => http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new("conversation_source_not_found", error.to_string()),
        ),
        ConversationReadError::InvalidLimit {
            resource,
            minimum,
            maximum,
            actual,
        } => http_error(
            StatusCode::BAD_REQUEST,
            HttpErrorEnvelope::new("conversation_invalid_limit", error.to_string())
                .extension("resource", *resource)
                .extension("minimum", *minimum)
                .extension("maximum", *maximum)
                .extension("actual", *actual),
        ),
        ConversationReadError::CountLimitExceeded { resource, limit } => http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            HttpErrorEnvelope::new("conversation_count_limit_exceeded", error.to_string())
                .extension("resource", *resource)
                .extension("limit", *limit),
        ),
        ConversationReadError::CursorOutsideCoverage => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new("conversation_cursor_outside_coverage", error.to_string())
                .hint("restart from a fresh conversation snapshot"),
        ),
        ConversationReadError::Cursor(cursor_error) => {
            let code = match cursor_error {
                crate::domain::conversation::CursorDecodeError::Malformed => {
                    "conversation_cursor_malformed"
                }
                crate::domain::conversation::CursorDecodeError::Tampered => {
                    "conversation_cursor_tampered"
                }
                crate::domain::conversation::CursorDecodeError::KindMismatch => {
                    "conversation_cursor_kind_mismatch"
                }
                crate::domain::conversation::CursorDecodeError::BindingMismatch => {
                    "conversation_cursor_scope_mismatch"
                }
                crate::domain::conversation::CursorDecodeError::EventLogEpochMismatch => {
                    "conversation_cursor_event_log_epoch_mismatch"
                }
                crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch {
                    ..
                } => "conversation_cursor_schema_version_mismatch",
                crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. } => {
                    "conversation_cursor_query_version_mismatch"
                }
            };
            http_error(
                StatusCode::BAD_REQUEST,
                HttpErrorEnvelope::new(code, error.to_string()),
            )
        }
        ConversationReadError::ResetRequired {
            reason,
            requested_seq,
            oldest_retained_seq,
            event_head_seq,
        } => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new("conversation_reset_required", error.to_string())
                .hint("restart from a fresh conversation snapshot")
                .extension(
                    "reason",
                    serde_json::to_value(stream_reset_reason(*reason))
                        .expect("conversation reset reason must serialize"),
                )
                .extension("requested_seq", *requested_seq)
                .extension("oldest_retained_seq", *oldest_retained_seq)
                .extension("event_head_seq", *event_head_seq),
        ),
    }
}

fn conversation_stream_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    if let Some(ConversationReadError::Cursor(cursor_error)) =
        error.downcast_ref::<ConversationReadError>()
    {
        if matches!(
            cursor_error,
            crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch { .. }
                | crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. }
                | crate::domain::conversation::CursorDecodeError::EventLogEpochMismatch
        ) {
            record_conversation_read_error(
                error
                    .downcast_ref::<ConversationReadError>()
                    .expect("conversation error was matched above"),
            );
            record_cursor_reset(cursor_error);
            return http_error(
                StatusCode::CONFLICT,
                HttpErrorEnvelope::new("conversation_reset_required", error.to_string())
                    .hint("restart from a fresh conversation snapshot")
                    .extension(
                        "reason",
                        match cursor_error {
                            crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch {
                                ..
                            } => "schema_version_mismatch",
                            crate::domain::conversation::CursorDecodeError::QueryVersionMismatch {
                                ..
                            } => "query_version_mismatch",
                            _ => "event_log_epoch_mismatch",
                        },
                    ),
            );
        }
    }
    conversation_error(error)
}

fn record_conversation_read_error(error: &ConversationReadError) {
    match error {
        ConversationReadError::IdentityChanged
        | ConversationReadError::InvalidRequest(_)
        | ConversationReadError::SourceNotFound => {}
        ConversationReadError::InvalidLimit { .. }
        | ConversationReadError::CountLimitExceeded { .. } => {
            crate::diagnostics::record_conversation_limit_failure();
        }
        ConversationReadError::CursorOutsideCoverage | ConversationReadError::Cursor(_) => {
            crate::diagnostics::record_conversation_cursor_failure();
        }
        ConversationReadError::ResetRequired { reason, .. } => {
            record_reset_reason(*reason);
        }
    }
}

fn record_background_stream_error(error: &anyhow::Error) {
    let Some(error) = error.downcast_ref::<ConversationReadError>() else {
        return;
    };
    record_conversation_read_error(error);
    if let ConversationReadError::Cursor(error) = error {
        record_cursor_reset(error);
    }
}

fn record_reset_reason(reason: ConversationResetReason) {
    let reason = match reason {
        ConversationResetReason::RetentionExpired => {
            crate::diagnostics::ConversationResetMetricReason::RetentionExpired
        }
        ConversationResetReason::CursorAhead => {
            crate::diagnostics::ConversationResetMetricReason::CursorAhead
        }
        ConversationResetReason::ReplayLimitExceeded => {
            crate::diagnostics::ConversationResetMetricReason::ReplayLimitExceeded
        }
    };
    crate::diagnostics::record_conversation_reset(reason);
}

fn record_cursor_reset(error: &crate::domain::conversation::CursorDecodeError) {
    let reason = match error {
        crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch { .. } => {
            crate::diagnostics::ConversationResetMetricReason::SchemaVersionMismatch
        }
        crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. } => {
            crate::diagnostics::ConversationResetMetricReason::QueryVersionMismatch
        }
        crate::domain::conversation::CursorDecodeError::EventLogEpochMismatch => {
            crate::diagnostics::ConversationResetMetricReason::EventLogEpochMismatch
        }
        crate::domain::conversation::CursorDecodeError::BindingMismatch => {
            crate::diagnostics::ConversationResetMetricReason::VisibilityScopeMismatch
        }
        _ => return,
    };
    crate::diagnostics::record_conversation_reset(reason);
}

fn oversized_turn<'a>(
    turns: impl Iterator<Item = &'a ConversationTurnSummary>,
    max_serialized_bytes: usize,
) -> Option<(StatusCode, Json<Value>)> {
    for turn in turns {
        let serialized_bytes = match serde_json::to_vec(turn) {
            Ok(bytes) => bytes.len(),
            Err(error) => return Some(error_response(error.into())),
        };
        if serialized_bytes > max_serialized_bytes {
            crate::diagnostics::record_conversation_payload_failure();
            return Some(http_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                HttpErrorEnvelope::new(
                    "conversation_turn_too_large",
                    "conversation turn exceeds the maximum serialized size",
                )
                .extension("turn_id", turn.turn_id.clone())
                .extension("serialized_bytes", serialized_bytes)
                .extension("max_serialized_bytes", max_serialized_bytes),
            ));
        }
    }
    None
}

fn payload_too_large(
    code: &'static str,
    message: &'static str,
    serialized_bytes: usize,
    max_serialized_bytes: usize,
) -> (StatusCode, Json<Value>) {
    crate::diagnostics::record_conversation_payload_failure();
    http_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        HttpErrorEnvelope::new(code, message)
            .extension("serialized_bytes", serialized_bytes)
            .extension("max_serialized_bytes", max_serialized_bytes),
    )
}

fn timeout_error(kind: &'static str, timeout: Duration) -> (StatusCode, Json<Value>) {
    crate::diagnostics::record_conversation_timeout();
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(
            "conversation_snapshot_timeout",
            format!(
                "conversation {kind} snapshot exceeded the {} second budget",
                timeout.as_secs()
            ),
        )
        .retryable(true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AppConfig,
        host::RuntimeHost,
        provider::StubProvider,
        types::{
            AuditEvent, AuthorityClass, BriefKind, BriefRecord, ContinuationTriggerKind,
            MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
            QueueEntryStatus, ToolExecutionRecord, ToolExecutionStatus, TurnNoBriefReason,
            TurnRecord, TurnTerminalKind, TurnTerminalSummary, TurnTriggerSummary,
        },
    };
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use chrono::TimeZone;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    async fn test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        host.create_named_agent("web", None).await.unwrap();
        (home, host)
    }

    fn timestamp(offset: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0)
            .single()
            .expect("valid timestamp")
            + chrono::Duration::seconds(offset)
    }

    fn turn(turn_id: &str, turn_index: u64) -> TurnRecord {
        let mut record = TurnRecord::new("web", turn_id, turn_index);
        record.created_at = timestamp(i64::try_from(turn_index).unwrap());
        record.trigger = Some(TurnTriggerSummary {
            message_id: Some(format!("trigger-{turn_id}")),
            kind: MessageKind::OperatorPrompt,
            origin: MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            authority_class: AuthorityClass::OperatorInstruction,
            priority: Priority::Normal,
            trigger_kind: Some(ContinuationTriggerKind::OperatorInput),
            task_id: None,
        });
        record
    }

    fn terminal(mut record: TurnRecord, no_brief_reason: Option<TurnNoBriefReason>) -> TurnRecord {
        record.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason,
            completed_at: record.created_at + chrono::Duration::seconds(1),
            duration_ms: 1_000,
        });
        record
    }

    async fn get_json(state: AppState, uri: &str) -> (StatusCode, Value) {
        let response = crate::http::router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, value)
    }

    async fn post_locate(state: AppState, source_ref: &str, limit: usize) -> (StatusCode, Value) {
        let response = crate::http::router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents/web/conversation/locate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"source_ref": source_ref, "limit": limit}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn locate_exact_sources_and_round_trip_both_history_directions() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let state = AppState::for_tcp(host.clone());
        let (_, read_before) = get_json(state.clone(), "/api/agents/web/brief-read-state").await;
        let (status, located) = post_locate(state.clone(), "brief:brief-two", 1).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(located["status"], "located");
        assert_eq!(located["target"]["brief_id"], "brief-two");
        assert_eq!(located["target"]["turn_id"], "turn-multi-brief");
        assert_eq!(located["agent_id"], "web");
        assert_eq!(located["schema_version"], CONVERSATION_SCHEMA_VERSION);
        assert_eq!(located["query_version"], CONVERSATION_QUERY_VERSION);
        let window = &located["window"];
        assert_eq!(window["turns"].as_array().unwrap().len(), 1);
        assert_eq!(window["turns"][0]["turn_id"], "turn-multi-brief");
        assert_eq!(window["coverage"]["has_older"], true);
        assert_eq!(window["coverage"]["has_newer"], true);
        let before = window["next_before_cursor"].as_str().unwrap();
        let after = window["next_after_cursor"].as_str().unwrap();
        let (status, older) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?limit=1&before={before}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(older["turns"][0]["turn_id"], "turn-zero-brief");
        let older_after = older["next_after_cursor"].as_str().unwrap();
        let (_, back) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?limit=1&after={older_after}"),
        )
        .await;
        assert_eq!(back["turns"][0]["turn_id"], "turn-multi-brief");
        let (status, newer) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?limit=1&after={after}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(newer["turns"][0]["turn_id"], "turn-legacy");
        assert_eq!(newer["coverage"]["reaches_latest"], true);
        let (_, centered) = post_locate(state.clone(), "brief:brief-one", 3).await;
        assert_eq!(
            centered["window"]["turns"][1]["turn_id"],
            "turn-multi-brief"
        );
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE messages SET message_seq = 999999 WHERE evidence_id = 'message-active'",
                [],
            )
            .unwrap();
        let (status, input) = post_locate(state.clone(), "message:message-active", 1).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(input["target"]["kind"], "input_message");
        assert_eq!(input["target"]["turn_id"], "turn-active");
        assert!(input["target"]["activity_id"].is_string());
        assert_eq!(
            input["window"]["turns"][0]["inputs"][0]["message_id"],
            "message-active"
        );
        assert!(input["window"]["event_head_seq"].as_u64().unwrap() < 999999);
        let (_, read_after) = get_json(state.clone(), "/api/agents/web/brief-read-state").await;
        assert_eq!(read_before, read_after, "locate does not mark Briefs read");
        let (status, error) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?before={before}&after={after}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "conversation_invalid_request");
        let (status, error) = get_json(
            state,
            &format!("/api/agents/web/conversation?after={before}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "conversation_cursor_kind_mismatch");
    }

    #[tokio::test]
    async fn locate_materializes_truncated_input_in_distant_owner_with_canonical_order() {
        for with_activity in [true, false] {
            let (_home, host) = test_host().await;
            let db = host.runtime_db();
            let mut owner = turn("turn-many-inputs", 1);
            for index in 0..140 {
                let mut input = MessageEnvelope::new(
                    "web",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: Some("Canonical operator".into()),
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("canonical input {index}"),
                    },
                );
                input.id = format!("message-many-{index:03}");
                input.created_at = timestamp(1);
                db.evidence().append_message(&input).unwrap();
                owner.input_message_ids.push(input.id);
            }
            db.turn_records().upsert(&owner).unwrap();
            for index in 2..52 {
                db.turn_records()
                    .upsert(&terminal(turn(&format!("turn-later-{index}"), index), None))
                    .unwrap();
            }
            if !with_activity {
                db.connection()
                    .unwrap()
                    .execute(
                        "DELETE FROM conversation_source_revisions
                         WHERE agent_id = 'web' AND turn_id = 'turn-many-inputs'
                           AND source_kind = 'operator'",
                        [],
                    )
                    .unwrap();
            }
            let state = AppState::for_tcp(host);
            let (_, initial) = post_locate(state.clone(), "message:message-many-000", 1).await;
            let initial_inputs = initial["window"]["turns"][0]["inputs"].as_array().unwrap();
            assert_eq!(initial_inputs.len(), 128);
            assert_eq!(initial["target"]["message_id"], "message-many-000");
            assert_eq!(initial_inputs[0]["message_id"], "message-many-000");
            assert_eq!(initial["window"]["turns"][0]["inputs_truncated"], true);
            let (status, located) = post_locate(state, "message:message-many-139", 1).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(located["status"], "located");
            assert_eq!(located["target"]["kind"], "input_message");
            assert_eq!(located["target"]["turn_id"], "turn-many-inputs");
            assert_eq!(located["target"]["message_id"], "message-many-139");
            assert_eq!(located["target"]["activity_id"].is_string(), with_activity);
            let turns = located["window"]["turns"].as_array().unwrap();
            assert_eq!(turns.len(), 1, "seek directly to the distant owner");
            let inputs = turns[0]["inputs"].as_array().unwrap();
            assert_eq!(inputs.len(), 129, "bounded summaries plus exact target");
            assert_eq!(&inputs[..128], initial_inputs);
            assert_eq!(inputs[128]["message_id"], "message-many-139");
            let preview: Value =
                serde_json::from_str(inputs[128]["preview"].as_str().unwrap()).unwrap();
            assert_eq!(preview["text"], "canonical input 139");
            assert_eq!(inputs[128]["actor_display_name"], "Canonical operator");
            assert_eq!(inputs[128]["activity_key"].is_object(), with_activity);
            assert_eq!(turns[0]["inputs_truncated"], true);
            assert_eq!(located["window"]["coverage"]["has_older"], false);
            assert_eq!(located["window"]["coverage"]["has_newer"], true);
        }
    }

    #[tokio::test]
    async fn locate_materializes_pending_input_beyond_summary_cap() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let db = host.runtime_db();
        for index in 0..140 {
            let mut input = MessageEnvelope::new(
                "web",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: Some("Pending operator".into()),
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("pending input {index}"),
                },
            );
            input.id = format!("message-queued-{index:03}");
            input.created_at = timestamp(30);
            db.evidence().append_message(&input).unwrap();
            db.queue_entries()
                .upsert(&QueueEntryRecord {
                    message_id: input.id,
                    agent_id: "web".into(),
                    priority: Priority::Normal,
                    status: QueueEntryStatus::Queued,
                    created_at: timestamp(30),
                    updated_at: timestamp(30),
                })
                .unwrap();
        }
        let error = db
            .conversation()
            .summary_page("web", 1, None, None)
            .expect_err("ordinary summary keeps its strict pending cap");
        assert_eq!(
            error.downcast_ref::<ConversationReadError>(),
            Some(&ConversationReadError::CountLimitExceeded {
                resource: "pending inputs",
                limit: 100,
            })
        );
        let state = AppState::for_tcp(host.clone());
        for queue_status in ["queued", "dequeued"] {
            db.connection()
                .unwrap()
                .execute(
                    "UPDATE queue_entries SET status = ?1 WHERE message_id = 'message-queued-139'",
                    [queue_status],
                )
                .unwrap();
            let (status, located) =
                post_locate(state.clone(), "message:message-queued-139", 1).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(located["status"], "pending");
            assert_eq!(located["target"]["message_id"], "message-queued-139");
            assert!(located["target"]["turn_id"].is_null());
            let pending = located["window"]["pending_inputs"].as_array().unwrap();
            assert_eq!(pending.len(), 100);
            let target = pending.last().unwrap();
            assert_eq!(target["message_id"], "message-queued-139");
            assert_eq!(target["actor_display_name"], "Pending operator");
            let preview: Value = serde_json::from_str(target["preview"].as_str().unwrap()).unwrap();
            assert_eq!(preview["text"], "pending input 139");
            let keys: Vec<_> = pending
                .iter()
                .map(|input| {
                    (
                        input["created_at"].as_str().unwrap(),
                        input["message_id"].as_str().unwrap(),
                    )
                })
                .collect();
            assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[tokio::test]
    async fn locate_pending_unattributed_non_conversation_and_missing_sources_are_explicit() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let state = AppState::for_tcp(host.clone());
        let (status, pending) = post_locate(state.clone(), "message:message-pending", 1).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(pending["status"], "pending");
        assert!(pending["target"]["turn_id"].is_null());
        assert_eq!(
            pending["window"]["pending_inputs"][0]["message_id"],
            "message-pending"
        );
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE queue_entries SET status = 'dequeued' WHERE message_id = 'message-pending'",
                [],
            )
            .unwrap();
        let (_, claimed) = post_locate(state.clone(), "message:message-pending", 1).await;
        assert_eq!(claimed["status"], "pending");
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE queue_entries SET status = 'dropped' WHERE message_id = 'message-pending'",
                [],
            )
            .unwrap();
        let (_, dropped) = post_locate(state.clone(), "message:message-pending", 1).await;
        assert_eq!(dropped["status"], "source_only");
        assert_eq!(dropped["reason"], "unattributed_source");
        for source in ["brief:brief-legacy-unattributed", "work_item:work-example"] {
            let (status, body) = post_locate(state.clone(), source, 1).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["status"], "source_only");
            assert!(body["window"].is_null());
            assert!(body["runtime_id"].is_string());
        }
        let (status, body) = post_locate(state.clone(), "message:missing", 1).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "conversation_source_not_found");
        let (status, body) = post_locate(state.clone(), "message:../invalid", 1).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "conversation_invalid_request");
        let (status, body) = post_locate(state, "brief:brief-one", 0).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "conversation_invalid_limit");
    }

    #[tokio::test]
    async fn locate_forward_cursor_is_scope_bound_and_preserves_fixed_membership() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let state = AppState::for_tcp(host.clone());
        let (_, located) = post_locate(state.clone(), "brief:brief-one", 1).await;
        let after = located["window"]["next_after_cursor"].as_str().unwrap();
        host.runtime_db()
            .turn_records()
            .upsert(&turn("turn-later", 5))
            .unwrap();
        let (_, newer) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?after={after}"),
        )
        .await;
        assert_eq!(newer["turns"].as_array().unwrap().len(), 1);
        assert_eq!(newer["turns"][0]["turn_id"], "turn-legacy");
        host.runtime_db().connection().unwrap().execute(
            "UPDATE runtime_metadata SET value = CAST(value AS INTEGER) + 1 WHERE key = 'visibility_policy_generation'", [],
        ).unwrap();
        let (status, error) = get_json(
            state.clone(),
            &format!("/api/agents/web/conversation?after={after}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "conversation_cursor_scope_mismatch");
        let (_, located) = post_locate(state.clone(), "brief:brief-one", 1).await;
        let after = located["window"]["next_after_cursor"].as_str().unwrap();
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE runtime_metadata SET value = 'rotated-epoch' WHERE key = 'event_log_epoch'",
                [],
            )
            .unwrap();
        let (status, error) = get_json(
            state,
            &format!("/api/agents/web/conversation?after={after}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["code"],
            "conversation_cursor_event_log_epoch_mismatch"
        );
    }

    #[tokio::test]
    async fn locate_authorizes_before_resolving_and_hides_cross_agent_sources() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let mut state = AppState::for_tcp(host.clone());
        state.require_control_token = true;
        let (status, error) = post_locate(state, "brief:brief-one", 1).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error["code"], "auth_required");
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE briefs SET agent_id = 'other-agent' WHERE evidence_id = 'brief-one'",
                [],
            )
            .unwrap();
        let state = AppState::for_tcp(host.clone());
        let (status, error) = post_locate(state.clone(), "brief:brief-one", 1).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["code"], "conversation_source_not_found");
        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE agent_identities SET visibility = 'private' WHERE agent_id = 'web'",
                [],
            )
            .unwrap();
        let mut unauthenticated = state.clone();
        unauthenticated.require_control_token = true;
        let (status, _) = post_locate(unauthenticated, "brief:brief-two", 1).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, located) = post_locate(state, "brief:brief-two", 1).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(located["status"], "located");
    }

    #[tokio::test]
    async fn incarnation_binding_covers_http_conditional_reads_and_admission() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let state = AppState::for_tcp(host.clone());
        for path in [
            "/api/agents/web/conversation",
            "/api/agents/web/turns/turn-active/activities",
            "/api/agents/web/briefs/brief-one",
            "/api/agents/web/conversation/stream",
        ] {
            let response = crate::http::router(state.clone())
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("x-holon-expected-incarnation", "1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()["x-holon-agent-id"], "web");
            assert_eq!(response.headers()["x-holon-agent-incarnation"], "1");
            let etag = response.headers().get(ETAG).cloned();
            drop(response);
            if let Some(etag) = etag {
                let response = crate::http::router(state.clone())
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("x-holon-expected-incarnation", "1")
                            .header("if-none-match", etag.clone())
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{path}");
                assert_eq!(response.headers()["x-holon-agent-incarnation"], "1");
                let response = crate::http::router(state.clone())
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("x-holon-expected-incarnation", "2")
                            .header("if-none-match", etag)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::CONFLICT,
                    "304 cannot bypass admission"
                );
            }
            for expected in ["2", "invalid"] {
                let (status, _, body) = get_with_headers(
                    state.clone(),
                    path,
                    &[("x-holon-expected-incarnation", expected)],
                )
                .await;
                assert_eq!(status, StatusCode::CONFLICT, "{path}");
                assert_eq!(body["code"], "conversation_identity_changed");
                assert_eq!(body["retryable"], false);
            }
        }
        for expected in ["1", "2"] {
            let response = crate::http::router(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/agents/web/conversation/locate")
                        .header("content-type", "application/json")
                        .header("x-holon-expected-incarnation", expected)
                        .body(Body::from(r#"{"source_ref":"brief:brief-one","limit":1}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            if expected == "1" {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()["x-holon-agent-incarnation"], "1");
            } else {
                assert_eq!(response.status(), StatusCode::CONFLICT);
            }
        }
    }

    async fn get_with_headers(
        state: AppState,
        uri: &str,
        headers: &[(&'static str, &str)],
    ) -> (StatusCode, Option<String>, Value) {
        let mut builder = Request::builder().method("GET").uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = crate::http::router(state)
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, etag, value)
    }

    fn stream_batch() -> ConversationChangeBatch {
        ConversationChangeBatch {
            identity: ConversationIdentity {
                agent_id: "web".into(),
                agent_incarnation: 1,
            },
            runtime_id: "runtime-test".into(),
            event_log_epoch: "epoch-test".into(),
            visibility_scope_id: "scope-test".into(),
            from_seq: 10,
            through_seq: 12,
            oldest_retained_seq: 1,
            checkpoint: "checkpoint-test".into(),
            changes: vec![
                ConversationChange::OperatorRemove {
                    message_id: "message-finished".into(),
                    revision: 7,
                },
                ConversationChange::DetailInvalidated {
                    turn_id: "turn-finished".into(),
                    detail_revision: 9,
                },
            ],
        }
    }

    async fn render_events(events: Vec<Event>) -> String {
        let stream = tokio_stream::iter(
            events
                .into_iter()
                .map(Ok::<Event, std::convert::Infallible>),
        );
        let response = Sse::new(stream).into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    async fn read_through_checkpoint(response: AxumResponse) -> String {
        let mut stream = response.into_body().into_data_stream();
        let mut body = String::new();
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("conversation stream should produce a bounded initial batch")
                .expect("conversation stream should remain open through its checkpoint")
                .expect("conversation stream body should be readable");
            body.push_str(std::str::from_utf8(&chunk).unwrap());
            if body.contains("event: checkpoint") {
                return body;
            }
        }
    }

    fn seed_conversation(host: &RuntimeHost) {
        let db = host.runtime_db();
        let mut input = MessageEnvelope::new(
            "web",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "show the current state".into(),
            },
        );
        input.id = "message-active".into();
        input.created_at = timestamp(1);
        db.evidence().append_message(&input).unwrap();

        let mut active = turn("turn-active", 1);
        active.input_message_ids = vec![input.id.clone()];
        active.tool_execution_ids = vec!["tool-active".into()];
        db.turn_records().upsert(&active).unwrap();
        db.evidence()
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-active".into(),
                agent_id: "web".into(),
                work_item_id: None,
                turn_index: 1,
                turn_id: Some("turn-active".into()),
                tool_name: "ExecCommand".into(),
                created_at: timestamp(2),
                completed_at: Some(timestamp(3)),
                duration_ms: 1,
                authority_class: AuthorityClass::RuntimeInstruction,
                status: ToolExecutionStatus::Success,
                input: serde_json::json!({
                    "cmd": "cat secret",
                    "workspace_secret": "must-not-leak"
                }),
                output: serde_json::json!({
                    "artifact": {"path": "must-not-leak"},
                    "workspace": {"root": "must-not-leak"}
                }),
                summary: "inspected state".into(),
                invocation_surface: None,
            })
            .unwrap();

        db.turn_records()
            .upsert(&terminal(
                turn("turn-zero-brief", 2),
                Some(TurnNoBriefReason::ToolOnlyWait),
            ))
            .unwrap();
        db.turn_records()
            .upsert(&terminal(turn("turn-multi-brief", 3), None))
            .unwrap();
        let mut legacy_turn = TurnRecord::new("web", "turn-legacy", 4);
        legacy_turn.created_at = timestamp(4);
        db.turn_records().upsert(&legacy_turn).unwrap();
        for (id, preview, offset) in [
            ("brief-one", "first result", 10),
            ("brief-two", "second result", 11),
        ] {
            let mut brief = BriefRecord::new("web", BriefKind::Result, preview, None, None);
            brief.id = id.into();
            brief.turn_id = Some("turn-multi-brief".into());
            brief.turn_index = Some(3);
            brief.created_at = timestamp(offset);
            db.evidence().append_brief(&brief).unwrap();
        }
        let mut legacy = BriefRecord::new(
            "web",
            BriefKind::Result,
            "legacy unattributed result",
            None,
            None,
        );
        legacy.id = "brief-legacy-unattributed".into();
        legacy.created_at = timestamp(12);
        db.evidence().append_brief(&legacy).unwrap();
        let mut pending = MessageEnvelope::new(
            "web",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued operator echo".into(),
            },
        );
        pending.id = "message-pending".into();
        pending.created_at = timestamp(20);
        db.evidence().append_message(&pending).unwrap();
        db.queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: "message-pending".into(),
                agent_id: "web".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: timestamp(20),
                updated_at: timestamp(20),
            })
            .unwrap();
    }

    #[test]
    fn openapi_registers_conversation_routes_and_schemas() {
        let api = crate::openapi::generate_openapi_json();
        let schemas = api["components"]["schemas"].as_object().unwrap();
        for name in [
            "ConversationReadQuery",
            "ConversationSummaryResponse",
            "ConversationActivityResponse",
            "ConversationShadowDiagnostics",
        ] {
            assert!(schemas.contains_key(name), "missing schema {name}");
        }
        assert_eq!(
            api["paths"]["/api/agents/{agent_id}/conversation"]["get"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationSummaryResponse"
        );
        assert_eq!(
            api["paths"]["/api/agents/{agent_id}/turns/{turn_id}/activities"]["get"]["responses"]
                ["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationActivityResponse"
        );
        assert_eq!(
            api["paths"]["/api/control/agents/{agent_id}/conversation/shadow-diagnostics"]["get"]
                ["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationShadowDiagnostics"
        );
        let summary_parameters = api["paths"]["/api/agents/{agent_id}/conversation"]["get"]
            ["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(
            api["paths"]["/api/agents/{agent_id}/conversation/locate"]["post"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationLocateResponse"
        );
        assert!(
            api["components"]["schemas"]["ConversationLocateRequest"]["properties"]
                .get("source_ref")
                .is_some()
        );
        assert_eq!(
            summary_parameters
                .iter()
                .map(|parameter| parameter["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["agent_id", "limit", "before", "after"]
        );
        let activity_parameters = api["paths"]["/api/agents/{agent_id}/turns/{turn_id}/activities"]
            ["get"]["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(
            activity_parameters
                .iter()
                .map(|parameter| parameter["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["agent_id", "turn_id", "limit", "before"]
        );
        let shadow_parameters = api["paths"]
            ["/api/control/agents/{agent_id}/conversation/shadow-diagnostics"]["get"]["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(
            shadow_parameters
                .iter()
                .map(|parameter| parameter["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["agent_id", "turn_limit"]
        );
        assert_eq!(shadow_parameters[1]["in"], "query");
        assert_eq!(shadow_parameters[1]["required"], false);
        assert_eq!(shadow_parameters[1]["schema"]["minimum"], 1);
        assert_eq!(shadow_parameters[1]["schema"]["maximum"], 100);
        assert_eq!(shadow_parameters[1]["schema"]["default"], 30);
    }

    #[test]
    fn detail_coverage_serializes_all_public_states() {
        assert_eq!(
            serde_json::to_value(DetailCoverage::Complete).unwrap(),
            serde_json::json!({"kind": "complete"})
        );
        assert_eq!(
            serde_json::to_value(DetailCoverage::Partial {
                reason: crate::domain::conversation::DetailCoverageReason::RetentionGap,
            })
            .unwrap(),
            serde_json::json!({"kind": "partial", "reason": "retention_gap"})
        );
        assert_eq!(
            serde_json::to_value(DetailCoverage::Unknown).unwrap(),
            serde_json::json!({"kind": "unknown"})
        );
    }

    #[tokio::test]
    async fn oidc_conversation_scopes_match_roster_across_users_and_rotation() {
        let (home, _) = test_host().await;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        config.auth.mode = crate::authentication::AuthenticationMode::Oidc;
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        seed_conversation(&host);
        let now = chrono::Utc::now();
        for (user_id, credential) in [
            ("user-a", "session-a"),
            ("user-b", "session-b"),
            ("user-a", "rotated-session-a"),
        ] {
            let authentication = host.runtime_db().authentication();
            authentication
                .upsert_user(&crate::authentication::AuthUserRecord {
                    user_id: user_id.to_string(),
                    issuer: "https://issuer.example".to_string(),
                    subject: user_id.to_string(),
                    display_name: None,
                    email: None,
                    created_at: now,
                    updated_at: now,
                    disabled_at: None,
                })
                .unwrap();
            authentication
                .create_session(&crate::authentication::AuthSessionRecord {
                    session_digest: crate::authentication::digest_secret(credential),
                    user_id: user_id.to_string(),
                    auth_method: "oidc".to_string(),
                    created_at: now,
                    expires_at: None,
                    last_seen_at: now,
                    revoked_at: None,
                })
                .unwrap();
        }
        let state = AppState::for_tcp(host);
        let mut scopes = Vec::new();
        for (header, authorization) in [
            ("authorization", "Bearer session-a"),
            ("authorization", "Bearer session-b"),
            ("authorization", "Bearer rotated-session-a"),
            ("cookie", "holon_session=session-a"),
        ] {
            let headers = [(header, authorization)];
            let (status, _, roster) =
                get_with_headers(state.clone(), "/api/agents/snapshot", &headers).await;
            assert_eq!(status, StatusCode::OK);
            let scope = roster["visibility_scope_id"].as_str().unwrap();
            for uri in [
                "/api/agents/web/conversation",
                "/api/agents/web/turns/turn-active/activities",
                "/api/control/agents/web/conversation/shadow-diagnostics",
            ] {
                let (status, _, body) = get_with_headers(state.clone(), uri, &headers).await;
                assert_eq!(status, StatusCode::OK, "{uri}");
                assert_eq!(body["visibility_scope_id"], scope, "{uri}");
            }
            let response = crate::http::router(state.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/agents/web/conversation/stream")
                        .header(header, authorization)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let stream = read_through_checkpoint(response).await;
            let stream_scopes: Vec<_> = stream
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .map(|data| serde_json::from_str::<Value>(data).unwrap())
                .filter_map(|event| event["visibility_scope_id"].as_str().map(str::to_owned))
                .collect();
            assert!(
                stream_scopes.len() >= 2,
                "batch begin and checkpoint carry scope"
            );
            assert!(stream_scopes
                .iter()
                .all(|stream_scope| stream_scope == scope));
            scopes.push(scope.to_owned());
        }
        assert_ne!(scopes[0], scopes[1]);
        assert_eq!(scopes[0], scopes[2]);
        assert_eq!(scopes[0], scopes[3]);
    }

    #[tokio::test]
    async fn summary_and_brief_honor_if_none_match() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let state = AppState::for_tcp(host);

        // Summary: 200 with a strong content-addressed ETag, then 304.
        let (status, etag, body) =
            get_with_headers(state.clone(), "/api/agents/web/conversation", &[]).await;
        assert_eq!(status, StatusCode::OK);
        let etag = etag.expect("summary response carries an ETag");
        assert!(etag.starts_with('"') && etag.ends_with('"'));
        assert!(body.get("turns").is_some());
        let (status, repeat_etag, body) = get_with_headers(
            state.clone(),
            "/api/agents/web/conversation",
            &[("if-none-match", etag.as_str())],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert_eq!(repeat_etag.as_deref(), Some(etag.as_str()));
        assert_eq!(body, Value::Null, "304 responses carry no body");

        // RFC 9110 weak comparison: W/"etag" also satisfies If-None-Match.
        let weak_echo = format!("W/{etag}");
        let (status, _, _) = get_with_headers(
            state.clone(),
            "/api/agents/web/conversation",
            &[("if-none-match", weak_echo.as_str())],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);

        // A different page (limit) is different content and a different tag.
        let (status, limited_etag, _) =
            get_with_headers(state.clone(), "/api/agents/web/conversation?limit=1", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(limited_etag.as_deref(), Some(etag.as_str()));

        // Brief: same conditional behavior on the compatibility endpoint.
        let (status, brief_etag, _) =
            get_with_headers(state.clone(), "/api/agents/web/briefs/brief-one", &[]).await;
        assert_eq!(status, StatusCode::OK);
        let brief_etag = brief_etag.expect("brief response carries an ETag");
        let (status, _, _) = get_with_headers(
            state.clone(),
            "/api/agents/web/briefs/brief-one",
            &[("if-none-match", brief_etag.as_str())],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);

        // A non-matching validator still returns the full payload.
        let (status, _, body) = get_with_headers(
            state.clone(),
            "/api/agents/web/conversation",
            &[("if-none-match", "\"stale-etag\"")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("turns").is_some());
    }

    #[tokio::test]
    async fn summary_and_activity_serve_bounded_safe_snapshots() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);

        let (status, summary) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/conversation",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(summary["snapshot_through_seq"], summary["event_head_seq"]);
        assert!(summary["snapshot_cursor"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            summary["pending_inputs"][0]["message_id"],
            "message-pending"
        );
        assert!(
            summary["pending_inputs"][0]["preview"]
                .as_str()
                .is_some_and(|value| value.contains("queued operator echo")),
            "pending input should expose a text preview"
        );
        let turns = summary["turns"].as_array().unwrap();
        let multi = turns
            .iter()
            .find(|turn| turn["turn_id"] == "turn-multi-brief")
            .unwrap();
        assert_eq!(
            multi["brief_ids"],
            serde_json::json!(["brief-one", "brief-two"])
        );
        assert_eq!(multi["result"]["kind"], "available");
        let zero = turns
            .iter()
            .find(|turn| turn["turn_id"] == "turn-zero-brief")
            .unwrap();
        assert_eq!(zero["result"]["kind"], "none");
        assert_eq!(zero["result"]["reason"]["kind"], "tool_only_wait");
        let legacy = turns
            .iter()
            .find(|turn| turn["turn_id"] == "turn-legacy")
            .unwrap();
        assert_eq!(legacy["detail_coverage"]["kind"], "partial");
        assert_eq!(legacy["detail_coverage"]["reason"], "legacy_ownership");
        assert!(!summary.to_string().contains("brief-legacy-unattributed"));

        let (status, activity) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/turns/turn-active/activities",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(activity["snapshot_through_seq"], activity["event_head_seq"]);
        assert_eq!(activity["coverage"]["kind"], "complete");
        assert_eq!(activity["activities"].as_array().unwrap().len(), 2);
        let serialized = activity.to_string();
        assert!(!serialized.contains("workspace_secret"));
        assert!(!serialized.contains("must-not-leak"));
        assert!(!serialized.contains("\"input\""));
        assert!(!serialized.contains("\"output\""));

        let (status, legacy_briefs) =
            get_json(AppState::for_tcp(host), "/api/agents/web/briefs?limit=20").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            legacy_briefs
                .to_string()
                .contains("brief-legacy-unattributed"),
            "unattributed legacy briefs remain reachable through the compatibility surface"
        );
    }

    #[tokio::test]
    async fn shadow_diagnostics_are_bounded_and_metadata_only() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);

        let (status, body) = get_json(
            AppState::for_tcp(host),
            "/api/control/agents/web/conversation/shadow-diagnostics?turn_limit=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["schema_version"], CONVERSATION_SCHEMA_VERSION);
        assert_eq!(body["query_version"], CONVERSATION_QUERY_VERSION);
        assert_eq!(body["checked_turn_limit"], 2);
        assert_eq!(body["mismatch_count"], 0);
        assert_eq!(body["legacy_unattributed_briefs"], 1);
        assert!(body["canonical"]["turns"].as_u64().unwrap() <= 2);
        assert!(body["projection"]["turns"].as_u64().unwrap() <= 2);

        let serialized = body.to_string();
        assert!(!serialized.contains("show the current state"));
        assert!(!serialized.contains("workspace_secret"));
        assert!(!serialized.contains("must-not-leak"));
        assert!(!serialized.contains("first result"));
        assert!(!serialized.contains("second result"));
        assert!(!serialized.contains("legacy unattributed result"));
    }

    #[tokio::test]
    async fn conversation_routes_authorize_without_diagnostic_gate() {
        let (_home, host) = test_host().await;
        let mut state = AppState::for_tcp(host.clone());
        state.require_control_token = true;
        let (status, body) = get_json(state, "/api/agents/web/conversation").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "auth_required");

        let mut state = AppState::for_tcp(host.clone());
        state.require_control_token = true;
        let (status, body) = get_json(
            state,
            "/api/control/agents/web/conversation/shadow-diagnostics",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "auth_required");

        host.runtime_db()
            .connection()
            .unwrap()
            .execute(
                "UPDATE observer_sync_capability_verifications
                 SET verified = 0 WHERE capability = 'conversation_read_verified'",
                [],
            )
            .unwrap();
        let (status, body) =
            get_json(AppState::for_tcp(host), "/api/agents/web/conversation").await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn conversation_cursor_failures_are_typed() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let (status, snapshot) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/conversation",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/conversation?before=not-a-cursor",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "conversation_cursor_malformed");

        let stream_cursor = snapshot["snapshot_cursor"].as_str().unwrap();
        let (status, body) = get_json(
            AppState::for_tcp(host.clone()),
            &format!("/api/agents/web/conversation?before={stream_cursor}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "conversation_cursor_kind_mismatch");

        let signing_key: String = host
            .runtime_db()
            .connection()
            .unwrap()
            .query_row(
                "SELECT value FROM runtime_metadata
                 WHERE key = 'conversation_cursor_signing_key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let binding = crate::domain::conversation::CursorBinding {
            runtime_id: snapshot["runtime_id"].as_str().unwrap().into(),
            agent_id: "web".into(),
            agent_incarnation: 1,
            event_log_epoch: snapshot["event_log_epoch"].as_str().unwrap().into(),
            visibility_scope_id: snapshot["visibility_scope_id"].as_str().unwrap().into(),
            schema_version: u32::try_from(snapshot["schema_version"].as_u64().unwrap()).unwrap(),
            query_version: u32::try_from(snapshot["query_version"].as_u64().unwrap()).unwrap(),
        };
        let outside_coverage = crate::domain::conversation::CursorCodec::new(
            signing_key.as_bytes(),
        )
        .encode(&crate::domain::conversation::HistoryCursor {
            binding,
            before: crate::domain::conversation::TurnKey {
                turn_index: 2,
                turn_id: "after-boundary".into(),
            },
            membership_upper_bound: crate::domain::conversation::TurnKey {
                turn_index: 1,
                turn_id: "boundary".into(),
            },
        });
        let (status, body) = get_json(
            AppState::for_tcp(host),
            &format!("/api/agents/web/conversation?before={outside_coverage}"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conversation_cursor_outside_coverage");
    }

    #[tokio::test]
    async fn conversation_routes_return_typed_limit_errors() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);

        let (status, body) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/conversation?limit=0",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "conversation_invalid_limit");

        let mut state = AppState::for_tcp(host.clone());
        state.conversation_read_limits.max_turn_serialized_bytes = 1;
        let (status, body) = get_json(state, "/api/agents/web/conversation").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["code"], "conversation_turn_too_large");

        let mut state = AppState::for_tcp(host.clone());
        state.conversation_read_limits.max_summary_serialized_bytes = 1;
        let (status, body) = get_json(state, "/api/agents/web/conversation").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["code"], "conversation_snapshot_too_large");

        let mut state = AppState::for_tcp(host.clone());
        state
            .conversation_read_limits
            .max_activity_item_serialized_bytes = 1;
        let (status, body) = get_json(state, "/api/agents/web/turns/turn-active/activities").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["code"], "conversation_activity_too_large");

        let mut state = AppState::for_tcp(host.clone());
        state.conversation_read_limits.max_activity_serialized_bytes = 1;
        let (status, body) = get_json(state, "/api/agents/web/turns/turn-active/activities").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["code"], "conversation_activity_page_too_large");

        let mut state = AppState::for_tcp(host);
        state.conversation_read_limits.timeout = Duration::ZERO;
        let (status, body) = get_json(state, "/api/agents/web/conversation").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "conversation_snapshot_timeout");
    }

    #[tokio::test]
    async fn conversation_stream_frames_checkpoint_after_the_complete_batch() {
        let batch = stream_batch();
        let checkpoint = batch.checkpoint.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let tx = BoundStreamSender {
            tx,
            identity: stream_batch().identity,
            identity_bound: true,
        };
        assert!(send_change_batch(&tx, batch).await);
        drop(tx);

        let response = Sse::new(ReceiverStream::new(rx)).into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        let batch_begin = body.find("event: batch_begin").unwrap();
        let operator_remove = body.find("event: operator_remove").unwrap();
        let detail_invalidated = body.find("event: detail_invalidated").unwrap();
        let checkpoint_event = body.find("event: checkpoint").unwrap();
        assert!(batch_begin < operator_remove);
        assert!(operator_remove < detail_invalidated);
        assert!(detail_invalidated < checkpoint_event);
        assert_eq!(body.matches("id: ").count(), 1);
        assert!(body.contains(&format!("id: {checkpoint}\n")));
    }

    #[tokio::test]
    async fn conversation_stream_disconnect_before_checkpoint_exposes_no_resumable_id() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let tx = BoundStreamSender {
            tx,
            identity: stream_batch().identity,
            identity_bound: true,
        };
        let sender = tokio::spawn(async move { send_change_batch(&tx, stream_batch()).await });
        let first = rx
            .recv()
            .await
            .expect("batch_begin should be queued")
            .expect("batch_begin should serialize");
        drop(rx);
        assert!(!sender.await.unwrap());

        let body = render_events(vec![first]).await;
        assert!(body.contains("event: batch_begin"));
        assert!(!body.contains("event: checkpoint"));
        assert!(!body.contains("id: "));
    }

    #[tokio::test]
    async fn conversation_stream_identity_reset_preserves_legacy_compatibility() {
        for (identity_bound, reason) in
            [(false, "agent_not_found"), (true, "agent_identity_changed")]
        {
            for source in ["missing", "replaced", "mismatched_batch"] {
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                let tx = BoundStreamSender {
                    tx,
                    identity: stream_batch().identity,
                    identity_bound,
                };
                match source {
                    "missing" => {
                        assert!(
                            send_stream_message(&tx, identity_changed_reset(identity_bound), None)
                                .await
                        );
                    }
                    "replaced" => {
                        let error = anyhow::Error::new(ConversationReadError::IdentityChanged);
                        assert!(
                            send_stream_message(
                                &tx,
                                reset_message_for_error(&error, identity_bound),
                                None,
                            )
                            .await
                        );
                    }
                    _ => {
                        let mut batch = stream_batch();
                        batch.identity.agent_incarnation += 1;
                        assert!(!send_change_batch(&tx, batch).await);
                    }
                }
                drop(tx);
                let response = Sse::new(ReceiverStream::new(rx)).into_response();
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                let body = String::from_utf8(body.to_vec()).unwrap();
                assert_eq!(body.matches("event: reset_required").count(), 1, "{source}");
                assert!(body.contains(&format!("\"reason\":\"{reason}\"")), "{body}");
                assert!(body.contains("\"agent_incarnation\":1"), "{body}");
                assert!(!body.contains("event: batch_begin"), "{body}");
                assert!(!body.contains("id: "), "{body}");
            }
        }
    }

    #[tokio::test]
    async fn conversation_stream_idle_wait_ends_when_client_disconnects() {
        let (_home, host) = test_host().await;
        let mut live_rx = host.subscribe_events();
        let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<Event, anyhow::Error>>(1);
        drop(rx);

        let result = tokio::time::timeout(Duration::from_millis(100), async {
            tokio::select! {
                _ = tx.closed() => None,
                published = live_rx.recv() => Some(published),
            }
        })
        .await
        .expect("idle stream wait should notice the disconnected client");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn conversation_stream_recovery_read_obeys_timeout() {
        let started = std::time::Instant::now();
        let result = bounded_blocking_read(Duration::from_millis(10), || {
            std::thread::sleep(Duration::from_millis(100));
            1
        })
        .await;

        assert!(matches!(result, Err(BoundedBlockingReadError::Timeout)));
        assert!(started.elapsed() < Duration::from_millis(80));
    }

    #[tokio::test]
    async fn conversation_stream_slow_consumer_hits_the_bounded_send_timeout() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let tx = BoundStreamSender {
            tx,
            identity: stream_batch().identity,
            identity_bound: true,
        };
        let started = std::time::Instant::now();
        assert!(
            !send_change_batch_with_timeout(&tx, stream_batch(), Duration::from_millis(20)).await
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn conversation_stream_prefers_last_event_id_and_returns_typed_resets() {
        let (_home, host) = test_host().await;
        seed_conversation(&host);
        let (status, snapshot) = get_json(
            AppState::for_tcp(host.clone()),
            "/api/agents/web/conversation",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let snapshot_cursor = snapshot["snapshot_cursor"].as_str().unwrap();

        host.runtime_db()
            .turn_records()
            .upsert(&turn("turn-after-snapshot", 99))
            .unwrap();
        let mut event = AuditEvent::legacy(
            "conversation_test_change",
            serde_json::json!({ "turn_id": "turn-after-snapshot" }),
        );
        event.id = "event-after-snapshot".into();
        event.created_at = timestamp(99);
        host.runtime_db()
            .audit_events()
            .append(Some("web"), &event)
            .unwrap();
        let response = crate::http::router(AppState::for_tcp(host.clone()))
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/agents/web/conversation/stream?after=not-a-cursor")
                    .header("last-event-id", snapshot_cursor)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"));
        let body = read_through_checkpoint(response).await;
        assert!(body.contains("event: batch_begin"));
        assert!(body.contains("turn-after-snapshot"));
        assert!(body.contains("event: checkpoint"));

        let signing_key: String = host
            .runtime_db()
            .connection()
            .unwrap()
            .query_row(
                "SELECT value FROM runtime_metadata
                 WHERE key = 'conversation_cursor_signing_key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut binding = crate::domain::conversation::CursorBinding {
            runtime_id: snapshot["runtime_id"].as_str().unwrap().into(),
            agent_id: "web".into(),
            agent_incarnation: 1,
            event_log_epoch: snapshot["event_log_epoch"].as_str().unwrap().into(),
            visibility_scope_id: snapshot["visibility_scope_id"].as_str().unwrap().into(),
            schema_version: u32::try_from(snapshot["schema_version"].as_u64().unwrap()).unwrap(),
            query_version: u32::try_from(snapshot["query_version"].as_u64().unwrap()).unwrap(),
        };
        binding.query_version += 1;
        let stale_cursor = crate::domain::conversation::CursorCodec::new(signing_key.as_bytes())
            .encode(&crate::domain::conversation::StreamCursor {
                binding,
                event_seq: snapshot["event_head_seq"].as_u64().unwrap(),
            });
        let (status, body) = get_json(
            AppState::for_tcp(host),
            &format!("/api/agents/web/conversation/stream?after={stale_cursor}"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conversation_reset_required");
        assert_eq!(body["reason"], "query_version_mismatch");
    }
}
