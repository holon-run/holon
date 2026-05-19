use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

static PROVIDER_HTTP_TRACE_SEQ: AtomicU64 = AtomicU64::new(1);

const PROVIDER_HTTP_TRACE_ENV: &str = "HOLON_PROVIDER_HTTP_TRACE";
const PROVIDER_HTTP_FAILURE_TRACE_ENV: &str = "HOLON_PROVIDER_HTTP_FAILURE_TRACE";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHttpTraceDiagnostics {
    pub mode: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderHttpTraceMode {
    All,
    FailureOnly,
}

impl ProviderHttpTraceMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::FailureOnly => "failure_only",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderHttpTrace {
    home_dir: PathBuf,
    mode: ProviderHttpTraceMode,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderHttpTraceRequest {
    inner: Arc<Mutex<ProviderHttpTraceRequestState>>,
}

#[derive(Debug)]
struct ProviderHttpTraceRequestState {
    home_dir: PathBuf,
    mode: ProviderHttpTraceMode,
    agent_id: String,
    sequence: u64,
    file_path: Option<PathBuf>,
    events: Vec<Value>,
}

impl ProviderHttpTrace {
    pub(crate) fn from_env(home_dir: impl Into<PathBuf>) -> Option<Self> {
        let mode = if std::env::var(PROVIDER_HTTP_TRACE_ENV).ok().as_deref() == Some("1") {
            ProviderHttpTraceMode::All
        } else if std::env::var(PROVIDER_HTTP_FAILURE_TRACE_ENV)
            .ok()
            .as_deref()
            == Some("1")
        {
            ProviderHttpTraceMode::FailureOnly
        } else {
            return None;
        };
        Some(Self {
            home_dir: home_dir.into(),
            mode,
        })
    }

    pub(crate) fn begin_request(
        &self,
        agent_id: Option<&str>,
        provider: &str,
        model_ref: Option<&str>,
        url: &str,
        endpoint_kind: &str,
        headers: &[(&str, String)],
        body: &Value,
    ) -> Option<ProviderHttpTraceRequest> {
        let agent_id = agent_id
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                std::env::var("HOLON_AGENT_ID")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "unknown-agent".into());
        let sequence = PROVIDER_HTTP_TRACE_SEQ.fetch_add(1, Ordering::Relaxed);
        let request = ProviderHttpTraceRequest {
            inner: Arc::new(Mutex::new(ProviderHttpTraceRequestState {
                home_dir: self.home_dir.clone(),
                mode: self.mode,
                agent_id,
                sequence,
                file_path: None,
                events: Vec::new(),
            })),
        };
        request.write_event(json!({
            "type": "request",
            "created_at_ms": current_time_millis(),
            "sequence": sequence,
            "provider": provider,
            "model_ref": model_ref,
            "endpoint_kind": endpoint_kind,
            "url": redact_url(url),
            "headers": redact_headers(headers),
            "body": redact_json_secrets(body),
        }));
        Some(request)
    }
}

impl ProviderHttpTraceRequest {
    pub(crate) fn write_response_headers(
        &self,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) {
        self.write_event(json!({
            "type": "response_headers",
            "created_at_ms": current_time_millis(),
            "status": status.as_u16(),
            "headers": redact_header_map(headers),
        }));
    }

    pub(crate) fn write_response_body(&self, body: &str) {
        self.write_event(json!({
            "type": "response_body",
            "created_at_ms": current_time_millis(),
            "bytes": body.len(),
            "body": body,
        }));
    }

    pub(crate) fn write_stream_chunk(&self, chunk: &[u8]) {
        self.write_event(json!({
            "type": "stream_chunk",
            "created_at_ms": current_time_millis(),
            "bytes": chunk.len(),
            "text": String::from_utf8_lossy(chunk),
        }));
    }

    pub(crate) fn write_stream_terminal(&self, body: &Value) {
        self.write_event(json!({
            "type": "stream_terminal_response",
            "created_at_ms": current_time_millis(),
            "body": body,
        }));
    }

    pub(crate) fn diagnostics(&self, status: Option<u16>) -> Option<ProviderHttpTraceDiagnostics> {
        let mut guard = self.inner.lock().ok()?;
        let path = ensure_trace_file(&mut guard)?;
        Some(ProviderHttpTraceDiagnostics {
            mode: guard.mode.as_str().to_string(),
            path: path.to_string_lossy().to_string(),
            status,
        })
    }

    fn write_event(&self, event: Value) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.events.push(event.clone());
        if guard.mode == ProviderHttpTraceMode::All {
            let already_open = guard.file_path.is_some();
            let Some(path) = ensure_trace_file(&mut guard) else {
                return;
            };
            if already_open {
                append_trace_event(&path, &event);
            }
        }
    }
}

fn ensure_trace_file(state: &mut ProviderHttpTraceRequestState) -> Option<PathBuf> {
    if let Some(path) = &state.file_path {
        return Some(path.clone());
    }
    let trace_dir = state
        .home_dir
        .join(".holon")
        .join("http-trace")
        .join(sanitize_trace_path_segment(&state.agent_id));
    fs::create_dir_all(&trace_dir).ok()?;
    let created_at_ms = current_time_millis();
    let path = trace_dir.join(format!(
        "trace-{created_at_ms}-{sequence}.jsonl",
        sequence = state.sequence
    ));
    for event in &state.events {
        append_trace_event(&path, event);
    }
    state.file_path = Some(path.clone());
    Some(path)
}

fn append_trace_event(path: &Path, event: &Value) {
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

pub(crate) fn sanitize_trace_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn redact_headers(headers: &[(&str, String)]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(name, value)| {
                json!({
                    "name": *name,
                    "value": redact_header_value(name, value),
                })
            })
            .collect(),
    )
}

fn redact_header_map(headers: &reqwest::header::HeaderMap) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(name, value)| {
                let value = value.to_str().unwrap_or("<non-utf8>");
                json!({
                    "name": name.as_str(),
                    "value": redact_header_value(name.as_str(), value),
                })
            })
            .collect(),
    )
}

fn redact_header_value(name: &str, value: &str) -> String {
    let lowered = name.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key"
    ) {
        "[REDACTED]".into()
    } else {
        value.into()
    }
}

pub(crate) fn redact_url(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return raw.into();
    };
    if url.password().is_some() {
        let _ = url.set_password(Some("[REDACTED]"));
    }
    if !url.username().is_empty() {
        let _ = url.set_username("[REDACTED]");
    }
    let redacted_pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let lowered = key.to_ascii_lowercase();
            let value = if lowered.contains("key")
                || lowered.contains("token")
                || lowered.contains("secret")
            {
                "[REDACTED]".into()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    if !redacted_pairs.is_empty() {
        url.query_pairs_mut().clear().extend_pairs(redacted_pairs);
    }
    url.to_string()
}

pub(crate) fn redact_json_secrets(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let lowered = key.to_ascii_lowercase();
                    let value = if is_secret_json_key(&lowered) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_json_secrets(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_secrets).collect()),
        other => other.clone(),
    }
}

fn is_secret_json_key(lowered: &str) -> bool {
    lowered.contains("api_key")
        || lowered.contains("apikey")
        || lowered.contains("secret")
        || lowered == "authorization"
        || lowered == "token"
        || lowered.ends_with("_token")
        || lowered.starts_with("token_")
        || lowered.contains("_token_")
}

fn current_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        redact_headers, redact_json_secrets, redact_url, sanitize_trace_path_segment,
        ProviderHttpTrace,
    };
    use serde_json::{json, Value};
    use std::fs;

    #[test]
    fn provider_http_trace_redacts_secrets() {
        let headers = redact_headers(&[
            ("authorization", "Bearer secret".into()),
            ("openai-beta", "responses=experimental".into()),
        ]);
        assert_eq!(headers[0]["value"], "[REDACTED]");
        assert_eq!(headers[1]["value"], "responses=experimental");

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
}
