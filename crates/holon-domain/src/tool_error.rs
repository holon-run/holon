use crate::RuntimeErrorDomain;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MODEL_VISIBLE_DETAILS_MAX_CHARS: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<RuntimeErrorDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<String>,
}

impl ToolError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            domain: None,
            details: None,
            recovery_hint: None,
            retryable: false,
            source_chain: Vec::new(),
        }
    }

    pub fn with_domain(mut self, domain: RuntimeErrorDomain) -> Self {
        self.domain = Some(domain);
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_recovery_hint(mut self, recovery_hint: impl Into<String>) -> Self {
        self.recovery_hint = Some(recovery_hint.into());
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_source_chain(mut self, source_chain: Vec<String>) -> Self {
        self.source_chain = source_chain;
        self
    }

    pub fn render(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            format!(
                "{{\"kind\":\"{}\",\"message\":\"{}\",\"retryable\":{}}}",
                self.kind, self.message, self.retryable
            )
        })
    }

    pub fn render_for_model(&self, tool_name: Option<&str>) -> String {
        let mut receipt = serde_json::Map::new();
        receipt.insert("ok".to_string(), Value::Bool(false));
        if let Some(tool_name) = tool_name {
            receipt.insert(
                "tool_name".to_string(),
                Value::String(tool_name.to_string()),
            );
        }
        receipt.insert("kind".to_string(), Value::String(self.kind.clone()));
        receipt.insert("message".to_string(), Value::String(self.message.clone()));
        if let Some(domain) = self.domain {
            receipt.insert("domain".to_string(), json!(domain));
        }
        if let Some(recovery_hint) = self.recovery_hint.as_deref() {
            receipt.insert("hint".to_string(), Value::String(recovery_hint.to_string()));
        }
        if let Some(field) = self
            .details
            .as_ref()
            .and_then(|details| details.get("field"))
            .and_then(Value::as_str)
        {
            receipt.insert("field".to_string(), Value::String(field.to_string()));
        }
        receipt.insert("retryable".to_string(), Value::Bool(self.retryable));
        if let Some(details) = self.details.as_ref() {
            receipt.insert(
                "details".to_string(),
                bounded_model_visible_details(details),
            );
        }
        serde_json::to_string_pretty(&Value::Object(receipt)).unwrap_or_else(|_| {
            format!(
                "{{\"ok\":false,\"kind\":\"{}\",\"message\":\"{}\",\"retryable\":{}}}",
                self.kind, self.message, self.retryable
            )
        })
    }

    pub fn audit_fields(&self) -> Value {
        json!({
            "error": self.render(),
            "error_kind": self.kind,
            "tool_error": self,
        })
    }
}

fn bounded_model_visible_details(details: &Value) -> Value {
    let rendered = serde_json::to_string(details).unwrap_or_else(|_| details.to_string());
    if rendered.chars().count() <= MODEL_VISIBLE_DETAILS_MAX_CHARS {
        return details.clone();
    }

    let digest = Sha256::digest(rendered.as_bytes());
    let preview: String = rendered
        .chars()
        .take(MODEL_VISIBLE_DETAILS_MAX_CHARS)
        .collect();
    json!({
        "omitted": true,
        "reason": "details exceeded model-visible budget",
        "char_count": rendered.chars().count(),
        "sha256": format!("{digest:x}"),
        "preview": preview,
    })
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render())
    }
}

impl std::error::Error for ToolError {}
