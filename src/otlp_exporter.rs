use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::observability::{TraceContext, TraceSpan, TraceSpanStatus};

const OTLP_JSON_CONTENT_TYPE: &str = "application/json";
const SERVICE_NAME: &str = "holon";
const SCOPE_NAME: &str = "holon.runtime";

static OTLP_HANDLE: OnceLock<Mutex<Option<OtlpExporterHandle>>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct OtlpExporterConfig {
    pub endpoint: String,
    pub headers: BTreeMap<String, String>,
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub batch_interval: Duration,
    pub timeout: Duration,
}

impl Default for OtlpExporterConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            headers: BTreeMap::new(),
            queue_capacity: 1_024,
            batch_size: 128,
            batch_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(5),
        }
    }
}

impl OtlpExporterConfig {
    pub fn validate(&self) -> Result<()> {
        let endpoint = self.endpoint.trim();
        if endpoint.is_empty() {
            return Err(anyhow!("OTLP endpoint must not be empty"));
        }
        let url = reqwest::Url::parse(endpoint).context("invalid OTLP endpoint")?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(anyhow!("OTLP endpoint must use http or https"));
        }
        if self.queue_capacity == 0 {
            return Err(anyhow!("OTLP queue capacity must be greater than zero"));
        }
        if self.batch_size == 0 {
            return Err(anyhow!("OTLP batch size must be greater than zero"));
        }
        if self.batch_interval.is_zero() {
            return Err(anyhow!("OTLP batch interval must be greater than zero"));
        }
        if self.timeout.is_zero() {
            return Err(anyhow!("OTLP timeout must be greater than zero"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OtlpExporterStats {
    pub queue_depth: u64,
    pub queued_spans: u64,
    pub dropped_spans: u64,
    pub exported_spans: u64,
    pub failed_batches: u64,
}

#[derive(Default)]
struct ExporterCounters {
    shutting_down: AtomicBool,
    queue_depth: AtomicU64,
    queued_spans: AtomicU64,
    dropped_spans: AtomicU64,
    exported_spans: AtomicU64,
    failed_batches: AtomicU64,
}

#[derive(Clone)]
struct OtlpExporterHandle {
    sender: SyncSender<ExporterMessage>,
    counters: Arc<ExporterCounters>,
}

impl OtlpExporterHandle {
    fn try_send(&self, span: ExportSpan) {
        if self.counters.shutting_down.load(Ordering::Acquire) {
            self.counters.dropped_spans.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.queue_depth.fetch_add(1, Ordering::Relaxed);
        match self.sender.try_send(ExporterMessage::Span(span)) {
            Ok(()) => {
                self.counters.queued_spans.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.counters.dropped_spans.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn stats(&self) -> OtlpExporterStats {
        stats_from_counters(&self.counters)
    }
}

#[derive(Debug)]
struct ExportSpan {
    context: TraceContext,
    span: TraceSpan,
}

enum ExporterMessage {
    Span(ExportSpan),
    Shutdown,
}

pub struct OtlpExporter {
    handle: Option<OtlpExporterHandle>,
    worker: Option<thread::JoinHandle<()>>,
}

impl OtlpExporter {
    pub fn start(config: OtlpExporterConfig) -> Result<Self> {
        config.validate()?;
        let headers = build_headers(&config.headers)?;
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .context("failed to build OTLP HTTP client")?;
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let counters = Arc::new(ExporterCounters::default());
        let worker_counters = Arc::clone(&counters);
        let worker = thread::Builder::new()
            .name("holon-otlp-exporter".to_string())
            .spawn(move || run_worker(receiver, client, headers, config, worker_counters))
            .context("failed to start OTLP exporter worker")?;
        Ok(Self {
            handle: Some(OtlpExporterHandle { sender, counters }),
            worker: Some(worker),
        })
    }

    pub fn install(&self) {
        let Some(handle) = self.handle.as_ref() else {
            return;
        };
        *otlp_handle().lock().expect("OTLP handle lock poisoned") = Some(handle.clone());
    }

    pub fn stats(&self) -> OtlpExporterStats {
        self.handle
            .as_ref()
            .map(OtlpExporterHandle::stats)
            .unwrap_or_default()
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.counters.shutting_down.store(true, Ordering::Release);
            clear_installed_handle(&handle);
            let _ = handle.sender.send(ExporterMessage::Shutdown);
            drop(handle);
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow!("OTLP exporter worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for OtlpExporter {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

pub fn try_record_span(context: &TraceContext, span: &TraceSpan) {
    let handle = otlp_handle()
        .lock()
        .expect("OTLP handle lock poisoned")
        .clone();
    if let Some(handle) = handle {
        handle.try_send(ExportSpan {
            context: context.clone(),
            span: span.clone(),
        });
    }
}

pub fn exporter_stats() -> OtlpExporterStats {
    otlp_handle()
        .lock()
        .expect("OTLP handle lock poisoned")
        .as_ref()
        .map(OtlpExporterHandle::stats)
        .unwrap_or_default()
}

fn otlp_handle() -> &'static Mutex<Option<OtlpExporterHandle>> {
    OTLP_HANDLE.get_or_init(|| Mutex::new(None))
}

fn clear_installed_handle(handle: &OtlpExporterHandle) {
    let mut installed = otlp_handle().lock().expect("OTLP handle lock poisoned");
    if installed
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(&current.counters, &handle.counters))
    {
        *installed = None;
    }
}

fn stats_from_counters(counters: &ExporterCounters) -> OtlpExporterStats {
    OtlpExporterStats {
        queue_depth: counters.queue_depth.load(Ordering::Relaxed),
        queued_spans: counters.queued_spans.load(Ordering::Relaxed),
        dropped_spans: counters.dropped_spans.load(Ordering::Relaxed),
        exported_spans: counters.exported_spans.load(Ordering::Relaxed),
        failed_batches: counters.failed_batches.load(Ordering::Relaxed),
    }
}

fn build_headers(configured: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(OTLP_JSON_CONTENT_TYPE),
    );
    for (name, value) in configured {
        let name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid OTLP header name {name}"))?;
        let value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid value for OTLP header {name}"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn run_worker(
    receiver: Receiver<ExporterMessage>,
    client: Client,
    headers: HeaderMap,
    config: OtlpExporterConfig,
    counters: Arc<ExporterCounters>,
) {
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut consecutive_failures = 0_u32;
    loop {
        match receiver.recv_timeout(config.batch_interval) {
            Ok(ExporterMessage::Span(span)) => {
                counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                batch.push(span);
                while batch.len() < config.batch_size {
                    match receiver.try_recv() {
                        Ok(ExporterMessage::Span(span)) => {
                            counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                            batch.push(span);
                        }
                        Ok(ExporterMessage::Shutdown) => {
                            flush_batch(
                                &client,
                                &headers,
                                &config.endpoint,
                                &mut batch,
                                &counters,
                                &mut consecutive_failures,
                            );
                            return;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            flush_batch(
                                &client,
                                &headers,
                                &config.endpoint,
                                &mut batch,
                                &counters,
                                &mut consecutive_failures,
                            );
                            return;
                        }
                    }
                }
                if batch.len() >= config.batch_size {
                    flush_batch(
                        &client,
                        &headers,
                        &config.endpoint,
                        &mut batch,
                        &counters,
                        &mut consecutive_failures,
                    );
                }
            }
            Ok(ExporterMessage::Shutdown) => {
                flush_batch(
                    &client,
                    &headers,
                    &config.endpoint,
                    &mut batch,
                    &counters,
                    &mut consecutive_failures,
                );
                return;
            }
            Err(RecvTimeoutError::Timeout) => flush_batch(
                &client,
                &headers,
                &config.endpoint,
                &mut batch,
                &counters,
                &mut consecutive_failures,
            ),
            Err(RecvTimeoutError::Disconnected) => {
                flush_batch(
                    &client,
                    &headers,
                    &config.endpoint,
                    &mut batch,
                    &counters,
                    &mut consecutive_failures,
                );
                return;
            }
        }
    }
}

fn flush_batch(
    client: &Client,
    headers: &HeaderMap,
    endpoint: &str,
    batch: &mut Vec<ExportSpan>,
    counters: &ExporterCounters,
    consecutive_failures: &mut u32,
) {
    if batch.is_empty() {
        return;
    }
    let span_count = batch.len() as u64;
    let payload = otlp_payload(batch);
    batch.clear();
    let result = client
        .post(endpoint)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .and_then(|response| response.error_for_status());
    match result {
        Ok(_) => {
            counters
                .exported_spans
                .fetch_add(span_count, Ordering::Relaxed);
            *consecutive_failures = 0;
        }
        Err(error) => {
            counters.failed_batches.fetch_add(1, Ordering::Relaxed);
            counters
                .dropped_spans
                .fetch_add(span_count, Ordering::Relaxed);
            *consecutive_failures = consecutive_failures.saturating_add(1);
            let shift = (*consecutive_failures).min(6);
            let backoff = Duration::from_millis(100_u64.saturating_mul(1_u64 << shift));
            tracing::warn!(
                endpoint,
                failed_spans = span_count,
                backoff_ms = backoff.as_millis() as u64,
                error = %error,
                "OTLP trace export failed"
            );
            thread::sleep(backoff);
        }
    }
}

fn otlp_payload(batch: &[ExportSpan]) -> Value {
    let spans = batch.iter().map(otlp_span).collect::<Vec<_>>();
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": {"stringValue": SERVICE_NAME}
                }]
            },
            "scopeSpans": [{
                "scope": {"name": SCOPE_NAME},
                "spans": spans
            }]
        }]
    })
}

fn otlp_span(item: &ExportSpan) -> Value {
    let mut attributes = Vec::new();
    if let Ok(Value::Object(values)) = serde_json::to_value(&item.span.attributes) {
        for (key, value) in values {
            if let Some(value) = otlp_attribute_value(value) {
                attributes.push(json!({
                    "key": format!("holon.{key}"),
                    "value": value
                }));
            }
        }
    }
    attributes.push(json!({
        "key": "holon.duration_us",
        "value": {"intValue": item.span.duration_us.to_string()}
    }));
    let mut span = json!({
        "traceId": item.context.trace_id,
        "spanId": item.span.span_id,
        "name": item.span.name,
        "kind": 1,
        "startTimeUnixNano": timestamp_nanos(item.span.started_at).to_string(),
        "endTimeUnixNano": timestamp_nanos(item.span.completed_at).to_string(),
        "attributes": attributes,
        "flags": item.context.trace_flags,
        "status": {
            "code": match item.span.status {
                TraceSpanStatus::Ok => 1,
                TraceSpanStatus::Error => 2,
            }
        }
    });
    if let Some(parent_span_id) = item.span.parent_span_id.as_ref() {
        span["parentSpanId"] = Value::String(parent_span_id.clone());
    }
    if let Some(trace_state) = item.context.trace_state.as_ref() {
        span["traceState"] = Value::String(trace_state.clone());
    }
    span
}

fn otlp_attribute_value(value: Value) -> Option<Value> {
    match value {
        Value::String(value) => Some(json!({"stringValue": value})),
        Value::Number(value) if value.is_i64() || value.is_u64() => {
            Some(json!({"intValue": value.to_string()}))
        }
        Value::Number(value) => value.as_f64().map(|value| json!({"doubleValue": value})),
        Value::Bool(value) => Some(json!({"boolValue": value})),
        _ => None,
    }
}

fn timestamp_nanos(value: chrono::DateTime<chrono::Utc>) -> u64 {
    value
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .max(0)
        .try_into()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use chrono::Utc;

    use super::*;
    use crate::observability::TraceAttributes;

    #[test]
    fn otlp_attribute_value_preserves_numeric_types() {
        assert_eq!(
            otlp_attribute_value(json!(-7)),
            Some(json!({"intValue": "-7"}))
        );
        assert_eq!(
            otlp_attribute_value(json!(u64::MAX)),
            Some(json!({"intValue": u64::MAX.to_string()}))
        );
        assert_eq!(
            otlp_attribute_value(json!(3.25)),
            Some(json!({"doubleValue": 3.25}))
        );
    }

    #[test]
    fn exporter_failure_is_counted_and_shutdown_drains_worker() {
        let config = OtlpExporterConfig {
            endpoint: "http://127.0.0.1:9/v1/traces".to_string(),
            queue_capacity: 4,
            batch_size: 1,
            batch_interval: Duration::from_millis(10),
            timeout: Duration::from_millis(100),
            ..OtlpExporterConfig::default()
        };
        let exporter = OtlpExporter::start(config).unwrap();
        let handle = exporter.handle.as_ref().unwrap().clone();
        let now = Utc::now();
        handle.try_send(ExportSpan {
            context: TraceContext {
                trace_id: "0123456789abcdef0123456789abcdef".to_string(),
                span_id: "0123456789abcdef".to_string(),
                trace_flags: 1,
                trace_state: None,
            },
            span: TraceSpan {
                name: "test".to_string(),
                span_id: "fedcba9876543210".to_string(),
                parent_span_id: None,
                started_at: now,
                completed_at: now,
                duration_us: 1,
                status: TraceSpanStatus::Ok,
                attributes: TraceAttributes::default(),
            },
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let stats = exporter.stats();
            if stats.failed_batches == 1 {
                assert_eq!(stats.queued_spans, 1);
                assert_eq!(stats.exported_spans, 0);
                assert_eq!(stats.dropped_spans, 1);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "OTLP failure was not observed: {stats:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }

        exporter.shutdown().unwrap();
    }
}
