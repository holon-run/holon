//! Tool schema specifications
//!
//! This module defines the public types used to describe tools and their input/output schemas.

use anyhow::Result;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::ToolError;

/// A tool specification describing name, description, and input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform_grammar: Option<ToolFreeformGrammar>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolFreeformGrammar {
    pub syntax: String,
    pub definition: String,
}

/// A tool call from the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub struct CompletionReportCandidate {
    pub text: String,
    pub citations: Vec<crate::types::Citation>,
    pub source_turn_index: u64,
    pub source_round: usize,
    pub source_turn_id: Option<String>,
    pub source_message_id: Option<String>,
    pub source_assistant_round_id: String,
    pub source_tool_call_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ToolExecutionContext {
    pub completion_report_candidate: Option<CompletionReportCandidate>,
}

/// Result from executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub envelope: ToolResultEnvelope,
    pub should_sleep: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal_transition: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_duration_ms: Option<u64>,
    #[serde(skip)]
    pub(crate) prepared_work_item_completion:
        Option<Box<crate::runtime::PreparedWorkItemCompletion>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultEnvelope {
    pub tool_name: String,
    pub status: ToolResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

impl ToolResult {
    pub fn success(
        tool_name: impl Into<String>,
        result: Value,
        summary_text: Option<String>,
    ) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Success,
            summary_text,
            result: Some(result),
            error: None,
        };
        Self {
            envelope,
            should_sleep: false,
            terminal_transition: false,
            sleep_duration_ms: None,
            prepared_work_item_completion: None,
        }
    }

    pub fn sleep(
        tool_name: impl Into<String>,
        result: Value,
        summary_text: Option<String>,
        sleep_duration_ms: Option<u64>,
    ) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Success,
            summary_text,
            result: Some(result),
            error: None,
        };
        Self {
            envelope,
            should_sleep: true,
            terminal_transition: false,
            sleep_duration_ms,
            prepared_work_item_completion: None,
        }
    }

    pub fn error(tool_name: impl Into<String>, error: ToolError) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Error,
            summary_text: Some(error.message.clone()),
            result: None,
            error: Some(error.clone()),
        };
        Self {
            envelope,
            should_sleep: false,
            terminal_transition: false,
            sleep_duration_ms: None,
            prepared_work_item_completion: None,
        }
    }

    pub fn content_text(&self) -> Result<String> {
        serde_json::to_string(&self.envelope).map_err(Into::into)
    }

    pub fn is_error(&self) -> bool {
        matches!(self.envelope.status, ToolResultStatus::Error)
    }

    pub fn tool_error(&self) -> Option<&ToolError> {
        self.envelope.error.as_ref()
    }

    pub fn summary_text(&self) -> Option<&str> {
        self.envelope.summary_text.as_deref()
    }
}

/// Helper to build a ToolSpec.
pub(crate) fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        freeform_grammar: None,
    }
}

/// Helper to build a ToolSpec from a typed input schema.
pub(crate) fn typed_spec<T: schemars::JsonSchema + 'static>(
    name: &str,
    description: &str,
) -> Result<ToolSpec> {
    Ok(spec(
        name,
        description,
        crate::tool::schema::tool_input_schema::<T>()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_builder() {
        let tool_spec = spec(
            "TestTool",
            "A test tool",
            serde_json::json!({"type": "object"}),
        );

        assert_eq!(tool_spec.name, "TestTool");
        assert_eq!(tool_spec.description, "A test tool");
    }
}
