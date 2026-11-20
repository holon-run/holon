use axum::{
    http::{HeaderMap, StatusCode},
    Json,
};
use holon_observability::observability::TraceContext;
use serde_json::Value;

use crate::error::bad_request;

pub fn trace_context_from_headers(
    headers: &HeaderMap,
) -> Result<Option<TraceContext>, (StatusCode, Json<Value>)> {
    let Some(trace_parent) = headers.get("traceparent") else {
        return Ok(None);
    };
    let trace_parent = trace_parent
        .to_str()
        .map_err(|_| bad_request("invalid traceparent header"))?;
    let trace_state = headers
        .get("tracestate")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| bad_request("invalid tracestate header"))?;
    TraceContext::parse(trace_parent, trace_state)
        .map(Some)
        .map_err(|error| bad_request(error.to_string()))
}
