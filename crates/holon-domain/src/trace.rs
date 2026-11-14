use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

const TRACE_ID_HEX_LEN: usize = 32;
const SPAN_ID_HEX_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceContextError {
    InvalidTraceParent,
    UnsupportedVersion,
    InvalidTraceState,
}

impl fmt::Display for TraceContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTraceParent => formatter.write_str("invalid W3C traceparent"),
            Self::UnsupportedVersion => formatter.write_str("unsupported W3C traceparent version"),
            Self::InvalidTraceState => formatter.write_str("invalid W3C tracestate"),
        }
    }
}

impl std::error::Error for TraceContextError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
}

impl TraceContext {
    pub fn new_root(sampled: bool) -> Self {
        Self {
            trace_id: Uuid::new_v4().simple().to_string(),
            span_id: random_span_id(),
            trace_flags: u8::from(sampled),
            trace_state: None,
        }
    }

    pub fn parse(trace_parent: &str, trace_state: Option<&str>) -> Result<Self, TraceContextError> {
        let mut parts = trace_parent.trim().split('-');
        let version = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let trace_id = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let span_id = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let trace_flags = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        if parts.next().is_some() || version.len() != 2 || !is_lower_hex(version) || version == "ff"
        {
            return Err(TraceContextError::InvalidTraceParent);
        }
        if version != "00" {
            return Err(TraceContextError::UnsupportedVersion);
        }
        if !valid_nonzero_hex(trace_id, TRACE_ID_HEX_LEN)
            || !valid_nonzero_hex(span_id, SPAN_ID_HEX_LEN)
            || trace_flags.len() != 2
            || !is_lower_hex(trace_flags)
        {
            return Err(TraceContextError::InvalidTraceParent);
        }
        let trace_flags = u8::from_str_radix(trace_flags, 16)
            .map_err(|_| TraceContextError::InvalidTraceParent)?;
        let trace_state = trace_state
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(validate_trace_state)
            .transpose()?;
        Ok(Self {
            trace_id: trace_id.to_owned(),
            span_id: span_id.to_owned(),
            trace_flags,
            trace_state,
        })
    }

    pub fn trace_parent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }

    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: random_span_id(),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
        }
    }

    pub fn sampled(&self) -> bool {
        self.trace_flags & 1 == 1
    }
}

fn random_span_id() -> String {
    Uuid::new_v4().as_bytes()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_nonzero_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && is_lower_hex(value)
        && value.as_bytes().iter().any(|byte| *byte != b'0')
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_trace_state(value: &str) -> Result<String, TraceContextError> {
    if value.len() > 512 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TraceContextError::InvalidTraceState);
    }
    Ok(value.to_owned())
}
