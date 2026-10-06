//! Small, provider-neutral contracts for model clients.
//!
//! This crate owns one model call and its provider-specific wire adapter. It
//! deliberately does not own route selection, fallback, budgets, transcripts,
//! tool execution, or agent/session lifecycle. Those policies belong to the
//! application embedding the client.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use thiserror::Error;

#[cfg(feature = "openai-compatible")]
mod openai_compatible;

#[cfg(feature = "openai-compatible")]
pub use openai_compatible::{parse_response, OpenAiCompatibleClient, OpenAiCompatibleConfig};

/// Context supplied by the host runtime for one provider attempt.
///
/// The fields are intentionally opaque to the client. A host may use them for
/// tracing, cancellation, route attribution, or request correlation without
/// making those concerns part of the client contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallContext {
    pub request_id: Option<String>,
    pub turn_id: Option<String>,
    pub route_identity: Option<String>,
    pub attempt_number: u32,
    pub timeout: Option<Duration>,
    pub headers: BTreeMap<String, String>,
}

/// A completion request independent of any one provider's route syntax.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
    pub tools: Vec<ToolDefinition>,
    pub response_format: Option<ResponseFormat>,
    pub continuation: Option<ContinuationState>,
    pub extensions: RequestExtensions,
    pub stream: bool,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_output_tokens: None,
            tools: Vec::new(),
            response_format: None,
            continuation: None,
            extensions: RequestExtensions::default(),
            stream: false,
        }
    }
}

/// A chat message. Tool calls are represented on [`CompletionResponse`] and
/// tool results are sent with the `Tool` role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_call_id: None,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    pub fn tool(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResponseFormat {
    JsonSchema {
        name: String,
        schema: Value,
        strict: bool,
    },
}

/// Provider-native continuation state.
///
/// The client may create or consume this state, but the host decides whether
/// it is valid to reuse it across turns or route changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub opaque: Value,
}

impl ContinuationState {
    pub fn response_id(response_id: impl Into<String>) -> Self {
        Self {
            response_id: Some(response_id.into()),
            token: None,
            opaque: Value::Null,
        }
    }
}

/// Explicit provider extensions. The common contract stays small while
/// provider-specific features remain available without changing route policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestExtensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_web_search: Option<NativeWebSearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheDirective>,
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeWebSearch {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheDirective {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub message: Message,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub continuation: Option<ContinuationState>,
    #[serde(default)]
    pub extensions: ResponseExtensions,
    #[serde(default)]
    pub provider_data: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseExtensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
}

/// A provider client performs one model attempt. Route chains, retries, and
/// fallback remain outside this trait.
#[async_trait]
pub trait ModelClient: Send + Sync {
    fn provider_name(&self) -> &str;

    async fn complete(
        &self,
        context: &CallContext,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError>;
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid client configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid completion request: {0}")]
    InvalidRequest(String),
    #[error("unsupported client capability: {0}")]
    Unsupported(String),
    #[error("request transport failed: {0}")]
    Transport(String),
    #[error("provider returned HTTP {status} ({request_id:?}): {body}")]
    Http {
        status: u16,
        request_id: Option<String>,
        body: String,
    },
    #[error("provider response could not be decoded: {0}")]
    Decode(String),
}
