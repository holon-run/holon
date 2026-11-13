//! Request-level regression tests for TUI conversation/state sync (#4009).
//!
//! These tests drive a real `LocalClient` against a real local Axum server and
//! a real `TuiApp` through `initialize`/`tick`/switch/refresh/scroll-up flows.
//! The server pins the sync contract:
//!
//! * bootstrap reads `/state` only and never pages `/events`;
//! * normal pages never open the raw `/events/stream` diagnostics stream;
//! * the conversation snapshot/stream/history endpoints are the only
//!   conversation source;
//! * committed checkpoints, readiness and reconnects hint coalesced `/state`
//!   refreshes, with bounded selected-agent polling as a fallback.

use super::*;
use crate::http_dto::{AgentStateSnapshotDto, SlimAgentDto, StateSessionSnapshotDto};
use axum::{
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, Sse},
        IntoResponse,
    },
    Json,
};
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt};

#[path = "diagnostics_sync.rs"]
mod diagnostics_sync;

fn roster(ids: &[&str]) -> serde_json::Value {
    json!({
        "contract_version": 1,
        "runtime_id": "test-runtime",
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "agents": ids.iter().map(|id| json!({
            "agent": AgentListEntry::from_summary(&sample_agent_summary(id)),
            "event_window": {"event_head_seq": 0, "oldest_retained_seq": 0},
            "latest_brief": null,
        })).collect::<Vec<_>>(),
    })
}

fn compact_status(id: &str) -> serde_json::Value {
    serde_json::to_value(SlimAgentDto::from(&sample_agent_summary(id))).unwrap()
}

fn state_snapshot(id: &str) -> serde_json::Value {
    serde_json::to_value::<AgentStateSnapshotDto>(AgentStateSnapshotDto {
        agent: SlimAgentDto::from(&sample_agent_summary(id)),
        session: StateSessionSnapshotDto {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: vec![],
        waits: vec![],
        timers: vec![],
        work_items: vec![],
        active_work_item_count: 0,
        active_timer_count: 0,
        external_triggers: vec![],
        workspace: Default::default(),
    })
    .unwrap()
}

fn turn_value(
    id: &str,
    turn_index: u64,
    revision: u64,
    execution: serde_json::Value,
) -> serde_json::Value {
    json!({
        "turn_id": id,
        "key": {"turn_index": turn_index, "turn_id": id},
        "revision": revision,
        "presentation_class": "operator",
        "inputs": [{"message_id": format!("input-{id}"), "preview": "operator prompt"}],
        "execution": execution,
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": null,
        "duration_ms": null,
        "result": {"kind": "pending"},
        "settled": false,
        "attention": null,
        "detail_coverage": {"kind": "complete"},
        "brief_ids": [],
    })
}

fn terminal_turn(id: &str, turn_index: u64) -> serde_json::Value {
    turn_value(
        id,
        turn_index,
        1,
        json!({"kind": "terminal", "outcome": "completed"}),
    )
}

fn conversation_first_page() -> serde_json::Value {
    json!({
        "schema_version": 2,
        "query_version": 2,
        "runtime_id": "test-runtime",
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "snapshot_through_seq": 10,
        "snapshot_cursor": "snapshot-10",
        "turns": [terminal_turn("t1", 1)],
        "active_turns": [],
        "pending_inputs": [],
        "next_before_cursor": "before-2",
        "has_more": true,
    })
}

fn conversation_older_page() -> serde_json::Value {
    json!({
        "schema_version": 2,
        "query_version": 2,
        "runtime_id": "test-runtime",
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "snapshot_through_seq": 10,
        "snapshot_cursor": "snapshot-10",
        "turns": [terminal_turn("t0", 0)],
        "active_turns": [],
        "pending_inputs": [],
        "next_before_cursor": null,
        "has_more": false,
    })
}

fn batch_begin(from_seq: u64, through_seq: u64) -> serde_json::Value {
    json!({
        "type": "batch_begin",
        "batch_id": format!("batch-{through_seq}"),
        "schema_version": 2,
        "query_version": 2,
        "runtime_id": "test-runtime",
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "from_seq": from_seq,
        "through_seq": through_seq,
    })
}

fn checkpoint(through_seq: u64) -> serde_json::Value {
    json!({
        "type": "checkpoint",
        "batch_id": format!("batch-{through_seq}"),
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "through_seq": through_seq,
        "checkpoint": format!("checkpoint-{through_seq}"),
    })
}

#[derive(Clone)]
struct SyncServerSpec {
    roster: Vec<&'static str>,
    /// Agent whose first `/state` read blocks until the test releases it.
    gate_state_agent: Option<&'static str>,
    /// Agent whose first history (`before=`) read fails with 503.
    fail_first_older_agent: Option<&'static str>,
    live_diagnostics: bool,
}

impl Default for SyncServerSpec {
    fn default() -> Self {
        Self {
            roster: vec!["default", "beta"],
            gate_state_agent: None,
            fail_first_older_agent: None,
            live_diagnostics: false,
        }
    }
}

type EventHub = Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>>;

struct SyncServer {
    client: LocalClient,
    requests: Arc<Mutex<Vec<String>>>,
    events: EventHub,
    diagnostic_events: EventHub,
    pending_count: Arc<AtomicUsize>,
    fail_state: Arc<AtomicBool>,
    reset_next_stream: Arc<AtomicBool>,
    snapshot_epoch: Arc<Mutex<String>>,
    release_state_read: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SyncServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn agent_route(path: &str) -> Option<(String, &'static str)> {
    let rest = path.strip_prefix("/api/agents/")?;
    for (suffix, route) in [
        ("/status", "status"),
        ("/state", "state"),
        ("/conversation/stream", "stream"),
        ("/conversation", "conversation"),
    ] {
        if let Some(agent) = rest.strip_suffix(suffix) {
            return Some((agent.to_string(), route));
        }
    }
    None
}

impl SyncServer {
    fn recorded(&self, keep: impl Fn(&str) -> bool) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|uri| keep(uri))
            .cloned()
            .collect()
    }

    fn count(&self, needle: &str) -> usize {
        self.recorded(|uri| uri.contains(needle)).len()
    }

    fn push_event(&self, value: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .retain(|sender| sender.send(value.clone()).is_ok());
    }

    fn assert_no_events_requests(&self) {
        let leaked = self.recorded(|uri| uri.contains("/events"));
        assert!(
            leaked.is_empty(),
            "normal pages must never page /events (fixed 503, page=0 only) \
             nor open the raw /events/stream; leaked requests: {leaked:?}"
        );
    }
}

async fn sync_server(spec: SyncServerSpec) -> SyncServer {
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events: EventHub = Arc::new(Mutex::new(Vec::new()));
    let release_state_read = Arc::new(tokio::sync::Notify::new());
    let diagnostic_events: EventHub = Default::default();
    let pending_count = Arc::new(AtomicUsize::new(0));
    let fail_state = Arc::new(AtomicBool::new(false));
    let reset_next_stream = Arc::new(AtomicBool::new(false));
    let snapshot_epoch = Arc::new(Mutex::new("test-epoch".to_string()));
    let diagnostic_hub = diagnostic_events.clone();
    let server_pending = pending_count.clone();
    let server_fail_state = fail_state.clone();
    let server_reset_stream = reset_next_stream.clone();
    let server_epoch = snapshot_epoch.clone();
    let server_requests = requests.clone();
    let hub = events.clone();
    let notify = release_state_read.clone();
    let gate_agent: Option<String> = spec.gate_state_agent.map(str::to_owned);
    let fail_older_agent: Option<String> = spec.fail_first_older_agent.map(str::to_owned);
    let roster_value = roster(&spec.roster);
    let state_gate_armed = Arc::new(Mutex::new(true));
    let older_failed = Arc::new(Mutex::new(false));

    let router = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let requests = server_requests.clone();
        let hub = hub.clone();
        let notify = notify.clone();
        let gate_agent = gate_agent.clone();
        let fail_older_agent = fail_older_agent.clone();
        let roster_value = roster_value.clone();
        let state_gate_armed = state_gate_armed.clone();
        let older_failed = older_failed.clone();
        let diagnostic_hub = diagnostic_hub.clone();
        let pending_count = server_pending.clone();
        let fail_state = server_fail_state.clone();
        let reset_stream = server_reset_stream.clone();
        let epoch = server_epoch.clone();
        async move {
            let uri = uri
                .path_and_query()
                .map(|value| value.to_string())
                .unwrap_or_default();
            requests.lock().unwrap().push(uri.clone());
            let path = uri.split('?').next().unwrap_or_default().to_string();
            let query = uri.split_once('?').map(|(_, value)| value.to_string());

            if spec.live_diagnostics && path.ends_with("/events/stream") {
                if query.as_deref().is_some_and(|value| value.contains("after_seq=")) {
                    return (
                        StatusCode::GONE,
                        Json(json!({"error": "diagnostic cursor expired", "code": "cursor_not_found"})),
                    ).into_response();
                }
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                diagnostic_hub.lock().unwrap().push(sender);
                let stream = UnboundedReceiverStream::new(receiver).map(|value| {
                    Ok::<SseEvent, Infallible>(SseEvent::default()
                        .id(value["id"].as_str().unwrap())
                        .event("message")
                        .data(value.to_string()))
                });
                return (
                    [(
                        crate::runtime_event::EVENT_CONTRACT_VERSION_HEADER,
                        crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
                    )],
                    Sse::new(stream),
                )
                    .into_response();
            }
            // Pagination always fails; normal pages must not request either raw endpoint.
            if path.contains("/events") {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": "events endpoints are pinned unavailable for sync tests",
                        "code": "unavailable"
                    })),
                )
                    .into_response();
            }
            if path == "/api/agents/snapshot" {
                return Json(roster_value).into_response();
            }
            let Some((agent, route)) = agent_route(&path) else {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("unexpected route {path}")})),
                )
                    .into_response();
            };
            match route {
                "status" => Json(compact_status(&agent)).into_response(),
                "state" => {
                    if gate_agent.as_deref() == Some(agent.as_str()) {
                        let blocked = {
                            let mut armed = state_gate_armed.lock().unwrap();
                            let blocked = *armed;
                            *armed = false;
                            blocked
                        };
                        if blocked {
                            notify.notified().await;
                        }
                    }
                    if fail_state.swap(false, Ordering::SeqCst) {
                        return (StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": "state unavailable"}))).into_response();
                    }
                    let mut state = state_snapshot(&agent);
                    state["session"]["pending_count"] = json!(pending_count.load(Ordering::SeqCst));
                    Json(state).into_response()
                }
                "conversation" => {
                    let older = query.as_deref().is_some_and(|value| value.contains("before="));
                    if older {
                        let mut failed = older_failed.lock().unwrap();
                        if fail_older_agent.as_deref() == Some(agent.as_str()) && !*failed {
                            *failed = true;
                            drop(failed);
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({
                                    "error": "older conversation page unavailable",
                                    "code": "unavailable"
                                })),
                            )
                                .into_response();
                        }
                        Json(conversation_older_page()).into_response()
                    } else {
                        let mut page = conversation_first_page();
                        page["event_log_epoch"] = json!(*epoch.lock().unwrap());
                        Json(page).into_response()
                    }
                }
                _ => {
                    if reset_stream.swap(false, Ordering::SeqCst) {
                        return (StatusCode::GONE, Json(json!({
                            "error": "conversation epoch changed",
                            "code": "conversation_reset_required"
                        }))).into_response();
                    }
                    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                    hub.lock().unwrap().push(sender);
                    let stream = UnboundedReceiverStream::new(receiver).map(|value| {
                        Ok::<SseEvent, Infallible>(SseEvent::default().data(value.to_string()))
                    });
                    Sse::new(stream).into_response()
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    SyncServer {
        client: LocalClient::remote(test_config(), format!("http://{addr}"), "secret").unwrap(),
        requests,
        events,
        diagnostic_events,
        pending_count,
        fail_state,
        reset_next_stream,
        snapshot_epoch,
        release_state_read,
        task,
    }
}

fn app(client: LocalClient) -> TuiApp {
    TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    )
}

async fn pump_until(
    app: &mut TuiApp,
    mut ready: impl FnMut(&TuiApp) -> bool,
    timeout: Duration,
    context: &str,
) {
    let deadline = Instant::now() + timeout;
    while !ready(app) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; client: {}",
            app.client.connection_summary()
        );
        app.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn pump_for(app: &mut TuiApp, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        app.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn conversation_through_seq(app: &TuiApp) -> Option<u64> {
    app.conversation
        .as_ref()
        .map(|model| model.state_refresh_head().3)
}

#[tokio::test]
async fn cold_start_bootstraps_state_then_conversation_without_events_pagination() {
    let server = sync_server(SyncServerSpec::default()).await;
    let mut app = app(server.client.clone());
    app.initialize().await;

    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
                && server.count("/api/agents/default/conversation/stream") >= 1
        },
        Duration::from_secs(6),
        "cold start to reach a streaming conversation",
    )
    .await;

    // Bootstrap is /state-only: no /events pagination, and the conversation
    // snapshot/stream/history endpoints are the only follow-up reads.
    assert_eq!(
        server.recorded(|uri| {
            !uri.contains("/api/agents/snapshot")
                && (uri.contains("/api/agents/default/") || uri.contains("/events"))
        }),
        vec![
            "/api/agents/default/state",
            "/api/agents/default/conversation?limit=30",
            "/api/agents/default/conversation/stream?limit=30&after=snapshot-10",
        ]
    );
    assert!(server.count("/api/agents/snapshot") >= 1);
    server.assert_no_events_requests();
    let model = app.conversation.as_ref().unwrap();
    assert_eq!(model.agent_id, "default");
    assert_eq!(model.cursor, "snapshot-10");
    assert!(model.turns.contains_key("t1"));
    assert!(model.has_older());
}

#[tokio::test]
async fn explicit_agent_selection_bootstraps_only_target_agent() {
    let server = sync_server(SyncServerSpec::default()).await;
    let mut app = app(server.client.clone());
    app.initialize_with_agent(Some("beta".into()))
        .await
        .unwrap();

    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
                && server.count("/api/agents/beta/conversation/stream") >= 1
        },
        Duration::from_secs(6),
        "explicit selection to reach a streaming conversation",
    )
    .await;

    assert_eq!(
        server.recorded(|uri| {
            !uri.contains("/api/agents/snapshot")
                && (uri.contains("/api/agents/beta/") || uri.contains("/events"))
        }),
        vec![
            "/api/agents/beta/status?detail=compact",
            "/api/agents/beta/state",
            "/api/agents/beta/conversation?limit=30",
            "/api/agents/beta/conversation/stream?limit=30&after=snapshot-10",
        ]
    );
    assert_eq!(server.count("/api/agents/default/"), 0);
    server.assert_no_events_requests();
    assert_eq!(app.selected_agent_id(), Some("beta"));
    assert_eq!(app.conversation.as_ref().unwrap().agent_id, "beta");
    assert_eq!(app.conversation_generation, 1);
}

#[tokio::test]
async fn history_older_page_loads_through_conversation_without_raw_fallback() {
    let server = sync_server(SyncServerSpec::default()).await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(6),
        "conversation to load before scrolling",
    )
    .await;

    app.chat_max_scroll = 12;
    app.chat_scroll.scroll_with_key(KeyCode::Home, 12);
    assert!(app.chat_scroll.is_at_top(12));
    app.maybe_begin_load_older_conversation();

    pump_until(
        &mut app,
        |app| {
            app.conversation
                .as_ref()
                .is_some_and(|model| model.turns.contains_key("t0"))
        },
        Duration::from_secs(6),
        "older conversation page to merge",
    )
    .await;

    assert_eq!(
        server.count("/api/agents/default/conversation?limit=30&before=before-2"),
        1
    );
    let model = app.conversation.as_ref().unwrap();
    assert!(!model.has_older());
    assert!(model.turns.contains_key("t1"));
    server.assert_no_events_requests();
}

#[tokio::test]
async fn history_older_failure_keeps_conversation_and_recovers_without_raw_fallback() {
    let server = sync_server(SyncServerSpec {
        fail_first_older_agent: Some("default"),
        ..SyncServerSpec::default()
    })
    .await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(6),
        "conversation to load before scrolling",
    )
    .await;

    let stream_reads_before = server.count("/api/agents/default/conversation/stream");
    app.chat_max_scroll = 12;
    app.chat_scroll.scroll_with_key(KeyCode::Home, 12);
    app.maybe_begin_load_older_conversation();

    // The failed history read must not wipe the live conversation; recovery
    // goes through the conversation stream reconnect, never the raw stream.
    pump_until(
        &mut app,
        |app| {
            server.count("/api/agents/default/conversation/stream") > stream_reads_before
                && app.conversation.is_some()
                && app.conversation.as_ref().unwrap().turns.contains_key("t1")
        },
        Duration::from_secs(8),
        "conversation reconnect after the failed older-page read",
    )
    .await;
    server.assert_no_events_requests();

    assert!(app.chat_scroll.is_at_top(12));
    app.maybe_begin_load_older_conversation();
    pump_until(
        &mut app,
        |app| {
            app.conversation
                .as_ref()
                .is_some_and(|model| model.turns.contains_key("t0"))
        },
        Duration::from_secs(6),
        "retried older conversation page to merge",
    )
    .await;
    assert_eq!(server.count("before=before-2"), 2);
    server.assert_no_events_requests();
}

#[tokio::test]
async fn checkpoint_burst_coalesces_into_one_state_refresh_and_converges() {
    let server = sync_server(SyncServerSpec::default()).await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
                && server.count("/api/agents/default/conversation/stream") >= 1
        },
        Duration::from_secs(6),
        "conversation stream to connect before checkpoints",
    )
    .await;
    let generation = app.conversation_generation;

    // Age the scheduler explicitly instead of sleeping past the 5s minimum
    // automatic interval: no recent read, no pending deadline.
    app.state_refresh = Default::default();

    // Burst: a committed batch with a change, then an empty-delta batch whose
    // checkpoint still advances through_seq. Both must count as one hint, so
    // the whole burst is pushed before the 300ms hint window can elapse.
    server.push_event(batch_begin(10, 11));
    server.push_event(json!({
        "type": "turn_summary_upsert",
        "turn": terminal_turn("t2", 2),
    }));
    server.push_event(checkpoint(11));
    server.push_event(batch_begin(11, 12));
    server.push_event(checkpoint(12));
    pump_until(
        &mut app,
        |app| conversation_through_seq(app) == Some(12),
        Duration::from_secs(3),
        "burst checkpoints (including the empty-delta batch) to commit",
    )
    .await;
    assert_eq!(
        server.count("/api/agents/default/state"),
        1,
        "checkpoints inside the hint window must not read /state immediately"
    );

    // Hint window (300ms) plus margin: exactly one coalesced /state refresh.
    pump_for(&mut app, Duration::from_millis(800)).await;
    assert_eq!(
        server.count("/api/agents/default/state"),
        2,
        "a checkpoint burst must coalesce into exactly one /state refresh"
    );
    assert!(!app.snapshot_refresh_in_flight);
    assert_eq!(app.conversation_generation, generation);
    let model = app.conversation.as_ref().unwrap();
    assert_eq!(model.cursor, "checkpoint-12");
    assert!(model.turns.contains_key("t2"));
    server.assert_no_events_requests();
}

#[tokio::test]
async fn explicit_state_refresh_preserves_conversation_generation_cursor_and_scroll() {
    let server = sync_server(SyncServerSpec::default()).await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |app| {
            app.conversation.is_some()
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(6),
        "conversation to load before refresh",
    )
    .await;
    let generation = app.conversation_generation;
    let cursor = app.conversation.as_ref().unwrap().cursor.clone();

    app.chat_max_scroll = 12;
    for _ in 0..3 {
        app.chat_scroll.scroll_with_key(KeyCode::Up, 12);
    }
    assert_eq!(app.chat_scroll.effective_scroll(12), 9);
    assert!(!app.chat_scroll.is_following_tail());
    app.chat_scroll.prepare_for_history_prepend(12);
    let scroll_before_refresh = app.chat_scroll.clone();

    app.begin_bootstrap_selected_agent();
    pump_until(
        &mut app,
        |app| server.count("/api/agents/default/state") >= 2 && !app.snapshot_refresh_in_flight,
        Duration::from_secs(6),
        "explicit same-agent state refresh to settle",
    )
    .await;

    assert_eq!(app.conversation_generation, generation);
    assert_eq!(app.conversation.as_ref().unwrap().cursor, cursor);
    assert!(app.conversation.as_ref().unwrap().turns.contains_key("t1"));
    assert!(app.conversation_task.is_some());
    assert_eq!(app.chat_scroll, scroll_before_refresh);
    assert_eq!(app.chat_scroll.effective_scroll(12), 9);
    assert!(!app.chat_scroll.is_following_tail());
    // A same-agent refresh reloads neither the conversation snapshot nor the stream.
    assert_eq!(server.count("/api/agents/default/conversation?limit=30"), 1);
    assert_eq!(server.count("/api/agents/default/conversation/stream"), 1);
    server.assert_no_events_requests();
}

#[tokio::test]
async fn stale_slow_state_response_cannot_pollute_switched_agent() {
    let server = sync_server(SyncServerSpec {
        gate_state_agent: Some("default"),
        ..SyncServerSpec::default()
    })
    .await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |_| server.count("/api/agents/default/state") >= 1,
        Duration::from_secs(6),
        "gated /state read for the default agent to start",
    )
    .await;
    assert!(app.snapshot_refresh_in_flight);

    app.begin_bootstrap_agent_index(1);
    pump_until(
        &mut app,
        |app| {
            app.selected_agent_id() == Some("beta")
                && app
                    .conversation
                    .as_ref()
                    .is_some_and(|model| model.agent_id == "beta")
                && matches!(app.connection_state, TuiConnectionState::Streaming)
        },
        Duration::from_secs(6),
        "switch to beta to finish bootstrapping",
    )
    .await;
    let generation = app.conversation_generation;

    server.release_state_read.notify_one();
    pump_for(&mut app, Duration::from_millis(400)).await;

    assert_eq!(app.selected_agent_id(), Some("beta"));
    assert_eq!(
        app.projection.as_ref().unwrap().agent.identity.agent_id,
        "beta"
    );
    assert_eq!(app.conversation.as_ref().unwrap().agent_id, "beta");
    assert_eq!(app.conversation_generation, generation);
    assert!(!app.snapshot_refresh_in_flight);
    assert_eq!(server.count("/api/agents/default/state"), 1);
    assert_eq!(server.count("/api/agents/default/conversation"), 0);
    server.assert_no_events_requests();
}

#[tokio::test]
async fn explicit_refresh_while_state_read_inflight_coalesces_one_followup() {
    let server = sync_server(SyncServerSpec {
        gate_state_agent: Some("default"),
        ..SyncServerSpec::default()
    })
    .await;
    let mut app = app(server.client.clone());
    app.initialize().await;
    pump_until(
        &mut app,
        |_| server.count("/api/agents/default/state") >= 1,
        Duration::from_secs(6),
        "gated /state read to start",
    )
    .await;
    assert!(app.snapshot_refresh_in_flight);

    // An explicit refresh while the same agent's read is inflight must not
    // start a parallel HTTP read; it is recorded and coalesced afterwards.
    app.begin_bootstrap_selected_agent();
    assert_eq!(server.count("/api/agents/default/state"), 1);
    assert!(app.snapshot_refresh_in_flight);

    server.release_state_read.notify_one();
    pump_until(
        &mut app,
        |app| server.count("/api/agents/default/state") >= 2 && !app.snapshot_refresh_in_flight,
        Duration::from_secs(6),
        "gated read and its coalesced follow-up to settle",
    )
    .await;
    let generation = app.conversation_generation;

    pump_for(&mut app, Duration::from_millis(600)).await;
    assert_eq!(
        server.count("/api/agents/default/state"),
        2,
        "exactly one follow-up read may run after an inflight explicit refresh"
    );
    assert_eq!(app.conversation_generation, generation);
    assert!(app
        .conversation
        .as_ref()
        .is_some_and(|model| model.agent_id == "default"));
    server.assert_no_events_requests();
}
