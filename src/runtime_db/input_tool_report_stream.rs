//! Selected Tool fields from incremental original JSON; unrelated strings are skipped.
use super::*;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{cell::RefCell, fmt, io::BufReader};

#[derive(Clone, Copy)]
enum Shape {
    Root,
    PromptRoot,
    PromptName,
    PromptInput,
    PromptOutput,
    PromptEnvelope,
    PromptCommand,
    TaskHandle,
    SummaryText,
    CommandText,
    Presence,
    Boolean,
    Integer,
    SignedInteger,
    InputLength,
    ArtifactLength,
    BatchItems,
    Input,
    Output,
    Envelope,
    Result,
    Text,
    Paths,
    PathEntry,
    All,
}
#[derive(Clone, Copy)]
enum KeyName {
    Id,
    Agent,
    Session,
    Name,
    Input,
    Output,
    Patch,
    Envelope,
    Result,
    Paths,
    Reason,
    Summary,
    WorkId,
    TurnId,
    PromptSummary,
    TurnIndex,
    Authority,
    Trust,
    Status,
    Cmd,
    Items,
    Disposition,
    TaskHandle,
    TaskId,
    ExitStatus,
    ExitCode,
    StdoutPreview,
    StderrPreview,
    InitialPreview,
    StdoutArtifact,
    StderrArtifact,
    OutputArtifact,
    Artifact,
    Artifacts,
    Truncated,
    InitialTruncated,
    StdoutTruncated,
    StderrTruncated,
    OutputTruncated,
    CompletedCount,
    FailedCount,
    Other,
}
impl<'de> serde::Deserialize<'de> for KeyName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = KeyName;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Tool object key")
            }
            fn visit_str<E: serde::de::Error>(
                self,
                key: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(match key {
                    "id" => KeyName::Id,
                    "agent_id" => KeyName::Agent,
                    "session_id" => KeyName::Session,
                    "tool_name" => KeyName::Name,
                    "input" => KeyName::Input,
                    "output" => KeyName::Output,
                    "patch" => KeyName::Patch,
                    "envelope" => KeyName::Envelope,
                    "result" => KeyName::Result,
                    "changed_paths" => KeyName::Paths,
                    "reason" => KeyName::Reason,
                    "summary_text" => KeyName::Summary,
                    "work_item_id" => KeyName::WorkId,
                    "turn_id" => KeyName::TurnId,
                    "turn_index" => KeyName::TurnIndex,
                    "authority_class" => KeyName::Authority,
                    "trust" => KeyName::Trust,
                    "status" => KeyName::Status,
                    "summary" => KeyName::PromptSummary,
                    "cmd" => KeyName::Cmd,
                    "items" => KeyName::Items,
                    "disposition" => KeyName::Disposition,
                    "task_handle" => KeyName::TaskHandle,
                    "task_id" => KeyName::TaskId,
                    "exit_status" => KeyName::ExitStatus,
                    "exit_code" => KeyName::ExitCode,
                    "stdout_preview" => KeyName::StdoutPreview,
                    "stderr_preview" => KeyName::StderrPreview,
                    "initial_output_preview" => KeyName::InitialPreview,
                    "stdout_artifact" => KeyName::StdoutArtifact,
                    "stderr_artifact" => KeyName::StderrArtifact,
                    "output_artifact" => KeyName::OutputArtifact,
                    "artifact" => KeyName::Artifact,
                    "artifacts" => KeyName::Artifacts,
                    "truncated" => KeyName::Truncated,
                    "initial_output_truncated" => KeyName::InitialTruncated,
                    "stdout_truncated" => KeyName::StdoutTruncated,
                    "stderr_truncated" => KeyName::StderrTruncated,
                    "output_truncated" => KeyName::OutputTruncated,
                    "completed_count" => KeyName::CompletedCount,
                    "failed_count" => KeyName::FailedCount,
                    _ => KeyName::Other,
                })
            }
        }
        d.deserialize_str(KeyVisitor)
    }
}
#[derive(Default)]
struct Count {
    bytes: u64,
    nodes: usize,
    input_items: usize,
    batch_tool: bool,
}
impl Count {
    fn node<E: serde::de::Error>(&mut self) -> std::result::Result<(), E> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| E::custom("Tool report node overflow"))?;
        Ok(())
    }
    fn text<E: serde::de::Error>(&mut self, text: &str) -> std::result::Result<(), E> {
        self.node()?;
        self.bytes = self
            .bytes
            .checked_add(
                crate::runtime::input_evidence_budget::serialized_bytes(&text)
                    .map_err(E::custom)?,
            )
            .ok_or_else(|| E::custom("Tool report bytes overflow"))?;
        Ok(())
    }
}
struct Selected<'a> {
    shape: Shape,
    tool: &'a str,
    count: &'a RefCell<Count>,
    build: bool,
}
impl Selected<'_> {
    fn nested(&self, shape: Shape) -> Selected<'_> {
        Selected {
            shape,
            tool: self.tool,
            count: self.count,
            build: self.build,
        }
    }
    fn node<E: serde::de::Error>(&self) -> std::result::Result<(), E> {
        if !self.build {
            self.count.borrow_mut().node()?;
        }
        Ok(())
    }
    fn text<E: serde::de::Error>(&self, text: &str) -> std::result::Result<(), E> {
        if !self.build {
            self.count.borrow_mut().text(text)?;
        }
        Ok(())
    }
    fn member(&self, key: KeyName) -> Option<(&'static str, Shape)> {
        match (self.shape, key) {
            (Shape::PromptRoot, KeyName::Id) => Some(("id", Shape::Text)),
            (Shape::PromptRoot, KeyName::Agent) => Some(("agent_id", Shape::Text)),
            (Shape::PromptRoot, KeyName::Session) => Some(("session_id", Shape::Text)),
            (Shape::PromptRoot, KeyName::Name) => Some(("tool_name", Shape::PromptName)),
            (Shape::PromptRoot, KeyName::WorkId) => Some(("work_item_id", Shape::Text)),
            (Shape::PromptRoot, KeyName::TurnId) => Some(("turn_id", Shape::Text)),
            (Shape::PromptRoot, KeyName::TurnIndex) => Some(("turn_index", Shape::Integer)),
            (Shape::PromptRoot, KeyName::Authority) => Some(("authority_class", Shape::Text)),
            (Shape::PromptRoot, KeyName::Trust) => Some(("trust", Shape::Text)),
            (Shape::PromptRoot, KeyName::Status) => Some(("status", Shape::Text)),
            (Shape::PromptRoot, KeyName::PromptSummary) => Some(("summary", Shape::SummaryText)),
            (Shape::PromptRoot, KeyName::Input) => Some(("input", Shape::PromptInput)),
            (Shape::PromptRoot, KeyName::Output) => Some(("output", Shape::PromptOutput)),
            (Shape::PromptInput, KeyName::Cmd) => Some(("cmd", Shape::CommandText)),
            (Shape::PromptInput, KeyName::Items) => Some(("items", Shape::InputLength)),
            (Shape::PromptOutput, KeyName::Result) => Some(("result", Shape::PromptCommand)),
            (Shape::PromptOutput, KeyName::Envelope) => Some(("envelope", Shape::PromptEnvelope)),
            (Shape::PromptEnvelope, KeyName::Result) => Some(("result", Shape::PromptCommand)),
            (Shape::TaskHandle, KeyName::TaskId) => Some(("task_id", Shape::Text)),
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::Disposition) => {
                Some(("disposition", Shape::Text))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::TaskHandle) => {
                Some(("task_handle", Shape::TaskHandle))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::ExitStatus) => {
                Some(("exit_status", Shape::SignedInteger))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::ExitCode) => {
                Some(("exit_code", Shape::SignedInteger))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StdoutPreview) => {
                Some(("stdout_preview", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StderrPreview) => {
                Some(("stderr_preview", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::InitialPreview) => {
                Some(("initial_output_preview", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StdoutArtifact) => {
                Some(("stdout_artifact", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StderrArtifact) => {
                Some(("stderr_artifact", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::OutputArtifact) => {
                Some(("output_artifact", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::Artifact) => {
                Some(("artifact", Shape::Presence))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::Artifacts) => {
                Some(("artifacts", Shape::ArtifactLength))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::Truncated) => {
                Some(("truncated", Shape::Boolean))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::InitialTruncated) => {
                Some(("initial_output_truncated", Shape::Boolean))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StdoutTruncated) => {
                Some(("stdout_truncated", Shape::Boolean))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::StderrTruncated) => {
                Some(("stderr_truncated", Shape::Boolean))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::OutputTruncated) => {
                Some(("output_truncated", Shape::Boolean))
            }
            (Shape::PromptOutput | Shape::PromptCommand, KeyName::Items) => {
                Some(("items", Shape::BatchItems))
            }
            (Shape::Root, KeyName::Id) => Some(("id", Shape::Text)),
            (Shape::Root, KeyName::Agent) => Some(("agent_id", Shape::Text)),
            (Shape::Root, KeyName::Session) => Some(("session_id", Shape::Text)),
            (Shape::Root, KeyName::Name) => Some(("tool_name", Shape::Text)),
            (Shape::Root, KeyName::Input) if self.tool == "ApplyPatch" => {
                Some(("input", Shape::Input))
            }
            (Shape::Root, KeyName::Output)
                if matches!(self.tool, "ApplyPatch" | "Sleep" | "ExecCommandBatch") =>
            {
                Some(("output", Shape::Output))
            }
            (Shape::Input, KeyName::Patch) => Some(("patch", Shape::Text)),
            (Shape::Output, KeyName::Envelope) => Some(("envelope", Shape::Envelope)),
            (Shape::Envelope, KeyName::Result) => Some(("result", Shape::Result)),
            (Shape::Envelope, KeyName::Summary) if self.tool == "Sleep" => {
                Some(("summary_text", Shape::Text))
            }
            (Shape::Result, KeyName::Paths) if self.tool == "ApplyPatch" => {
                Some(("changed_paths", Shape::Paths))
            }
            (Shape::Result, KeyName::Reason) if self.tool == "Sleep" => {
                Some(("reason", Shape::Text))
            }
            (Shape::Result, KeyName::CompletedCount) if self.tool == "ExecCommandBatch" => {
                Some(("completed_count", Shape::Integer))
            }
            (Shape::Result, KeyName::FailedCount) if self.tool == "ExecCommandBatch" => {
                Some(("failed_count", Shape::Integer))
            }
            _ => None,
        }
    }
}
impl<'de> DeserializeSeed<'de> for Selected<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Selected<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an original selected Tool JSON value")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        self.node()?;
        Ok(if self.build && matches!(self.shape, Shape::Presence) {
            Value::Bool(true)
        } else {
            Value::Null
        })
    }
    fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        self.visit_unit()
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        self.node()?;
        Ok(if self.build {
            match self.shape {
                Shape::All | Shape::Boolean => Value::Bool(value),
                Shape::PromptOutput | Shape::PromptCommand | Shape::PromptEnvelope => {
                    Value::Bool(false)
                }
                Shape::Presence => Value::Bool(true),
                _ => Value::Null,
            }
        } else {
            Value::Null
        })
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        self.node()?;
        Ok(if self.build {
            match self.shape {
                Shape::All | Shape::Integer | Shape::SignedInteger => Value::Number(value.into()),
                Shape::PromptOutput | Shape::PromptCommand | Shape::PromptEnvelope => {
                    Value::Bool(false)
                }
                Shape::Boolean => Value::Bool(false),
                Shape::Presence => Value::Bool(true),
                _ => Value::Null,
            }
        } else {
            Value::Null
        })
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        self.node()?;
        Ok(if self.build {
            match self.shape {
                Shape::All | Shape::Integer | Shape::SignedInteger => Value::Number(value.into()),
                Shape::PromptOutput | Shape::PromptCommand | Shape::PromptEnvelope => {
                    Value::Bool(false)
                }
                Shape::Boolean => Value::Bool(false),
                Shape::Presence => Value::Bool(true),
                _ => Value::Null,
            }
        } else {
            Value::Null
        })
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        self.node()?;
        Ok(if self.build {
            match self.shape {
                Shape::All => {
                    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
                }
                Shape::PromptOutput | Shape::PromptCommand | Shape::PromptEnvelope => {
                    Value::Bool(false)
                }
                Shape::Boolean => Value::Bool(false),
                Shape::Presence => Value::Bool(true),
                _ => Value::Null,
            }
        } else {
            Value::Null
        })
    }
    fn visit_str<E: serde::de::Error>(self, text: &str) -> std::result::Result<Value, E> {
        if matches!(self.shape, Shape::PromptName) && !self.build {
            self.count.borrow_mut().batch_tool = text == "ExecCommandBatch";
        }
        if matches!(
            self.shape,
            Shape::PromptOutput | Shape::PromptCommand | Shape::PromptEnvelope
        ) {
            self.node()?;
            return Ok(if self.build {
                Value::Bool(false)
            } else {
                Value::Null
            });
        }
        if matches!(self.shape, Shape::Boolean) {
            self.node()?;
            return Ok(if self.build {
                Value::Bool(false)
            } else {
                Value::Null
            });
        }
        let text = if matches!(self.shape, Shape::SummaryText) {
            // Match the canonical writer's UTF-8 character cut, including NUL.
            let end = text
                .char_indices()
                .nth(TOOL_SUMMARY_CHARS)
                .map_or(text.len(), |(index, _)| index);
            &text[..end]
        } else if matches!(self.shape, Shape::CommandText)
            && text.len() > MAX_COMMAND_PREVIEW_INPUT_BYTES
        {
            OVERSIZED_COMMAND_PREVIEW
        } else {
            text
        };
        if matches!(
            self.shape,
            Shape::Text
                | Shape::Input
                | Shape::PathEntry
                | Shape::All
                | Shape::Integer
                | Shape::PromptName
                | Shape::SummaryText
                | Shape::CommandText
        ) {
            self.text(text)?;
            Ok(if self.build {
                Value::String(text.to_owned())
            } else {
                Value::Null
            })
        } else {
            self.visit_unit()
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        if matches!(self.shape, Shape::InputLength | Shape::ArtifactLength) {
            self.node()?;
            let mut len = 0usize;
            while seq.next_element::<IgnoredAny>()?.is_some() {
                len = len.checked_add(1).ok_or_else(|| {
                    <A::Error as serde::de::Error>::custom("Tool array length overflow")
                })?;
            }
            if !self.build && matches!(self.shape, Shape::InputLength) {
                self.count.borrow_mut().input_items = len;
            }
            return Ok(if self.build {
                Value::Number(
                    u64::try_from(len)
                        .map_err(<A::Error as serde::de::Error>::custom)?
                        .into(),
                )
            } else {
                Value::Null
            });
        }
        if matches!(self.shape, Shape::BatchItems) {
            self.node()?;
            let mut stats = [0u64; 5];
            let limit = self.count.borrow().input_items;
            if !self.build || !self.count.borrow().batch_tool {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
            } else {
                for _ in 0..limit {
                    let Some(item) = seq.next_element_seed(BatchItemSeed)? else {
                        break;
                    };
                    stats[0] += 1;
                    let item = item.result.unwrap_or(item.direct);
                    stats[if item.promoted {
                        3
                    } else {
                        match item.exit {
                            Some(0) => 1,
                            Some(_) => 2,
                            None => 4,
                        }
                    }] += 1;
                }
                while seq.next_element::<IgnoredAny>()?.is_some() {}
            } // end build-only scalar batch iterator
              // Only scalar counters survive, never one Value per command item.
              // Account their fixed container/keys before the build pass.
            if !self.build {
                for key in ["seen", "succeeded", "failed", "promoted", "unknown"] {
                    self.text(key)?;
                    self.node()?;
                }
            }
            return Ok(if self.build {
                serde_json::json!({"seen":stats[0],"succeeded":stats[1],"failed":stats[2],"promoted":stats[3],"unknown":stats[4]})
            } else {
                Value::Null
            });
        }
        if !matches!(self.shape, Shape::Paths | Shape::All) {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            return self.visit_unit();
        }
        self.node()?;
        let mut values = Vec::new();
        let child = if self.build && matches!(self.shape, Shape::Paths) {
            Shape::PathEntry
        } else {
            Shape::All
        };
        while let Some(value) = seq.next_element_seed(self.nested(child))? {
            if self.build {
                values.push(value);
            }
        }
        Ok(if self.build {
            Value::Array(values)
        } else {
            Value::Null
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        if matches!(
            self.shape,
            Shape::Text
                | Shape::Paths
                | Shape::PathEntry
                | Shape::Presence
                | Shape::Boolean
                | Shape::Integer
                | Shape::SignedInteger
                | Shape::PromptName
                | Shape::SummaryText
                | Shape::CommandText
                | Shape::InputLength
                | Shape::ArtifactLength
                | Shape::BatchItems
        ) {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return self.visit_unit();
        }
        self.node()?;
        let mut values = Map::new();
        if matches!(self.shape, Shape::All) {
            // Only this finite selected subtree owns keys. Count pass borrows
            // the decoder scratch, including every key/container before build.
            struct FullKey<'a> {
                count: &'a RefCell<Count>,
                build: bool,
            }
            impl<'de> DeserializeSeed<'de> for FullKey<'_> {
                type Value = Option<String>;
                fn deserialize<D: serde::Deserializer<'de>>(
                    self,
                    d: D,
                ) -> std::result::Result<Self::Value, D::Error> {
                    struct FullKeyVisitor<'a>(FullKey<'a>);
                    impl Visitor<'_> for FullKeyVisitor<'_> {
                        type Value = Option<String>;
                        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                            f.write_str("a selected object key")
                        }
                        fn visit_str<E: serde::de::Error>(
                            self,
                            key: &str,
                        ) -> std::result::Result<Self::Value, E> {
                            if !self.0.build {
                                self.0.count.borrow_mut().text(key)?;
                            }
                            Ok(self.0.build.then(|| key.to_owned()))
                        }
                    }
                    d.deserialize_str(FullKeyVisitor(self))
                }
            }
            while let Some(key) = map.next_key_seed(FullKey {
                count: self.count,
                build: self.build,
            })? {
                let value = map.next_value_seed(self.nested(Shape::All))?;
                if let Some(key) = key {
                    values.insert(key, value);
                }
            }
        } else {
            // No String is allocated for an unknown key, even when the source
            // contains a huge key or an unrelated deeply nested stdout object.
            let mut seen = 0u64;
            while let Some(key) = map.next_key::<KeyName>()? {
                let bit = 1u64 << key as u32;
                if seen & bit != 0 {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
                seen |= bit;
                if let Some((key, shape)) = self.member(key) {
                    self.text(key)?;
                    let value = map.next_value_seed(self.nested(shape))?;
                    if self.build && !values.contains_key(key) {
                        values.insert(key.into(), value);
                    }
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(if self.build {
            Value::Object(values)
        } else {
            Value::Null
        })
    }
}

pub(super) fn selected_report_on(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
    tool: &str,
    admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<Value> {
    selected_on(tx, agent, id, tool, Shape::Root, admit)
}
pub(super) fn selected_prompt_on(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
    admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<Value> {
    selected_on(tx, agent, id, "", Shape::PromptRoot, admit)
}
fn selected_on(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
    tool: &str,
    shape: Shape,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<Value> {
    // Incremental IO owns one 64 KiB reader buffer. SQLite sees only scalar
    // locator columns, never json_extract/json_tree of the complete Tool body.
    admit(u64::try_from((64 * 1024 + 4096) * 8)?, 32)?;
    let (rowid, text): (i64, bool) = tx.query_row(
        "SELECT rowid,typeof(payload_json)='text' FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
        params![agent,id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?.context(InputEvidenceSnapshotError::NeedsPreparation(id.into()))?;
    ensure!(
        text,
        InputEvidenceSnapshotError::ProjectionChanged(id.into())
    );
    let mut blob = tx.blob_open(
        rusqlite::DatabaseName::Main,
        "tool_executions",
        "payload_json",
        rowid,
        true,
    )?;
    // serde_json 1.0.149 IoRead has one scratch Vec. Decoded UTF-8 strings and
    // its IgnoredAny nesting stack never exceed original bytes; Vec growth is
    // below twice that length (plus its minimum capacity). This guard precedes
    // Deserializer construction, including giant unknown keys. IgnoredAny's
    // ignore_str never appends stdout bytes to that Vec.
    let scratch = blob
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(4096))
        .context(InputEvidenceSnapshotError::ResourceExhausted(
            "Tool report raw",
        ))?;
    ensure!(
        scratch <= MAX_METADATA_BYTES,
        InputEvidenceSnapshotError::ResourceExhausted("Tool report raw")
    );
    admit(u64::try_from(scratch)?, 1)?;
    let count = RefCell::new(Count::default());
    {
        let reader = BufReader::with_capacity(64 * 1024, &mut blob);
        let mut decoder = serde_json::Deserializer::from_reader(reader);
        Selected {
            shape,
            tool,
            count: &count,
            build: false,
        }
        .deserialize(&mut decoder)?;
        decoder.end()?;
    }
    use std::io::{Seek, SeekFrom};
    blob.seek(SeekFrom::Start(0))?;
    let counted = count.into_inner();
    let bytes = counted.bytes;
    let nodes = counted.nodes;
    let raw = bytes
        .checked_add(4096)
        .and_then(|n| n.checked_mul(8))
        .context(InputEvidenceSnapshotError::ResourceExhausted(
            "Tool report raw",
        ))?;
    ensure!(
        raw <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("Tool report raw")
    );
    admit(raw, 8)?;
    let nodes = nodes
        .checked_mul(8)
        .context(InputEvidenceSnapshotError::ResourceExhausted(
            "Tool report native",
        ))?;
    let native = u64::try_from(nodes)?.checked_mul(256).context(
        InputEvidenceSnapshotError::ResourceExhausted("Tool report native"),
    )?;
    ensure!(
        nodes <= MAX_RECORDS
            && raw
                .checked_add(native)
                .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
        InputEvidenceSnapshotError::ResourceExhausted("Tool report native")
    );
    admit(native, nodes)?;
    let count = RefCell::new(Count {
        input_items: counted.input_items,
        batch_tool: counted.batch_tool,
        ..Default::default()
    });
    let source = {
        let reader = BufReader::with_capacity(64 * 1024, &mut blob);
        let mut decoder = serde_json::Deserializer::from_reader(reader);
        let source = Selected {
            shape,
            tool,
            count: &count,
            build: true,
        }
        .deserialize(&mut decoder)?;
        decoder.end()?;
        source
    };
    blob.close()?;
    ensure!(
        source.get("id").and_then(Value::as_str) == Some(id)
            && source
                .get("agent_id")
                .and_then(Value::as_str)
                .or_else(|| source.get("session_id").and_then(Value::as_str))
                == Some(agent)
            && (matches!(shape, Shape::PromptRoot)
                || source.get("tool_name").and_then(Value::as_str) == Some(tool)),
        InputEvidenceSnapshotError::ProjectionChanged(id.into())
    );
    Ok(source)
}

#[derive(Default)]
struct CommandStatus {
    promoted: bool,
    exit: Option<i64>,
}
#[derive(Default)]
struct BatchItem {
    direct: CommandStatus,
    result: Option<CommandStatus>,
}
struct BatchItemSeed;
impl<'de> DeserializeSeed<'de> for BatchItemSeed {
    type Value = BatchItem;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<BatchItem, D::Error> {
        d.deserialize_any(BatchItemVisitor)
    }
}
struct BatchItemVisitor;
impl<'de> Visitor<'de> for BatchItemVisitor {
    type Value = BatchItem;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a batch command result")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(BatchItem::default())
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        self.visit_unit()
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut item = BatchItem::default();
        let mut code = None;
        let mut seen = 0u64;
        while let Some(key) = map.next_key::<KeyName>()? {
            let bit = 1u64 << key as u32;
            if seen & bit != 0 {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            seen |= bit;
            match key {
                KeyName::Disposition => item.direct.promoted = map.next_value_seed(PromotedSeed)?,
                KeyName::ExitStatus => item.direct.exit = map.next_value_seed(SignedNumberSeed)?,
                KeyName::ExitCode => code = map.next_value_seed(SignedNumberSeed)?,
                KeyName::Result => item.result = map.next_value_seed(CommandStatusSeed)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if seen & (1u64 << KeyName::ExitStatus as u32) == 0 {
            item.direct.exit = code;
        }
        Ok(item)
    }
}
struct CommandStatusSeed;
impl<'de> DeserializeSeed<'de> for CommandStatusSeed {
    type Value = Option<CommandStatus>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_any(CommandStatusVisitor)
    }
}
struct CommandStatusVisitor;
impl<'de> Visitor<'de> for CommandStatusVisitor {
    type Value = Option<CommandStatus>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a command envelope result")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
        Ok(Some(CommandStatus::default()))
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Some(CommandStatus::default()))
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut status = CommandStatus::default();
        let mut code = None;
        let mut seen = 0u64;
        while let Some(key) = map.next_key::<KeyName>()? {
            let bit = 1u64 << key as u32;
            if seen & bit != 0 {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            seen |= bit;
            match key {
                KeyName::Disposition => status.promoted = map.next_value_seed(PromotedSeed)?,
                KeyName::ExitStatus => status.exit = map.next_value_seed(SignedNumberSeed)?,
                KeyName::ExitCode => code = map.next_value_seed(SignedNumberSeed)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if seen & (1u64 << KeyName::ExitStatus as u32) == 0 {
            status.exit = code;
        }
        Ok(Some(status))
    }
}
struct PromotedSeed;
impl<'de> DeserializeSeed<'de> for PromotedSeed {
    type Value = bool;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<bool, D::Error> {
        d.deserialize_any(PromotedVisitor)
    }
}
struct PromotedVisitor;
impl<'de> Visitor<'de> for PromotedVisitor {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a command disposition")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<bool, E> {
        Ok(false)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<bool, E> {
        Ok(false)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<bool, E> {
        Ok(false)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<bool, E> {
        Ok(false)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<bool, E> {
        Ok(false)
    }
    fn visit_str<E: serde::de::Error>(self, text: &str) -> std::result::Result<bool, E> {
        Ok(matches!(text, "promoted_to_task" | "already_running"))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<bool, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(false)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<bool, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(false)
    }
}

struct SignedNumberSeed;
impl<'de> DeserializeSeed<'de> for SignedNumberSeed {
    type Value = Option<i64>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_any(SignedNumberVisitor)
    }
}
struct SignedNumberVisitor;
impl<'de> Visitor<'de> for SignedNumberVisitor {
    type Value = Option<i64>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a command numeric exit status")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(Some(value))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(i64::try_from(value).ok())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

/// Count the whole old DTO before the original Focus comparer decodes it.
/// This pass owns decoder scratch only, never a body String/Value tree.
pub(super) fn original_body_nodes_on(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<usize> {
    admit(u64::try_from((64 * 1024 + 4096) * 8)?, 32)?;
    let (rowid, text): (i64, bool) = tx.query_row(
        "SELECT rowid,typeof(payload_json)='text' FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
        params![agent,id], |r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?.context(InputEvidenceSnapshotError::NeedsPreparation(id.into()))?;
    ensure!(
        text,
        InputEvidenceSnapshotError::ProjectionChanged(id.into())
    );
    let mut blob = tx.blob_open(
        rusqlite::DatabaseName::Main,
        "tool_executions",
        "payload_json",
        rowid,
        true,
    )?;
    // Same IoRead capacity bound as selected_on, including every old key/text.
    let scratch = blob
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(4096))
        .context(InputEvidenceSnapshotError::ResourceExhausted(
            "original Tool decoder scratch",
        ))?;
    ensure!(
        scratch <= MAX_METADATA_BYTES,
        InputEvidenceSnapshotError::ResourceExhausted("original Tool decoder scratch")
    );
    admit(u64::try_from(scratch)?, 1)?;
    let count = RefCell::new(Count::default());
    {
        let reader = BufReader::with_capacity(64 * 1024, &mut blob);
        let mut decoder = serde_json::Deserializer::from_reader(reader);
        Selected {
            shape: Shape::All,
            tool: "",
            count: &count,
            build: false,
        }
        .deserialize(&mut decoder)?;
        decoder.end()?;
    }
    blob.close()?;
    Ok(count.into_inner().nodes)
}
