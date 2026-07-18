use anyhow::Error as AnyhowError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::runtime_error::{
    collect_runtime_error_source_chain, describe_runtime_error, RuntimeErrorDomain,
};

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

    pub fn from_anyhow(error: &AnyhowError) -> Self {
        if let Some(mut tool_error) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ToolError>())
            .cloned()
        {
            if tool_error.source_chain.is_empty() {
                tool_error.source_chain = collect_runtime_error_source_chain(error);
            }
            return tool_error;
        }

        let descriptor = describe_runtime_error(error);
        let details = (!descriptor.safe_context.is_empty()).then(|| json!(descriptor.safe_context));
        let kind = if descriptor.domain == RuntimeErrorDomain::Unknown
            && descriptor.code == "runtime_error"
        {
            "tool_execution_failed".to_string()
        } else {
            descriptor.code
        };
        Self {
            kind,
            message: descriptor.operator_message,
            domain: Some(descriptor.domain),
            details,
            recovery_hint: descriptor.recovery_hint,
            retryable: descriptor.retryable,
            source_chain: descriptor.source_chain,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_renders_as_pretty_json() {
        let rendered = ToolError::new("invalid_tool_input", "missing required string field")
            .with_details(json!({ "field": "cmd" }))
            .with_recovery_hint("provide a string value for `cmd`")
            .render();

        assert!(rendered.contains("\"kind\": \"invalid_tool_input\""));
        assert!(rendered.contains("\"field\": \"cmd\""));
        assert!(rendered.contains("provide a string value"));
    }

    #[test]
    fn tool_error_model_rendering_uses_shared_corrective_receipt() {
        let rendered = ToolError::new("invalid_tool_input", "missing required string field")
            .with_details(json!({ "field": "cmd", "parse_error": "missing field `cmd`" }))
            .with_recovery_hint("provide a string value for `cmd`")
            .render_for_model(Some("ExecCommand"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["tool_name"], "ExecCommand");
        assert_eq!(receipt["kind"], "invalid_tool_input");
        assert_eq!(receipt["message"], "missing required string field");
        assert_eq!(receipt["hint"], "provide a string value for `cmd`");
        assert_eq!(receipt["field"], "cmd");
        assert_eq!(receipt["retryable"], false);
        assert_eq!(receipt["details"]["parse_error"], "missing field `cmd`");
    }

    #[test]
    fn tool_error_model_rendering_supports_runtime_failures() {
        let rendered = ToolError::new("command_spawn_failed", "failed to start command process")
            .with_recovery_hint("check the command path and arguments")
            .with_retryable(true)
            .render_for_model(Some("ExecCommand"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["kind"], "command_spawn_failed");
        assert_eq!(receipt["hint"], "check the command path and arguments");
        assert_eq!(receipt["retryable"], true);
    }

    #[test]
    fn tool_error_model_rendering_bounds_long_details_without_changing_canonical_error() {
        let raw_payload = "x".repeat(MODEL_VISIBLE_DETAILS_MAX_CHARS + 500);
        let error = ToolError::new("invalid_tool_input", "malformed tool input")
            .with_details(json!({ "raw_input": raw_payload.clone() }));
        let canonical = error.render();
        let rendered = error.render_for_model(Some("ApplyPatch"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert!(canonical.contains(&raw_payload));
        assert_eq!(receipt["details"]["omitted"], true);
        assert_eq!(
            receipt["details"]["reason"],
            "details exceeded model-visible budget"
        );
        assert!(receipt["details"]["sha256"].as_str().unwrap().len() >= 64);
        assert!(
            receipt["details"]["preview"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= MODEL_VISIBLE_DETAILS_MAX_CHARS
        );
        assert!(!rendered.contains(&raw_payload));
    }

    #[test]
    fn tool_error_from_anyhow_finds_wrapped_tool_error() {
        let error = anyhow::Error::from(
            ToolError::new(
                "execution_root_violation",
                "requested working directory is outside the current execution root",
            )
            .with_recovery_hint("omit `workdir`"),
        )
        .context("failed to resolve command task");
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "execution_root_violation");
        assert_eq!(tool_error.recovery_hint.as_deref(), Some("omit `workdir`"));
        assert!(tool_error
            .source_chain
            .iter()
            .any(|message| message == "failed to resolve command task"));
    }

    #[test]
    fn tool_error_from_anyhow_projects_typed_runtime_error() {
        let error = anyhow::Error::from(
            crate::runtime_error::RuntimeError::not_found(
                "task_not_found",
                "task task_123 not found",
            )
            .with_safe_context("task_id", "task_123"),
        )
        .context("failed to read task");

        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "task_not_found");
        assert_eq!(tool_error.domain, Some(RuntimeErrorDomain::NotFound));
        assert_eq!(tool_error.details.as_ref().unwrap()["task_id"], "task_123");
        assert!(tool_error
            .source_chain
            .iter()
            .any(|message| message == "failed to read task"));
    }

    #[test]
    fn tool_error_from_anyhow_preserves_unknown_fallback_code() {
        let tool_error = ToolError::from_anyhow(&anyhow::anyhow!("unclassified failure"));

        assert_eq!(tool_error.kind, "tool_execution_failed");
        assert_eq!(tool_error.domain, Some(RuntimeErrorDomain::Unknown));
        assert!(!tool_error.retryable);
    }

    #[test]
    fn tool_error_deserializes_legacy_shape_with_taxonomy_defaults() {
        let tool_error: ToolError = serde_json::from_value(serde_json::json!({
            "kind": "tool_execution_failed",
            "message": "legacy failure",
            "retryable": false
        }))
        .unwrap();

        assert_eq!(tool_error.domain, None);
        assert!(tool_error.source_chain.is_empty());
    }
}
