use crate::ids;
use crate::tool_error::ToolError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Ref-backed tool result metadata for transcript storage.
/// Stores stable identifiers and bounded provider-visible content rather than full tool output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResultRef {
    /// Stable tool call id from assistant tool use block
    pub tool_call_id: String,
    /// Stable tool execution record id for full content resolution
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_execution_id: Option<String>,
    /// Provider-visible bounded content (truncated or summary)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_visible_text: Option<String>,
    /// Whether the full content was truncated from provider view
    #[serde(default)]
    pub content_truncated: bool,
    /// Error marker
    #[serde(default)]
    pub is_error: bool,
}

/// Legacy tool result block format for backward compatibility.
/// Used when reading old transcripts that store full content directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LegacyToolResultBlock {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

/// Unified tool result data format in transcript.
/// Supports both new ref-backed format and legacy full-content format.
///
/// # Serialization formats
///
/// ## New ref-backed format
/// ```json
/// {
///   "refs": [
///     {
///       "tool_call_id": "...",
///       "tool_execution_id": "...",
///       "provider_visible_text": "...",
///       "content_truncated": false,
///       "is_error": false
///     }
///   ]
/// }
/// ```
///
/// ## Legacy format
/// ```json
/// {
///   "results": [
///     { "tool_use_id": "...", "content": "...", "is_error": false }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolResultData {
    /// New ref-backed format with explicit wrapper
    RefsWithWrapper {
        /// The owning turn for new transcript entries; absent in legacy data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        refs: Vec<ToolResultRef>,
    },
    /// Legacy format with explicit wrapper
    LegacyWithWrapper { results: Vec<LegacyToolResultBlock> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptEntryKind {
    IncomingMessage,
    AssistantRound,
    ToolResults,
    RuntimeFailure,
    ContinuationPrompt,
    SubagentPrompt,
    SubagentAssistantRound,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantRoundPurpose {
    AgentResponse,
    RuntimeCheckpoint,
}

impl AssistantRoundPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentResponse => "agent_response",
            Self::RuntimeCheckpoint => "runtime_checkpoint",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_seq: Option<u64>,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub kind: TranscriptEntryKind,
    pub round: Option<usize>,
    pub related_message_id: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub data: Value,
}

impl TranscriptEntry {
    pub fn new(
        agent_id: impl Into<String>,
        kind: TranscriptEntryKind,
        round: Option<usize>,
        related_message_id: Option<String>,
        data: Value,
    ) -> Self {
        Self {
            id: ids::transcript_entry_id(),
            transcript_seq: None,
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            kind,
            round,
            related_message_id,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            data,
        }
    }
}

pub fn transcript_text(entry: &TranscriptEntry) -> Option<String> {
    transcript_text_projection(entry).map(TranscriptTextProjection::into_text)
}

/// Borrow only the selected text; unrelated transcript JSON is not materialized.
pub enum TranscriptTextProjection<'a> {
    Body(&'a str),
    Blocks(&'a [Value]),
}

impl serde::Serialize for TranscriptTextProjection<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Body(text) => serde::Serialize::serialize(text, serializer),
            Self::Blocks(blocks) => serializer.collect_seq(text_blocks(blocks)),
        }
    }
}

impl<'a> TranscriptTextProjection<'a> {
    pub fn borrowed_text(&self) -> Option<&'a str> {
        match self {
            Self::Body(text) => Some(text),
            Self::Blocks(blocks) => {
                let mut texts = text_blocks(blocks);
                let text = texts.next()?;
                texts.next().is_none().then_some(text)
            }
        }
    }

    pub fn into_text(self) -> String {
        match self {
            Self::Body(text) => text.to_owned(),
            Self::Blocks(blocks) => text_blocks(blocks).collect::<Vec<_>>().join(" "),
        }
    }
}

pub fn transcript_text_projection(entry: &TranscriptEntry) -> Option<TranscriptTextProjection<'_>> {
    match entry.kind {
        TranscriptEntryKind::AssistantRound
        | TranscriptEntryKind::ToolResults
        | TranscriptEntryKind::ContinuationPrompt
        | TranscriptEntryKind::SubagentPrompt
        | TranscriptEntryKind::SubagentAssistantRound => text_blocks_projection(&entry.data),
        TranscriptEntryKind::IncomingMessage | TranscriptEntryKind::RuntimeFailure => {
            text_from_message_body(&entry.data)
                .map(TranscriptTextProjection::Body)
                .or_else(|| text_blocks_projection(&entry.data))
        }
    }
}

fn text_blocks_projection(data: &Value) -> Option<TranscriptTextProjection<'_>> {
    let blocks = data.get("blocks")?.as_array()?;
    text_blocks(blocks)
        .any(|text| !text.trim().is_empty())
        .then_some(TranscriptTextProjection::Blocks(blocks))
}

fn text_blocks(blocks: &[Value]) -> impl Iterator<Item = &str> {
    blocks.iter().filter_map(|block| {
        let kind = block.get("type").and_then(Value::as_str)?;
        if kind != "text" {
            return None;
        }
        block
            .get("Text")
            .and_then(|value| value.get("text"))
            .or_else(|| block.get("text"))
            .and_then(Value::as_str)
    })
}

fn text_from_message_body(data: &Value) -> Option<&str> {
    data.get("body")
        .and_then(|body| {
            body.get("Text")
                .and_then(|value| value.get("text"))
                .or_else(|| body.get("Brief").and_then(|value| value.get("text")))
                .or_else(|| body.get("text"))
                .and_then(Value::as_str)
        })
        .filter(|text| !text.trim().is_empty())
}
