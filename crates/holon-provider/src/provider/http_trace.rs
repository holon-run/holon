use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ProviderRequestMaterial;
use crate::resource_io;
use crate::utf8::IncrementalUtf8LossyDecoder;

static PROVIDER_HTTP_TRACE_SEQ: AtomicU64 = AtomicU64::new(1);

const PROVIDER_HTTP_TRACE_ENV: &str = "HOLON_PROVIDER_HTTP_TRACE";
const PROVIDER_HTTP_FAILURE_TRACE_ENV: &str = "HOLON_PROVIDER_HTTP_FAILURE_TRACE";
const PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHttpTraceDiagnostics {
    #[serde(default)]
    pub capture_id: String,
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
pub struct ProviderHttpTrace {
    home_dir: PathBuf,
    mode: ProviderHttpTraceMode,
}

#[derive(Clone, Debug)]
pub struct ProviderHttpTraceRequest {
    inner: Arc<Mutex<ProviderHttpTraceRequestState>>,
    material: ProviderRequestMaterial,
}

#[derive(Debug)]
struct ProviderHttpTraceRequestState {
    home_dir: PathBuf,
    mode: ProviderHttpTraceMode,
    agent_id: String,
    sequence: u64,
    file_path: Option<PathBuf>,
    events: VecDeque<Value>,
    stream_text_decoder: IncrementalUtf8LossyDecoder,
    started_at: Instant,
    response_headers_at: Option<Duration>,
    first_body_chunk_at: Option<Duration>,
    last_progress_at: Option<Duration>,
    stream_body_bytes_received: u64,
    body_read_completed_at: Option<Duration>,
    completed_body_bytes: Option<usize>,
    timeout_policy: Option<Value>,
}

impl ProviderHttpTrace {
    pub fn begin_request_with_material(
        &self,
        agent_id: Option<&str>,
        provider: &str,
        model_ref: Option<&str>,
        url: &str,
        endpoint_kind: &str,
        headers: &[(&str, String)],
        body: &Value,
        material: &ProviderRequestMaterial,
    ) -> anyhow::Result<Option<ProviderHttpTraceRequest>> {
        material.read(|| {
            resource_io::admit_cloned_value(body)?;
            resource_io::admit_cloned_value(&headers)?;
            resource_io::admit_cloned_value(&(agent_id, provider, model_ref, url, endpoint_kind))?;
            resource_io::charge_owned_metadata(2048, 16)?;
            let request = self.begin_request(
                agent_id,
                provider,
                model_ref,
                url,
                endpoint_kind,
                headers,
                body,
            );
            Ok(request.map(|mut request| {
                request.material = material.clone();
                request
            }))
        })
    }

    #[cfg(any(test, feature = "runtime-tests"))]
    pub fn failure_only_for_tests(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            mode: ProviderHttpTraceMode::FailureOnly,
        }
    }

    pub fn from_env(home_dir: impl Into<PathBuf>) -> Option<Self> {
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

    pub fn begin_request(
        &self,
        agent_id: Option<&str>,
        provider: &str,
        model_ref: Option<&str>,
        url: &str,
        endpoint_kind: &str,
        headers: &[(&str, String)],
        body: &Value,
    ) -> Option<ProviderHttpTraceRequest> {
        let started_at = Instant::now();
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
            material: Default::default(),
            inner: Arc::new(Mutex::new(ProviderHttpTraceRequestState {
                home_dir: self.home_dir.clone(),
                mode: self.mode,
                agent_id,
                sequence,
                file_path: None,
                events: VecDeque::new(),
                stream_text_decoder: IncrementalUtf8LossyDecoder::new(),
                started_at,
                response_headers_at: None,
                first_body_chunk_at: None,
                last_progress_at: None,
                stream_body_bytes_received: 0,
                body_read_completed_at: None,
                completed_body_bytes: None,
                timeout_policy: None,
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
    #[cfg(feature = "runtime-tests")]
    pub fn buffered_events(&self) -> Vec<Value> {
        self.inner.lock().unwrap().events.iter().cloned().collect()
    }

    pub fn write_timeout_policy(
        &self,
        connect: Duration,
        headers: Duration,
        body: Duration,
        idle: Duration,
        total: Option<Duration>,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let policy = json!({
            "connect_timeout_ms": connect.as_millis(),
            "response_headers_timeout_ms": headers.as_millis(),
            "response_body_timeout_ms": body.as_millis(),
            "stream_idle_timeout_ms": idle.as_millis(),
            "total_timeout_ms": total.map(|duration| duration.as_millis()),
        });
        guard.timeout_policy = Some(policy.clone());
        write_event(
            &mut guard,
            json!({
                "type": "timeout_policy",
                "created_at_ms": current_time_millis(),
                "policy": policy,
            }),
        );
    }

    pub fn write_response_headers(
        &self,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) {
        if self
            .material
            .read(|| {
                let bytes = headers.iter().try_fold(0u64, |sum, (name, value)| {
                    sum.checked_add(u64::try_from(name.as_str().len())?)
                        .and_then(|n| n.checked_add(value.len() as u64))
                        .ok_or_else(|| anyhow::anyhow!("trace headers size overflow"))
                })?;
                resource_io::charge_owned_payload(bytes)?;
                resource_io::charge_owned_metadata(
                    u64::try_from(headers.len())?
                        .checked_mul(512)
                        .and_then(|n| n.checked_add(1024))
                        .ok_or_else(|| anyhow::anyhow!("trace headers size overflow"))?,
                    headers
                        .len()
                        .checked_mul(8)
                        .and_then(|n| n.checked_add(8))
                        .ok_or_else(|| anyhow::anyhow!("trace headers record overflow"))?,
                )?;
                Ok(())
            })
            .is_err()
        {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let elapsed = guard.started_at.elapsed();
        guard.response_headers_at.get_or_insert(elapsed);
        guard.last_progress_at = Some(elapsed);
        write_event(
            &mut guard,
            json!({
                "type": "response_headers",
                "created_at_ms": current_time_millis(),
                "status": status.as_u16(),
                "headers": redact_header_map(headers),
            }),
        );
    }

    pub fn write_response_body(&self, body: &str) {
        let _ = self.material.read(|| {
            resource_io::admit_cloned_value(&body)?;
            resource_io::charge_owned_metadata(1024, 8)?;
            let Ok(mut guard) = self.inner.lock() else {
                return Ok(());
            };
            let elapsed = guard.started_at.elapsed();
            guard.body_read_completed_at = Some(elapsed);
            guard.completed_body_bytes = Some(body.len());
            guard.last_progress_at = Some(elapsed);
            // A complete decoded body does not establish when its first byte arrived.
            write_event(
                &mut guard,
                json!({
                    "type": "response_body",
                    "created_at_ms": current_time_millis(),
                    "bytes": body.len(),
                    "body": body,
                }),
            );
            Ok(())
        });
    }

    pub fn write_stream_chunk(&self, chunk: &[u8]) {
        if self
            .material
            .read(|| {
                resource_io::charge_owned_payload(
                    u64::try_from(chunk.len())?
                        .checked_mul(6)
                        .and_then(|n| n.checked_add(24))
                        .ok_or_else(|| anyhow::anyhow!("trace text size overflow"))?,
                )?;
                resource_io::charge_owned_metadata(1024, 8)?;
                Ok(())
            })
            .is_err()
        {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if !chunk.is_empty() {
            let elapsed = guard.started_at.elapsed();
            guard.first_body_chunk_at.get_or_insert(elapsed);
            guard.last_progress_at = Some(elapsed);
            guard.stream_body_bytes_received = guard
                .stream_body_bytes_received
                .saturating_add(chunk.len() as u64);
        }
        let text = guard.stream_text_decoder.push(chunk);
        write_event(
            &mut guard,
            json!({
                "type": "stream_chunk",
                "created_at_ms": current_time_millis(),
                "bytes": chunk.len(),
                "text": text,
            }),
        );
    }

    pub fn write_stream_terminal(&self, body: &Value) {
        if self
            .material
            .read(|| {
                resource_io::admit_cloned_value(body)?;
                resource_io::charge_owned_metadata(1024, 8)?;
                Ok(())
            })
            .is_err()
        {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        flush_stream_text(&mut guard);
        write_event(
            &mut guard,
            json!({
                "type": "stream_terminal_response",
                "created_at_ms": current_time_millis(),
                "body": body,
            }),
        );
    }

    pub fn diagnostics(&self, status: Option<u16>) -> Option<ProviderHttpTraceDiagnostics> {
        self.material
            .read(|| {
                resource_io::charge_owned_metadata(4096, 8)?;
                Ok(())
            })
            .ok()?;
        let mut guard = self.inner.lock().ok()?;
        let elapsed = guard.started_at.elapsed();
        let phase = if guard.body_read_completed_at.is_some() {
            "body_complete"
        } else if guard.first_body_chunk_at.is_some() {
            "body_streaming"
        } else if guard.response_headers_at.is_some() {
            "response_headers"
        } else {
            "request_started"
        };
        let summary = json!({
            "type": "response_phase_summary",
            "created_at_ms": current_time_millis(),
            "phase": phase,
            "elapsed_ms": elapsed.as_millis(),
            "response_headers_elapsed_ms": guard.response_headers_at.map(|at| at.as_millis()),
            "first_body_chunk_elapsed_ms": guard.first_body_chunk_at.map(|at| at.as_millis()),
            "last_progress_elapsed_ms": guard.last_progress_at.map(|at| at.as_millis()),
            "since_last_progress_ms": elapsed
                .saturating_sub(guard.last_progress_at.unwrap_or_default())
                .as_millis(),
            "stream_body_bytes_received": guard.stream_body_bytes_received,
            "body_read_completed_elapsed_ms": guard.body_read_completed_at.map(|at| at.as_millis()),
            "completed_body_bytes": guard.completed_body_bytes,
            // Keep effective budgets even if their event fell out of the failure-only window.
            "timeout_policy": guard.timeout_policy,
        });
        write_event(&mut guard, summary);
        flush_stream_text(&mut guard);
        let path = ensure_trace_file(&mut guard)?;
        Some(ProviderHttpTraceDiagnostics {
            capture_id: format!("provider-http-{:016x}", guard.sequence),
            mode: guard.mode.as_str().to_string(),
            path: path.to_string_lossy().to_string(),
            status,
        })
    }

    fn write_event(&self, event: Value) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        write_event(&mut guard, event);
    }
}

fn flush_stream_text(state: &mut ProviderHttpTraceRequestState) {
    let text = state.stream_text_decoder.finish();
    if text.is_empty() {
        return;
    }
    write_event(
        state,
        json!({
            "type": "stream_text_flush",
            "created_at_ms": current_time_millis(),
            "text": text,
        }),
    );
}

fn write_event(state: &mut ProviderHttpTraceRequestState, event: Value) {
    match state.mode {
        ProviderHttpTraceMode::All => {
            let Some(path) = ensure_trace_file(state) else {
                return;
            };
            append_trace_event(&path, &event);
        }
        ProviderHttpTraceMode::FailureOnly => {
            state.events.push_back(event);
            if state.events.len() > PROVIDER_HTTP_FAILURE_TRACE_MAX_EVENTS {
                state.events.pop_front();
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
    write_trace_events(&path, &state.events).ok()?;
    state.events.clear();
    state.file_path = Some(path.clone());
    Some(path)
}

fn write_trace_events(path: &Path, events: &VecDeque<Value>) -> io::Result<()> {
    let mut file = File::create(path)?;
    for event in events {
        serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
        writeln!(file)?;
    }
    Ok(())
}

fn append_trace_event(path: &Path, event: &Value) {
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if serde_json::to_writer(&mut file, event).is_ok() {
        let _ = writeln!(file);
    }
}

pub fn sanitize_trace_path_segment(value: &str) -> String {
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

pub fn redact_headers(headers: &[(&str, String)]) -> Value {
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
    // Unknown headers may carry operator-configured values, regardless of name.
    // Only transport-owned protocol controls are safe to display.
    if !matches!(
        lowered.as_str(),
        "content-type"
            | "content-length"
            | "accept"
            | "anthropic-version"
            | "anthropic-beta"
            | "openai-beta"
    ) {
        "[REDACTED]".into()
    } else {
        value.into()
    }
}

pub fn redact_url(raw: &str) -> String {
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

pub fn redact_json_secrets(value: &Value) -> Value {
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
mod tests;
