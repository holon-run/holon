use anyhow::Error as AnyhowError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            details: None,
            recovery_hint: None,
            retryable: false,
        }
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

    pub fn render(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            format!(
                "{{\"kind\":\"{}\",\"message\":\"{}\",\"retryable\":{}}}",
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
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ToolError>())
            .cloned()
            .unwrap_or_else(|| {
                ToolError::new("tool_execution_failed", error.to_string()).with_retryable(false)
            })
    }
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
    }
}
