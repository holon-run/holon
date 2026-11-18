//! Tool schema specifications
//!
//! This module defines the public types used to describe tools and their input/output schemas.

use anyhow::Result;
use std::sync::{atomic::AtomicUsize, Arc};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use holon_domain::ToolError;

pub use holon_domain::{ToolFreeformGrammar, ToolSpec};

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

#[derive(Debug)]
pub struct ExternalCommandAttempt {
    pub assistant_round_id: String,
    pub model_output_budget: usize,
    pub submitted: std::sync::OnceLock<crate::types::ToolExecutionRecord>,
}

#[derive(Debug, thiserror::Error)]
#[error("external command attempt registration failed")]
pub struct ExternalCommandRegistrationFailed;

#[derive(Debug, Clone, Default)]
pub struct ToolExecutionContext {
    pub completion_report_candidate: Option<CompletionReportCandidate>,
    pub effective_work_item_id: Option<String>,
    pub turn_id: Option<String>,
    pub message_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub trace_context: Option<holon_domain::TraceContext>,
    pub external_command_attempt: Option<Arc<ExternalCommandAttempt>>,
    pub decision_tool_calls: Arc<AtomicUsize>,
}

#[derive(Debug, Clone)]
pub struct AwaitCompletionReportDirective {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_work_revision: u64,
    pub warnings: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct AwaitWaitReportDirective {
    pub input: Value,
}

#[derive(Debug, Clone)]
pub enum ToolLoopDirective {
    AwaitCompletionReport(AwaitCompletionReportDirective),
    AwaitWaitReport(AwaitWaitReportDirective),
}

/// Result from executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome<State> {
    pub envelope: ToolResultEnvelope,
    pub should_sleep: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal_transition: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_duration_ms: Option<u64>,
    #[serde(skip)]
    pub loop_directive: Option<ToolLoopDirective>,
    #[serde(skip)]
    pub runtime_state: State,
}

pub type ToolResult = ToolOutcome<()>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Deferred,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolInputCoercion {
    UnwrapToolInputEnvelope {
        envelope_key: String,
        outer_keys: Vec<String>,
        inner_keys: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultEnvelope {
    pub tool_name: String,
    pub status: ToolResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_coercion: Option<ToolInputCoercion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

impl<State: Default> ToolOutcome<State> {
    pub fn success(
        tool_name: impl Into<String>,
        result: Value,
        summary_text: Option<String>,
    ) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Success,
            summary_text,
            input_coercion: None,
            result: Some(result),
            error: None,
        };
        Self {
            envelope,
            should_sleep: false,
            terminal_transition: false,
            sleep_duration_ms: None,
            loop_directive: None,
            runtime_state: State::default(),
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
            input_coercion: None,
            result: Some(result),
            error: None,
        };
        Self {
            envelope,
            should_sleep: true,
            terminal_transition: false,
            sleep_duration_ms,
            loop_directive: None,
            runtime_state: State::default(),
        }
    }

    pub fn deferred(
        tool_name: impl Into<String>,
        result: Value,
        summary_text: Option<String>,
        directive: ToolLoopDirective,
    ) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Deferred,
            summary_text,
            input_coercion: None,
            result: Some(result),
            error: None,
        };
        Self {
            envelope,
            should_sleep: false,
            terminal_transition: false,
            sleep_duration_ms: None,
            loop_directive: Some(directive),
            runtime_state: State::default(),
        }
    }

    pub fn error(tool_name: impl Into<String>, error: ToolError) -> Self {
        let envelope = ToolResultEnvelope {
            tool_name: tool_name.into(),
            status: ToolResultStatus::Error,
            summary_text: Some(error.message.clone()),
            input_coercion: None,
            result: None,
            error: Some(error.clone()),
        };
        Self {
            envelope,
            should_sleep: false,
            terminal_transition: false,
            sleep_duration_ms: None,
            loop_directive: None,
            runtime_state: State::default(),
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
pub fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        freeform_grammar: None,
    }
}

/// Helper to build a ToolSpec from a typed input schema.
pub fn typed_spec<T: schemars::JsonSchema + 'static>(
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
mod tests;
