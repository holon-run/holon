use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::prelude::*;

type ConcurrentFailure = (
    RuntimeDb,
    ProviderAdmissionLease,
    ProviderAdmissionScope,
    DateTime<Utc>,
);

fn cool_primary(failure: &Mutex<Option<ConcurrentFailure>>) {
    let (db, lease, scope, now) = failure.lock().unwrap().take().unwrap();
    assert!(db
        .record_provider_admission_failure(
            &lease,
            &scope,
            ProviderCooldownPolicy::Transport,
            now,
            Some(now + Duration::minutes(10)),
            "concurrent/request",
        )
        .unwrap());
}

struct CoolOnRetry(Arc<Mutex<Option<ConcurrentFailure>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CoolOnRetry {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // The provider has recorded the failed HTTP attempt and scheduled backoff.
        if event.metadata().fields().field("backoff_source").is_some()
            && event.metadata().fields().field("max_attempts").is_some()
        {
            cool_primary(&self.0);
        }
    }
}

#[test]
fn real_http_tool_continuation_reselects_an_independent_cooling_candidate() {
    run_tool_continuation_reselection(false);
}

#[test]
fn real_http_tool_continuation_reselection_preserves_retry_usage() {
    run_tool_continuation_reselection(true);
}

fn run_tool_continuation_reselection(retry_before_cooling: bool) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(tool_continuation_reselection(retry_before_cooling));
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn tool_continuation_reselection(retry_before_cooling: bool) {
    use crate::runtime_db::provider_admission::ProviderAdmissionReservation;
    use axum::{routing::post, Json, Router};

    let failure = Arc::new(Mutex::<Option<ConcurrentFailure>>::new(None));
    let _subscriber = retry_before_cooling.then(|| {
        tracing::subscriber::set_default(
            tracing_subscriber::registry().with(CoolOnRetry(failure.clone())),
        )
    });
    let pending_failure = failure.clone();
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let observed_primary = primary_calls.clone();
    let primary_requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let observed_primary_requests = primary_requests.clone();
    let primary = Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let failure = pending_failure.clone();
        let calls = observed_primary.clone();
        let bodies = observed_primary_requests.clone();
        async move {
            bodies.lock().unwrap().push(body);
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if retry_before_cooling && call == 1 {
                return Json(serde_json::json!({
                    "id": "resp-empty", "status": "completed",
                    "usage": {"input_tokens": 7, "output_tokens": 11},
                    "output": []
                }));
            }
            assert_eq!(call, 0, "cooled A must not receive a continuation");
            if !retry_before_cooling {
                cool_primary(&failure);
            }
            Json(serde_json::json!({
                "id": "resp-tool", "status": "completed",
                "usage": {"input_tokens": 1, "output_tokens": 1},
                "output": [{
                    "type": "function_call",
                    "id": "fc-tool",
                    "status": "completed",
                    "call_id": "call-tool",
                    "name": "GetAgent",
                    "arguments": "{}"
                }]
            }))
        }
    }));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let observed_fallback = fallback_calls.clone();
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let observed_requests = requests.clone();
    let fallback = Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let calls = observed_fallback.clone();
        let requests = observed_requests.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            requests.lock().unwrap().push(body);
            Json(serde_json::json!({
                "id": "msg-fallback", "type": "message", "role": "assistant",
                "model": "claude-sonnet-4-6", "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "continued once"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
        }
    }));
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_url = format!("http://{}", primary_listener.local_addr().unwrap());
    let fallback_url = format!("http://{}", fallback_listener.local_addr().unwrap());
    let primary_server =
        tokio::spawn(async move { axum::serve(primary_listener, primary).await.unwrap() });
    let fallback_server =
        tokio::spawn(async move { axum::serve(fallback_listener, fallback).await.unwrap() });
    let (_home, runtime) = continuation_runtime_with_urls([&primary_url, &fallback_url]);
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy initial input")
    };
    let admission = scheduled.dispatch_plan.provider_admission.as_ref().unwrap();
    let episode_key = admission.episode_key.clone();
    let ProviderAdmissionReservation::Granted(lease) = runtime
        .runtime_db()
        .reserve_provider_admission(&admission.candidate.scopes, runtime.now(), PROBE_LEASE)
        .unwrap()
    else {
        panic!("concurrent healthy reservation")
    };
    *failure.lock().unwrap() = Some((
        runtime.runtime_db().clone(),
        lease,
        admission.candidate.connection.clone(),
        runtime.now(),
    ));
    let message = scheduled.message.clone();
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .expect("independent B must continue after A cools");
    assert!(runtime
        .commit_queue_terminal_settlement(
            crate::types::QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority,
                status: crate::types::QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: runtime.now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap());
    let primary_body = primary_requests.lock().unwrap()[0].to_string();
    assert!(
        !primary_body.contains("function_call_output") && !primary_body.contains("tool_result"),
        "cooled A must not receive the tool replay"
    );
    assert_eq!(
        primary_calls.load(Ordering::SeqCst),
        if retry_before_cooling { 2 } else { 1 }
    );
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool.tool_name == "GetAgent")
            .count(),
        1
    );
    let requests = requests.lock().unwrap();
    let messages = requests[0]["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(
            |message| message["content"].as_array().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block["type"] == "tool_result" && block["tool_use_id"] == "call-tool"
                })
            })
        ),
        "B must receive the original tool result"
    );
    assert!(
        requests[0].to_string().contains("continue"),
        "original input retained"
    );
    let episode = runtime
        .runtime_db()
        .provider_recovery_episode(&episode_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        episode.total_requests,
        if retry_before_cooling { 3 } else { 2 },
        "only actual HTTP requests are charged"
    );
    if retry_before_cooling {
        assert!(failure.lock().unwrap().is_none(), "retry boundary cooled A");
        let events = runtime.storage().read_recent_events(256).unwrap();
        let round = events
            .iter()
            .find(|event| {
                event.data["round"] == 2
                    && event.data["provider_attempt_timeline"]["winning_model_ref"]
                        .as_str()
                        .is_some_and(|model| model.contains("claude"))
            })
            .expect("completed continuation round");
        let timeline = &round.data["provider_attempt_timeline"];
        let attempts = timeline["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 2, "A retry failure and B success retained");
        assert_eq!(attempts[0]["token_usage"]["input_tokens"], 7);
        assert_eq!(attempts[0]["token_usage"]["output_tokens"], 11);
        assert_eq!(attempts[1]["token_usage"]["input_tokens"], 1);
        assert_eq!(timeline["aggregated_token_usage"]["input_tokens"], 8);
        assert_eq!(timeline["aggregated_token_usage"]["output_tokens"], 12);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "provider_attempt_failed")
                .count(),
            1,
            "one real failed HTTP attempt; local guard denial is not HTTP"
        );
    }
    assert!(runtime
        .runtime_db()
        .provider_queue_parking("default", &message.id)
        .unwrap()
        .is_none());
    primary_server.abort();
    fallback_server.abort();
}
