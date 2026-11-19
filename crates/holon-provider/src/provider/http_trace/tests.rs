use super::{
    redact_headers, redact_json_secrets, redact_url, sanitize_trace_path_segment,
    ProviderHttpTrace, ProviderHttpTraceDiagnostics,
};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

fn failure_trace_request(home: &Path) -> super::ProviderHttpTraceRequest {
    ProviderHttpTrace::failure_only_for_tests(home)
        .begin_request(
            Some("agent/one"),
            "openai",
            Some("openai/gpt-test"),
            "https://api.example.com/v1/responses",
            "responses",
            &[],
            &json!({}),
        )
        .unwrap()
}

fn failure_trace_events(request: &super::ProviderHttpTraceRequest) -> Vec<Value> {
    let diagnostics = request.diagnostics(Some(500)).unwrap();
    fs::read_to_string(diagnostics.path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn phase_summary(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|event| event["type"] == "response_phase_summary")
        .expect("phase summary should be recorded")
}

#[test]
fn provider_http_trace_records_effective_timeout_policy() {
    for total in [None, Some(Duration::from_secs(10))] {
        let home = tempfile::tempdir().unwrap();
        let request = failure_trace_request(home.path());
        request.write_timeout_policy(
            Duration::from_millis(250),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
            total,
        );
        assert!(!home.path().join(".holon/http-trace").exists());

        let events = failure_trace_events(&request);
        let policy = json!({
            "connect_timeout_ms": 250,
            "response_headers_timeout_ms": 1_000,
            "response_body_timeout_ms": 2_000,
            "stream_idle_timeout_ms": 3_000,
            "total_timeout_ms": total.map(|duration| duration.as_millis()),
        });
        assert_eq!(events[1]["type"], "timeout_policy");
        assert_eq!(events[1]["policy"], policy);
        assert_eq!(phase_summary(&events)["timeout_policy"], policy);
    }
}

#[test]
fn provider_http_failure_trace_summarizes_request_before_response() {
    let home = tempfile::tempdir().unwrap();
    let request = failure_trace_request(home.path());
    request.inner.lock().unwrap().started_at -= Duration::from_secs(2);

    let events = failure_trace_events(&request);
    let summary = phase_summary(&events);
    assert_eq!(summary["phase"], "request_started");
    assert!(summary["elapsed_ms"].as_u64().unwrap() >= 2_000);
    assert_eq!(summary["since_last_progress_ms"], summary["elapsed_ms"]);
    assert!(summary["response_headers_elapsed_ms"].is_null());
    assert!(summary["first_body_chunk_elapsed_ms"].is_null());
    assert!(summary["last_progress_elapsed_ms"].is_null());
    assert_eq!(summary["stream_body_bytes_received"], 0);
}

#[test]
fn provider_http_failure_trace_distinguishes_headers_without_body_progress() {
    let home = tempfile::tempdir().unwrap();
    let request = failure_trace_request(home.path());
    request.write_response_headers(reqwest::StatusCode::OK, &reqwest::header::HeaderMap::new());
    request.write_stream_chunk(&[]);
    {
        let mut state = request.inner.lock().unwrap();
        assert_eq!(state.last_progress_at, state.response_headers_at);
        state.started_at -= Duration::from_secs(2);
    }

    let events = failure_trace_events(&request);
    let summary = phase_summary(&events);
    assert_eq!(summary["phase"], "response_headers");
    assert!(summary["response_headers_elapsed_ms"].is_number());
    assert!(summary["first_body_chunk_elapsed_ms"].is_null());
    assert_eq!(summary["stream_body_bytes_received"], 0);
    assert!(summary["since_last_progress_ms"].as_u64().unwrap() >= 2_000);
    assert!(summary["body_read_completed_elapsed_ms"].is_null());
    assert!(summary["completed_body_bytes"].is_null());
}

#[test]
fn provider_http_failure_trace_counts_nonempty_chunks_before_utf8_is_complete() {
    let home = tempfile::tempdir().unwrap();
    let request = failure_trace_request(home.path());
    request.write_response_headers(reqwest::StatusCode::OK, &reqwest::header::HeaderMap::new());
    request.write_stream_chunk(&[0xF0, 0x9F]);
    let first_chunk_at = {
        let state = request.inner.lock().unwrap();
        assert!(state.first_body_chunk_at >= state.response_headers_at);
        assert_eq!(state.stream_body_bytes_received, 2);
        assert_eq!(state.first_body_chunk_at, state.last_progress_at);
        state.first_body_chunk_at.unwrap()
    };
    request.write_stream_chunk(&[]);
    assert_eq!(
        request.inner.lock().unwrap().last_progress_at,
        Some(first_chunk_at)
    );
    request.write_stream_chunk(&[0x98, 0x80]);
    {
        let mut state = request.inner.lock().unwrap();
        assert_eq!(state.first_body_chunk_at, Some(first_chunk_at));
        assert!(state.last_progress_at.unwrap() >= first_chunk_at);
        state.started_at -= Duration::from_secs(3);
    }

    let events = failure_trace_events(&request);
    let summary = phase_summary(&events);
    assert_eq!(summary["phase"], "body_streaming");
    assert_eq!(
        summary["first_body_chunk_elapsed_ms"],
        json!(first_chunk_at.as_millis())
    );
    assert_eq!(summary["stream_body_bytes_received"], 4);
    assert!(summary["since_last_progress_ms"].as_u64().unwrap() >= 3_000);
    assert!(summary["body_read_completed_elapsed_ms"].is_null());
}

#[test]
fn provider_http_trace_complete_body_does_not_invent_first_chunk_or_double_count() {
    for streamed in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let request = failure_trace_request(home.path());
        let body = "hello😀";
        if streamed {
            request.write_stream_chunk(body.as_bytes());
        }
        request.write_response_body(body);

        let events = failure_trace_events(&request);
        let summary = phase_summary(&events);
        assert_eq!(summary["phase"], "body_complete");
        assert_eq!(summary["first_body_chunk_elapsed_ms"].is_number(), streamed);
        assert_eq!(
            summary["stream_body_bytes_received"],
            json!(if streamed { body.len() } else { 0 })
        );
        assert_eq!(summary["completed_body_bytes"], json!(body.len()));
        assert_eq!(
            summary["last_progress_elapsed_ms"],
            summary["body_read_completed_elapsed_ms"]
        );
    }
}

#[test]
fn provider_http_failure_trace_summary_survives_event_window_eviction() {
    let home = tempfile::tempdir().unwrap();
    let request = failure_trace_request(home.path());
    request.write_timeout_policy(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(3),
        Duration::from_secs(4),
        Some(Duration::from_secs(30)),
    );
    request.write_response_headers(reqwest::StatusCode::OK, &reqwest::header::HeaderMap::new());
    let chunk_count = super::PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS + 10;
    for _ in 0..chunk_count {
        request.write_stream_chunk(b"x");
    }
    assert!(!home.path().join(".holon/http-trace").exists());

    let events = failure_trace_events(&request);
    assert_eq!(events.len(), super::PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS);
    assert!(events.iter().all(|event| {
        event["type"] == "stream_chunk" || event["type"] == "response_phase_summary"
    }));
    let summary = phase_summary(&events);
    assert_eq!(summary["phase"], "body_streaming");
    assert!(summary["response_headers_elapsed_ms"].is_number());
    assert!(summary["first_body_chunk_elapsed_ms"].is_number());
    assert_eq!(summary["stream_body_bytes_received"], json!(chunk_count));
    assert_eq!(summary["timeout_policy"]["total_timeout_ms"], 30_000);
}

#[test]
fn provider_http_trace_diagnostics_deserialize_without_capture_id() {
    let diagnostics: ProviderHttpTraceDiagnostics = serde_json::from_value(json!({
        "mode": "failure_only",
        "path": "/tmp/provider-http-trace.jsonl"
    }))
    .unwrap();

    assert_eq!(diagnostics.capture_id, "");
}

#[test]
fn provider_http_trace_redacts_secrets() {
    let headers = redact_headers(&[
        ("authorization", "Bearer secret".into()),
        ("openai-beta", "responses=experimental".into()),
        ("user-agent", "private-custom-value".into()),
        ("x-ordinary-routing", "arbitrary-secret-like-value".into()),
        ("x-goog-api-key", "gemini-secret".into()),
        ("x-opencode-session", "opaque-session".into()),
    ]);
    assert_eq!(headers[0]["value"], "[REDACTED]");
    assert_eq!(headers[1]["value"], "responses=experimental");
    for header in headers.as_array().unwrap().iter().skip(2) {
        assert_eq!(header["value"], "[REDACTED]");
    }

    assert_eq!(
            redact_url("https://user:pass@example.com/v1/responses?api_key=secret&debug=1"),
            "https://%5BREDACTED%5D:%5BREDACTED%5D@example.com/v1/responses?api_key=%5BREDACTED%5D&debug=1"
        );

    let body = redact_json_secrets(&json!({
        "model": "gpt-test",
        "max_output_tokens": 4096,
        "access_token": "secret",
        "nested": {
            "api_key": "secret",
            "prompt_tokens": 123,
            "reasoning_tokens": 45
        }
    }));
    assert_eq!(body["access_token"], "[REDACTED]");
    assert_eq!(body["nested"]["api_key"], "[REDACTED]");
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["nested"]["prompt_tokens"], 123);
    assert_eq!(body["nested"]["reasoning_tokens"], 45);
    assert_eq!(body["model"], "gpt-test");
}

#[test]
fn provider_http_trace_sanitizes_agent_path_segment() {
    assert_eq!(
        sanitize_trace_path_segment("agent/with spaces"),
        "agent_with_spaces"
    );
}

#[test]
fn provider_http_trace_writes_full_request_body_under_home() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::All,
    };
    let body = json!({
        "model": "gpt-test",
        "input": [{ "type": "message", "content": "hello" }],
        "tools": [{ "type": "function", "name": "ApplyPatch" }]
    });

    trace
        .begin_request(
            Some("agent/one"),
            "openai",
            Some("openai/gpt-test"),
            "https://api.openai.com/v1/responses",
            "responses",
            &[("authorization", "Bearer secret".into())],
            &body,
        )
        .expect("trace should be written");

    let trace_dir = home.path().join(".holon/http-trace/agent_one");
    let entries = fs::read_dir(trace_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let line = fs::read_to_string(entries[0].path()).unwrap();
    let event: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(event["type"], "request");
    assert_eq!(
        event["body"]["tools"][0]["name"],
        Value::String("ApplyPatch".into())
    );
    assert_eq!(event["headers"][0]["value"], "[REDACTED]");
}

#[test]
fn provider_http_failure_trace_writes_only_when_diagnostics_are_requested() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::FailureOnly,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "anthropic",
            Some("anthropic/claude"),
            "https://api.anthropic.com/v1/messages",
            "messages",
            &[("authorization", "Bearer secret".into())],
            &json!({ "model": "claude", "messages": [] }),
        )
        .expect("trace should be created");
    let trace_dir = home.path().join(".holon/http-trace/agent_one");
    assert!(!trace_dir.exists());

    let diagnostics = request
        .diagnostics(Some(400))
        .expect("failure diagnostics should write trace");
    assert_eq!(diagnostics.mode, "failure_only");
    assert!(std::path::Path::new(&diagnostics.path).exists());
}

#[test]
fn provider_http_failure_trace_flushes_request_for_protocol_diagnostics() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::FailureOnly,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "anthropic",
            Some("anthropic/mimo"),
            "https://api.example.com/v1/messages",
            "messages",
            &[("authorization", "Bearer secret".into())],
            &json!({
                "model": "mimo",
                "messages": [{ "role": "user", "content": "use a tool" }],
                "tools": [{ "name": "ExecCommand" }]
            }),
        )
        .expect("trace should be created");
    request.write_response_body(
            r#"{"content":[{"type":"text","text":"<tool_call><function=ExecCommand>{}</function></tool_call>"}],"stop_reason":"tool_use"}"#,
        );

    let diagnostics = request
        .diagnostics(None)
        .expect("protocol diagnostics should write trace");
    let trace_text = fs::read_to_string(diagnostics.path).unwrap();
    assert!(trace_text.contains("\"type\":\"request\""));
    assert!(trace_text.contains("\"type\":\"response_body\""));
    assert!(trace_text.contains("\"name\":\"ExecCommand\""));
    assert!(trace_text.contains("[REDACTED]"));
    assert!(!trace_text.contains("Bearer secret"));
}

#[test]
fn provider_http_trace_preserves_utf8_across_stream_chunks() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::All,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "openai",
            Some("openai/gpt-test"),
            "https://api.example.com/v1/responses",
            "responses",
            &[],
            &json!({}),
        )
        .unwrap();

    for byte in "😀".as_bytes() {
        request.write_stream_chunk(std::slice::from_ref(byte));
    }
    request.write_stream_terminal(&json!({ "status": "completed" }));

    let trace_dir = home.path().join(".holon/http-trace/agent_one");
    let path = fs::read_dir(trace_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let events = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let text = events
        .iter()
        .filter_map(|event| {
            matches!(
                event["type"].as_str(),
                Some("stream_chunk" | "stream_text_flush")
            )
            .then(|| event["text"].as_str().unwrap_or_default())
        })
        .collect::<String>();

    assert_eq!(text, "😀");
    assert!(!text.contains('\u{FFFD}'));
}

#[test]
fn provider_http_failure_trace_flushes_incomplete_utf8_lossily() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::FailureOnly,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "openai",
            Some("openai/gpt-test"),
            "https://api.example.com/v1/responses",
            "responses",
            &[],
            &json!({}),
        )
        .unwrap();
    request.write_stream_chunk(&[0xF0, 0x9F]);

    let diagnostics = request.diagnostics(Some(500)).unwrap();
    let trace_text = fs::read_to_string(diagnostics.path).unwrap();
    assert!(trace_text.contains("\"type\":\"stream_text_flush\""));
    assert!(trace_text.contains('�'));
}

#[test]
fn provider_http_failure_trace_keeps_buffer_when_trace_file_cannot_be_created() {
    let home = tempfile::tempdir().unwrap();
    let home_file = home.path().join("not-a-directory");
    fs::write(&home_file, "occupied").unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home_file,
        mode: super::ProviderHttpTraceMode::FailureOnly,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "anthropic",
            Some("anthropic/claude"),
            "https://api.anthropic.com/v1/messages",
            "messages",
            &[("authorization", "Bearer secret".into())],
            &json!({ "model": "claude", "messages": [] }),
        )
        .expect("trace should be created");

    assert!(request.diagnostics(Some(500)).is_none());
    let state = request.inner.lock().unwrap();
    assert_eq!(state.events.len(), 2);
    assert_eq!(state.events.front().unwrap()["type"], "request");
    assert_eq!(
        state.events.back().unwrap()["type"],
        "response_phase_summary"
    );
    assert!(state.file_path.is_none());
}

#[test]
fn provider_http_failure_trace_keeps_only_the_recent_event_window() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace {
        home_dir: home.path().to_path_buf(),
        mode: super::ProviderHttpTraceMode::FailureOnly,
    };
    let request = trace
        .begin_request(
            Some("agent/one"),
            "anthropic",
            Some("anthropic/claude"),
            "https://api.anthropic.com/v1/messages",
            "messages",
            &[("authorization", "Bearer secret".into())],
            &json!({ "model": "claude", "messages": [] }),
        )
        .expect("trace should be created");

    for index in 0..super::PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS + 10 {
        request.write_stream_terminal(&json!({ "index": index }));
    }

    let state = request.inner.lock().unwrap();
    assert_eq!(
        state.events.len(),
        super::PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS
    );
    assert_eq!(state.events.front().unwrap()["body"]["index"], 10);
}
