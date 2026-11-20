use std::collections::BTreeMap;

use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    http::StatusCode,
    Json,
};
use holon_domain::{RuntimeErrorContext, RuntimeErrorDescriptor, RuntimeErrorDomain};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HttpErrorEnvelope {
    ok: bool,
    error: String,
    code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    domain: Option<RuntimeErrorDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    context: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "RuntimeErrorContext::is_empty")]
    correlation: Box<RuntimeErrorContext>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl HttpErrorEnvelope {
    pub fn new(code: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: error.into(),
            code: code.into(),
            hint: None,
            domain: None,
            retryable: None,
            context: BTreeMap::new(),
            correlation: Box::default(),
            extensions: Map::new(),
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }

    pub fn context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    pub fn extension(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }

    pub fn domain(mut self, domain: RuntimeErrorDomain) -> Self {
        self.domain = Some(domain);
        self
    }

    pub fn from_descriptor(descriptor: RuntimeErrorDescriptor) -> Self {
        let mut envelope = Self::new(descriptor.code, descriptor.operator_message);
        envelope.hint = descriptor.recovery_hint;
        envelope.domain = Some(descriptor.domain);
        envelope.retryable = Some(descriptor.retryable);
        envelope.correlation.message_id = descriptor.safe_context.get("message_id").cloned();
        envelope.correlation.turn_id = descriptor.safe_context.get("turn_id").cloned();
        envelope.correlation.run_id = descriptor.safe_context.get("run_id").cloned();
        envelope.correlation.work_item_id = descriptor.safe_context.get("work_item_id").cloned();
        envelope.correlation.tool_execution_id =
            descriptor.safe_context.get("tool_execution_id").cloned();
        envelope.correlation.task_id = descriptor.safe_context.get("task_id").cloned();
        envelope.correlation.provider = descriptor.safe_context.get("provider").cloned();
        envelope.correlation.model_ref = descriptor.safe_context.get("model_ref").cloned();
        envelope.context = descriptor.safe_context;
        envelope
    }
}

pub async fn extract_json<T, S>(request: Request, state: &S) -> Result<T, (StatusCode, Json<Value>)>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    Json::<T>::from_request(request, state)
        .await
        .map(|Json(value)| value)
        .map_err(json_rejection_response)
}

fn json_rejection_response(rejection: JsonRejection) -> (StatusCode, Json<Value>) {
    let status = rejection.status();
    let detail = rejection.to_string();
    let (code, error, include_detail) = match rejection {
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => (
            "invalid_json",
            "request body must be valid JSON matching the expected shape",
            true,
        ),
        JsonRejection::MissingJsonContentType(_) => (
            "missing_json_content_type",
            "request must use Content-Type: application/json",
            false,
        ),
        JsonRejection::BytesRejection(_) => (
            "request_body_rejected",
            "request body could not be read",
            false,
        ),
        _ => ("request_body_rejected", "request body was rejected", false),
    };
    let mut envelope = HttpErrorEnvelope::new(code, error);
    if include_detail {
        envelope.context.insert("detail".to_owned(), detail);
    }
    http_error(status, envelope)
}

pub fn runtime_error_status(descriptor: &RuntimeErrorDescriptor) -> StatusCode {
    if descriptor.code == "runtime_db_quarantined" {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    match descriptor.domain {
        RuntimeErrorDomain::Validation => StatusCode::BAD_REQUEST,
        RuntimeErrorDomain::NotFound => StatusCode::NOT_FOUND,
        RuntimeErrorDomain::Conflict => StatusCode::CONFLICT,
        RuntimeErrorDomain::Policy => StatusCode::FORBIDDEN,
        RuntimeErrorDomain::Provider => {
            if descriptor.retryable {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            }
        }
        RuntimeErrorDomain::Storage | RuntimeErrorDomain::Io if descriptor.retryable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn http_error(status: StatusCode, envelope: HttpErrorEnvelope) -> (StatusCode, Json<Value>) {
    let value = serde_json::to_value(envelope).expect("HTTP error envelope serializes");
    (status, Json(value))
}

pub fn forbidden(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::FORBIDDEN,
        HttpErrorEnvelope::new("forbidden", reason),
    )
}

pub fn bad_request(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::BAD_REQUEST,
        HttpErrorEnvelope::new("bad_request", reason),
    )
}

pub fn service_unavailable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new("service_unavailable", reason),
    )
}

pub fn not_found(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new("not_found", reason),
    )
}

#[cfg(test)]
mod tests;
