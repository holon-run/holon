use anyhow::{bail, Result};
use serde_json::Value;

use crate::{
    storage::AppStorage,
    types::{
        BriefContentSource, BriefRecord, MessageEnvelope, ToolExecutionRecord, TranscriptEntry,
        TranscriptEntryKind, TurnRecord,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRuntimeObject {
    Message(MessageEnvelope),
    TranscriptEntry(TranscriptEntry),
    Brief(ResolvedBrief),
    Turn(TurnRecord),
    ToolExecution(ResolvedToolExecution),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBrief {
    pub record: BriefRecord,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedToolExecution {
    pub record: ToolExecutionRecord,
    pub selector: Option<ToolExecutionSelector>,
    pub selected: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionSelector {
    Input,
    Output,
    Summary,
}

impl ToolExecutionSelector {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            "summary" => Ok(Self::Summary),
            _ => bail!("unsupported tool_execution selector: {value}"),
        }
    }
}

pub struct RuntimeObjectResolver<'a> {
    storage: &'a AppStorage,
}

impl<'a> RuntimeObjectResolver<'a> {
    pub fn new(storage: &'a AppStorage) -> Self {
        Self { storage }
    }

    pub fn resolve_ref(&self, object_ref: &str) -> Result<Option<ResolvedRuntimeObject>> {
        let Some((kind, rest)) = object_ref.split_once(':') else {
            bail!("runtime object ref must use kind:id syntax: {object_ref}");
        };
        if rest.trim().is_empty() {
            bail!("runtime object ref is missing id: {object_ref}");
        }

        match kind {
            "message" => Ok(self
                .resolve_message(rest)?
                .map(ResolvedRuntimeObject::Message)),
            "transcript" => Ok(self
                .resolve_transcript_entry(rest)?
                .map(ResolvedRuntimeObject::TranscriptEntry)),
            "brief" => Ok(self.resolve_brief(rest)?.map(ResolvedRuntimeObject::Brief)),
            "turn" => Ok(self.resolve_turn(rest)?.map(ResolvedRuntimeObject::Turn)),
            "tool_execution" => Ok(self
                .resolve_tool_execution_ref(rest)?
                .map(ResolvedRuntimeObject::ToolExecution)),
            _ => bail!("unsupported runtime object ref kind: {kind}"),
        }
    }

    pub fn resolve_message(&self, message_id: &str) -> Result<Option<MessageEnvelope>> {
        self.storage.read_message_by_id(message_id)
    }

    pub fn resolve_transcript_entry(&self, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        self.storage.read_transcript_entry_by_id(entry_id)
    }

    pub fn resolve_transcript_text(&self, entry: &TranscriptEntry) -> Result<Option<String>> {
        if entry.kind == TranscriptEntryKind::IncomingMessage {
            if let Some(message_id) = entry.related_message_id.as_deref() {
                if let Some(message) = self.resolve_message(message_id)? {
                    return Ok(message_body_text(&message.body));
                }
            }
        }
        Ok(transcript_text(entry))
    }

    pub fn resolve_brief(&self, brief_id: &str) -> Result<Option<ResolvedBrief>> {
        let Some(record) = self.storage.read_brief_by_id(brief_id)? else {
            return Ok(None);
        };
        let content = self.resolve_brief_content(&record)?;
        Ok(Some(ResolvedBrief { record, content }))
    }

    pub fn resolve_brief_content(&self, brief: &BriefRecord) -> Result<String> {
        match &brief.content_source {
            BriefContentSource::Inline => Ok(brief.text.clone()),
            BriefContentSource::TranscriptEntry { entry_id, .. } => {
                let Some(entry) = self.resolve_transcript_entry(entry_id)? else {
                    return Ok(brief.text.clone());
                };
                Ok(self
                    .resolve_transcript_text(&entry)?
                    .unwrap_or_else(|| brief.text.clone()))
            }
        }
    }

    pub fn resolve_turn(&self, turn_id: &str) -> Result<Option<TurnRecord>> {
        Ok(self
            .storage
            .read_recent_turns(usize::MAX)?
            .into_iter()
            .find(|record| record.turn_id == turn_id))
    }

    pub fn resolve_tool_execution_ref(&self, value: &str) -> Result<Option<ResolvedToolExecution>> {
        let mut parts = value.splitn(2, ':');
        let tool_execution_id = parts.next().unwrap_or_default();
        let selector = parts.next().map(ToolExecutionSelector::parse).transpose()?;
        self.resolve_tool_execution(tool_execution_id, selector)
    }

    pub fn resolve_tool_execution(
        &self,
        tool_execution_id: &str,
        selector: Option<ToolExecutionSelector>,
    ) -> Result<Option<ResolvedToolExecution>> {
        let Some(record) = self.storage.read_tool_execution_by_id(tool_execution_id)? else {
            return Ok(None);
        };
        let selected = selector.map(|selector| match selector {
            ToolExecutionSelector::Input => record.input.clone(),
            ToolExecutionSelector::Output => record.output.clone(),
            ToolExecutionSelector::Summary => Value::String(record.summary.clone()),
        });
        Ok(Some(ResolvedToolExecution {
            record,
            selector,
            selected,
        }))
    }
}

pub fn transcript_text(entry: &TranscriptEntry) -> Option<String> {
    match entry.kind {
        TranscriptEntryKind::AssistantRound
        | TranscriptEntryKind::ToolResults
        | TranscriptEntryKind::ContinuationPrompt
        | TranscriptEntryKind::SubagentPrompt
        | TranscriptEntryKind::SubagentAssistantRound => text_from_blocks(&entry.data),
        TranscriptEntryKind::IncomingMessage | TranscriptEntryKind::RuntimeFailure => {
            text_from_message_body(&entry.data).or_else(|| text_from_blocks(&entry.data))
        }
    }
}

fn text_from_blocks(data: &Value) -> Option<String> {
    let blocks = data.get("blocks")?.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|block| {
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
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn text_from_message_body(data: &Value) -> Option<String> {
    data.get("body")
        .and_then(|body| {
            body.get("Text")
                .and_then(|value| value.get("text"))
                .or_else(|| body.get("Brief").and_then(|value| value.get("text")))
                .or_else(|| body.get("text"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .filter(|text| !text.trim().is_empty())
}

fn message_body_text(body: &crate::types::MessageBody) -> Option<String> {
    match body {
        crate::types::MessageBody::Text { text }
        | crate::types::MessageBody::Brief { text, .. } => {
            (!text.trim().is_empty()).then(|| text.clone())
        }
        crate::types::MessageBody::Json { value } => serde_json::to_string(value).ok(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        storage::AppStorage,
        types::{
            AuthorityClass, BriefKind, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
            Priority, ToolExecutionStatus,
        },
    };

    fn storage() -> (TempDir, AppStorage) {
        let dir = TempDir::new().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        (dir, storage)
    }

    #[test]
    fn resolves_canonical_object_refs() {
        let (_dir, storage) = storage();
        let message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: "hi".into() },
        );
        storage.append_message(&message).unwrap();

        let turn = TurnRecord::new("agent-a", "turn-a", 1);
        storage.append_turn(&turn).unwrap();

        let tool = ToolExecutionRecord {
            id: "tool-a".into(),
            agent_id: "agent-a".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some("turn-a".into()),
            tool_name: "ExecCommand".into(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            duration_ms: 7,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "true"}),
            output: json!({"exit": 0}),
            summary: "ok".into(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool).unwrap();

        let resolver = RuntimeObjectResolver::new(&storage);
        assert!(matches!(
            resolver
                .resolve_ref(&format!("message:{}", message.id))
                .unwrap(),
            Some(ResolvedRuntimeObject::Message(_))
        ));
        assert!(matches!(
            resolver.resolve_ref("turn:turn-a").unwrap(),
            Some(ResolvedRuntimeObject::Turn(_))
        ));
        let Some(ResolvedRuntimeObject::ToolExecution(resolved)) = resolver
            .resolve_ref("tool_execution:tool-a:summary")
            .unwrap()
        else {
            panic!("expected tool execution");
        };
        assert_eq!(resolved.selected, Some(Value::String("ok".into())));
    }

    #[test]
    fn resolves_transcript_backed_brief_content() {
        let (_dir, storage) = storage();
        let entry = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            json!({
                "blocks": [
                    {"type": "text", "Text": {"text": "full"}},
                    {"type": "text", "text": "content"}
                ]
            }),
        );
        storage.append_transcript_entry(&entry).unwrap();

        let mut brief = BriefRecord::new("agent-a", BriefKind::Result, "preview", None, None);
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: entry.id.clone(),
            relation: crate::types::BriefContentSourceRelation::DerivedFrom,
        };
        storage.append_brief(&brief).unwrap();

        let resolved = RuntimeObjectResolver::new(&storage)
            .resolve_brief(&brief.id)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.content, "full content");
    }

    #[test]
    fn resolves_incoming_transcript_text_from_related_message() {
        let (_dir, storage) = storage();
        let message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "canonical input".into(),
            },
        );
        storage.append_message(&message).unwrap();
        let entry = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            json!({"delivery_surface": "api"}),
        );

        let resolved = RuntimeObjectResolver::new(&storage)
            .resolve_transcript_text(&entry)
            .unwrap();
        assert_eq!(resolved.as_deref(), Some("canonical input"));
    }

    #[test]
    fn transcript_backed_brief_falls_back_to_preview_for_legacy_missing_entry() {
        let (_dir, storage) = storage();
        let mut brief = BriefRecord::new("agent-a", BriefKind::Result, "preview", None, None);
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "missing".into(),
            relation: crate::types::BriefContentSourceRelation::DerivedFrom,
        };
        storage.append_brief(&brief).unwrap();

        let resolved = RuntimeObjectResolver::new(&storage)
            .resolve_brief(&brief.id)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.content, "preview");
    }
}
