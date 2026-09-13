//! Bounded conversation snapshot HTTP reads.

use super::*;
use crate::domain::conversation::{
    ConversationActivity, ConversationTurnSummary, DetailCoverage, PendingInput,
    CONVERSATION_QUERY_VERSION, CONVERSATION_SCHEMA_VERSION,
};
use crate::runtime_db::conversation::{ConversationReadError, ConversationSnapshot};

pub(crate) const CONVERSATION_SUMMARY_DEFAULT_LIMIT: usize = 30;
pub(crate) const CONVERSATION_ACTIVITY_DEFAULT_LIMIT: usize = 50;
pub(crate) const CONVERSATION_TURN_MAX_SERIALIZED_BYTES: usize = 64 * 1024;
pub(crate) const CONVERSATION_ACTIVITY_ITEM_MAX_SERIALIZED_BYTES: usize = 256 * 1024;
pub(crate) const CONVERSATION_SUMMARY_MAX_SERIALIZED_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const CONVERSATION_ACTIVITY_MAX_SERIALIZED_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const CONVERSATION_READ_TIMEOUT: Duration = Duration::from_secs(10);

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

pub async fn summary(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationReadQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    if !conversation_capability_available(&state) {
        return capability_unavailable().into_response();
    }
    let limits = state.conversation_read_limits.clone();
    let limit = query.limit.unwrap_or(CONVERSATION_SUMMARY_DEFAULT_LIMIT);
    let before = query.before;
    let host = state.host.clone();
    let (scope_principal, scope_entitlement) = observer_sync::observer_scope_authority(&state);
    let snapshot = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db().conversation().summary_snapshot(
                &agent_id,
                limit,
                before.as_deref(),
                scope_principal,
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
    traced_json_bytes("/agents/{agent_id}/conversation", started_at, bytes)
}

pub async fn activities(
    Path((agent_id, turn_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ConversationReadQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    if !conversation_capability_available(&state) {
        return capability_unavailable().into_response();
    }
    let limits = state.conversation_read_limits.clone();
    let limit = query.limit.unwrap_or(CONVERSATION_ACTIVITY_DEFAULT_LIMIT);
    let before = query.before;
    let host = state.host.clone();
    let (scope_principal, scope_entitlement) = observer_sync::observer_scope_authority(&state);
    let snapshot = match tokio::time::timeout(
        limits.timeout,
        tokio::task::spawn_blocking(move || {
            host.runtime_db().conversation().activity_snapshot(
                &agent_id,
                &turn_id,
                limit,
                before.as_deref(),
                scope_principal,
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
    let Some(response) = activity_response(snapshot) else {
        return http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new("no conversation turn for this request")
                .code("conversation_turn_not_found"),
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
    traced_json_bytes(
        "/agents/{agent_id}/turns/{turn_id}/activities",
        started_at,
        bytes,
    )
}

fn summary_response(
    snapshot: ConversationSnapshot<crate::domain::conversation::ConversationSummaryPage>,
) -> ConversationSummaryResponse {
    let page = snapshot.value;
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

fn conversation_capability_available(state: &AppState) -> bool {
    advertised_observer_sync_capabilities(&load_observer_sync_verification(state))
        .contains(&observer_sync::CONVERSATION_READ_CAPABILITY)
}

fn capability_unavailable() -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(
            "the agents.conversation-read.v1 capability is not verified for this database",
        )
        .code("capability_unavailable")
        .hint(
            "see the handshake capabilities; route registration alone never serves this contract",
        ),
    )
}

fn agent_not_found() -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new("no accessible Agent for this request").code("agent_not_found"),
    )
}

fn conversation_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let Some(error) = error.downcast_ref::<ConversationReadError>() else {
        return error_response(error);
    };
    match error {
        ConversationReadError::InvalidLimit {
            resource,
            minimum,
            maximum,
            actual,
        } => http_error(
            StatusCode::BAD_REQUEST,
            HttpErrorEnvelope::new(error.to_string())
                .code("conversation_invalid_limit")
                .extension("resource", *resource)
                .extension("minimum", *minimum)
                .extension("maximum", *maximum)
                .extension("actual", *actual),
        ),
        ConversationReadError::CountLimitExceeded { resource, limit } => http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            HttpErrorEnvelope::new(error.to_string())
                .code("conversation_count_limit_exceeded")
                .extension("resource", *resource)
                .extension("limit", *limit),
        ),
        ConversationReadError::CursorOutsideCoverage => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(error.to_string())
                .code("conversation_cursor_outside_coverage")
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
                crate::domain::conversation::CursorDecodeError::SchemaVersionMismatch {
                    ..
                } => "conversation_cursor_schema_version_mismatch",
                crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. } => {
                    "conversation_cursor_query_version_mismatch"
                }
            };
            http_error(
                StatusCode::BAD_REQUEST,
                HttpErrorEnvelope::new(error.to_string()).code(code),
            )
        }
    }
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
            return Some(http_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                HttpErrorEnvelope::new("conversation turn exceeds the maximum serialized size")
                    .code("conversation_turn_too_large")
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
    http_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        HttpErrorEnvelope::new(message)
            .code(code)
            .extension("serialized_bytes", serialized_bytes)
            .extension("max_serialized_bytes", max_serialized_bytes),
    )
}

fn timeout_error(kind: &'static str, timeout: Duration) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(format!(
            "conversation {kind} snapshot exceeded the {} second budget",
            timeout.as_secs()
        ))
        .code("conversation_snapshot_timeout")
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
            AuthorityClass, BriefKind, BriefRecord, ContinuationTriggerKind, MessageBody,
            MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
            QueueEntryStatus, ToolExecutionRecord, ToolExecutionStatus, TurnNoBriefReason,
            TurnRecord, TurnTerminalKind, TurnTerminalSummary, TurnTriggerSummary,
        },
    };
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use chrono::TimeZone;
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
        let summary_parameters = api["paths"]["/api/agents/{agent_id}/conversation"]["get"]
            ["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(
            summary_parameters
                .iter()
                .map(|parameter| parameter["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["agent_id", "limit", "before"]
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
    async fn conversation_routes_authorize_and_gate_capability() {
        let (_home, host) = test_host().await;
        let mut state = AppState::for_tcp(host.clone());
        state.require_control_token = true;
        let (status, body) = get_json(state, "/api/agents/web/conversation").await;
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
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "capability_unavailable");
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
}
