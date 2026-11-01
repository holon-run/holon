//! Select a recoverable representation before admitting a new source.

use anyhow::Result;
use serde::ser::SerializeMap;
use sha2::{Digest, Sha256};
use std::{borrow::Cow, fmt, mem::size_of};

use crate::{
    storage::AppStorage,
    types::{
        AuthorityClass, IdleInputBatchItem, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin,
    },
};

use super::{BATCH_HEADER, INPUT_BATCH_BYTES};
#[cfg(test)]
use crate::context::estimate_text_tokens;
use crate::diagnostics::resource_io;
use crate::runtime::RuntimeHandle;

/// The source body is qualified by the current finite IdleRead cut.
pub(in crate::runtime) fn qualified_item_after(
    read: &super::read::IdleRead<'_, '_>,
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
) -> Result<Option<IdleInputBatchItem>> {
    read.with_message(&message.id, |original| {
        if !same_source(original, message) {
            return Ok(None);
        }
        project_item_after(message, token_limit, byte_limit, preceding)
    })
    .map(Option::flatten)
}

fn same_source(original: &MessageEnvelope, message: &MessageEnvelope) -> bool {
    original.agent_id == message.agent_id
        && original.body == message.body
        && original.origin == message.origin
        && original.authority_class == message.authority_class
        && original.routing_context == message.routing_context
        && original.source_refs == message.source_refs
        && original.work_item_id == message.work_item_id
        && original.task_id == message.task_id
}

const FULL_BODY: &str = "  representation: full\n  body:\n";
const PREVIEW_BODY: &str = "  representation: bounded_preview\n  body preview:\n";

/// The same fragments feed capacity measurement and final materialization.
#[derive(Clone, Copy)]
pub(crate) struct ProjectionText<'a>([&'a str; 4]);

impl fmt::Display for ProjectionText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for part in self.0 {
            formatter.write_str(part)?;
        }
        Ok(())
    }
}

impl ProjectionText<'_> {
    pub(crate) fn measure(self) -> Result<(usize, usize)> {
        let mut bytes = 0usize;
        let mut chars = 0usize;
        for part in self.0 {
            bytes = bytes
                .checked_add(part.len())
                .ok_or_else(material_overflow)?;
            chars = chars
                .checked_add(part.chars().count())
                .ok_or_else(material_overflow)?;
        }
        Ok((bytes, chars))
    }

    fn matches(self, mut text: &str) -> bool {
        for part in self.0 {
            let Some(rest) = text.strip_prefix(part) else {
                return false;
            };
            text = rest;
        }
        text.is_empty()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ProjectionView<'a> {
    pub message_id: &'a str,
    pub source_ref: &'a str,
    pub rendered: ProjectionText<'a>,
}

impl<'a> From<&'a IdleInputBatchItem> for ProjectionView<'a> {
    fn from(item: &'a IdleInputBatchItem) -> Self {
        Self {
            message_id: &item.message_id,
            source_ref: &item.source_ref,
            rendered: ProjectionText([&item.rendered, "", "", ""]),
        }
    }
}

fn material_overflow() -> anyhow::Error {
    anyhow::anyhow!("ResourceExhausted: input projection material overflow")
}

fn owned_text(parts: &[&str]) -> Result<String> {
    let bytes = parts.iter().try_fold(0usize, |bytes, part| {
        bytes.checked_add(part.len()).ok_or_else(material_overflow)
    })?;
    resource_io::charge_owned_buffer(u64::try_from(bytes)?)?;
    resource_io::charge_owned_metadata(size_of::<String>() as u64, 1)?;
    let mut text = String::with_capacity(bytes);
    for part in parts {
        text.push_str(part);
    }
    Ok(text)
}

fn serialized_text(value: &impl serde::Serialize) -> Result<String> {
    let bytes = crate::runtime::input_evidence_budget::serialized_bytes(value)?;
    resource_io::charge_owned_buffer(bytes)?;
    resource_io::charge_owned_metadata(size_of::<String>() as u64, 1)?;
    let mut text = Vec::with_capacity(usize::try_from(bytes)?);
    serde_json::to_writer(&mut text, value)?;
    Ok(String::from_utf8(text)?)
}

fn admit_slots<T>(count: usize) -> Result<()> {
    let bytes = count
        .checked_mul(size_of::<T>())
        .and_then(|bytes| bytes.checked_add(size_of::<Vec<T>>()))
        .ok_or_else(material_overflow)?;
    resource_io::charge_owned_metadata(
        u64::try_from(bytes)?,
        count.checked_add(1).ok_or_else(material_overflow)?,
    )?;
    Ok(())
}

struct DigestWriter(Sha256);

impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Preserve the original Value object's sorted JSON keys without cloning its
// string/map fields. Only nested typed objects need canonicalization scratch.
struct ProjectionProvenance<'a> {
    message: &'a MessageEnvelope,
    content_version: &'a str,
    source_ref: &'a str,
    origin: &'a serde_json::Value,
    routing: &'a serde_json::Value,
}

impl serde::Serialize for ProjectionProvenance<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let message = self.message;
        let mut map = serializer.serialize_map(Some(18))?;
        map.serialize_entry("admission_context", &message.admission_context)?;
        map.serialize_entry("agent_id", &message.agent_id)?;
        map.serialize_entry("authority_class", &message.authority_class)?;
        map.serialize_entry("causation_id", &message.causation_id)?;
        map.serialize_entry("content_version", self.content_version)?;
        map.serialize_entry("correlation_id", &message.correlation_id)?;
        map.serialize_entry("created_at", &message.created_at)?;
        map.serialize_entry("delivery_surface", &message.delivery_surface)?;
        map.serialize_entry("full_metadata_source_ref", self.source_ref)?;
        map.serialize_entry("id", &message.id)?;
        map.serialize_entry("kind", &message.kind)?;
        map.serialize_entry("origin", self.origin)?;
        map.serialize_entry("priority", &message.priority)?;
        map.serialize_entry("projection_version", &1u8)?;
        map.serialize_entry("routing_context", self.routing)?;
        map.serialize_entry("source_refs", &message.source_refs)?;
        map.serialize_entry("task_id", &message.task_id)?;
        map.serialize_entry("work_item_id", &message.work_item_id)?;
        map.end()
    }
}

struct PreparedProjection<'a> {
    message_id: &'a str,
    source_ref: String,
    prefix: String,
    suffix: String,
    body: Cow<'a, str>,
    body_chars: usize,
    may_preview: bool,
}

impl<'a> PreparedProjection<'a> {
    fn new(message: &'a MessageEnvelope) -> Result<Self> {
        resource_io::check_active()?;
        let source_ref = owned_text(&["message:", &message.id])?;
        let mut digest = DigestWriter(Sha256::new());
        serde_json::to_writer(&mut digest, &message.body)?;
        let hash = digest.0.finalize();
        let mut content_version = [0u8; 71];
        content_version[..7].copy_from_slice(b"sha256:");
        for (index, byte) in hash.iter().enumerate() {
            content_version[7 + index * 2] = b"0123456789abcdef"[usize::from(*byte >> 4)];
            content_version[8 + index * 2] = b"0123456789abcdef"[usize::from(*byte & 15)];
        }
        resource_io::admit_cloned_value(&message.origin)?;
        let origin = serde_json::to_value(&message.origin)?;
        if message.routing_context.is_some() {
            resource_io::admit_cloned_value(&message.routing_context)?;
        }
        let routing = serde_json::to_value(&message.routing_context)?;
        let provenance = serialized_text(&ProjectionProvenance {
            message,
            content_version: std::str::from_utf8(&content_version)?,
            source_ref: &source_ref,
            origin: &origin,
            routing: &routing,
        })?;
        // The existing reply formatter owns three sanitized labels and its
        // final String. Bound those fields only, never the source body.
        if let Some(expectation) = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("agent_message_reply_expectation"))
        {
            let labels = ["sender_agent_id", "waiting_task_id", "request_delivery_id"]
                .iter()
                .filter_map(|key| expectation.get(*key).and_then(serde_json::Value::as_str))
                .try_fold(0usize, |bytes, label| {
                    bytes.checked_add(label.len()).ok_or_else(material_overflow)
                })?;
            let bytes = labels
                .checked_mul(2)
                .and_then(|bytes| bytes.checked_add(1024 + 4 * size_of::<String>()))
                .ok_or_else(material_overflow)?;
            resource_io::charge_owned_metadata(u64::try_from(bytes)?, 4)?;
        }
        let reply = crate::context::render_message_reply_expectation_context(message);
        let prefix = owned_text(&[
            "- reply_context_id: ",
            &message.id,
            "\n  source_ref: ",
            &source_ref,
            "\n  provenance: ",
            &provenance,
            "\n",
            reply.as_deref().unwrap_or(""),
            if reply.is_some() { "\n" } else { "" },
        ])?;
        let suffix = owned_text(&[
            "\n[bounded preview; original not fully read; retrieve with MemoryGet(",
            &source_ref,
            ")]\n",
        ])?;
        let body = match &message.body {
            MessageBody::Text { text } | MessageBody::Brief { text, .. } => {
                Cow::Borrowed(text.as_str())
            }
            MessageBody::Json { value } => Cow::Owned(serialized_text(value)?),
        };
        let body_chars = body.chars().count();
        let may_preview = !matches!(message.origin, MessageOrigin::Operator { .. })
            && message.kind != MessageKind::OperatorPrompt
            && (matches!(
                message.authority_class,
                AuthorityClass::ExternalEvidence | AuthorityClass::IntegrationSignal
            ) || message.kind == MessageKind::TaskResult);
        Ok(Self {
            message_id: &message.id,
            source_ref,
            prefix,
            suffix,
            body,
            body_chars,
            may_preview,
        })
    }

    fn full(&self) -> ProjectionView<'_> {
        self.view(FULL_BODY, &self.body, "\n")
    }

    fn preview(&self, chars: usize) -> ProjectionView<'_> {
        let boundary = self
            .body
            .char_indices()
            .nth(chars)
            .map_or(self.body.len(), |(offset, _)| offset);
        self.view(PREVIEW_BODY, &self.body[..boundary], &self.suffix)
    }

    fn limited(&self, chars: usize) -> ProjectionView<'_> {
        if !self.may_preview || chars >= self.body_chars {
            self.full()
        } else {
            self.preview(chars)
        }
    }

    fn view<'b>(&'b self, header: &'b str, body: &'b str, suffix: &'b str) -> ProjectionView<'b> {
        ProjectionView {
            message_id: self.message_id,
            source_ref: &self.source_ref,
            rendered: ProjectionText([&self.prefix, header, body, suffix]),
        }
    }
}

fn materialize(view: ProjectionView<'_>) -> Result<IdleInputBatchItem> {
    resource_io::check_active()?;
    Ok(IdleInputBatchItem {
        message_id: owned_text(&[view.message_id])?,
        source_ref: owned_text(&[view.source_ref])?,
        rendered: owned_text(&view.rendered.0)?,
    })
}

fn text_tokens(parts: &[&str]) -> Option<usize> {
    parts
        .iter()
        .try_fold(0usize, |chars, part| {
            chars.checked_add(part.chars().count())
        })
        .map(|chars| chars.div_ceil(4).max(1))
}

#[derive(Clone, Copy)]
pub(crate) struct InputBudget {
    pub tokens: usize,
    pub bytes: usize,
}

#[cfg(test)]
impl From<usize> for InputBudget {
    fn from(bytes: usize) -> Self {
        Self {
            tokens: usize::MAX,
            bytes,
        }
    }
}

impl InputBudget {
    pub(crate) fn fits(&self, section: &str, content: &str) -> bool {
        text_tokens(&["[", section, "]\n", content]).is_some_and(|tokens| tokens <= self.tokens)
    }

    pub(crate) fn fits_idle(&self, current: &str, pending: Option<&str>) -> bool {
        let pending_tokens = pending.map_or(Some(0), |text| {
            text_tokens(&["[pending_task_results]\n", text])
        });
        let Some(pending_tokens) = pending_tokens else {
            return false;
        };
        self.fits_idle_tokens(current, pending_tokens)
    }

    pub(crate) fn fits_idle_tokens(&self, current: &str, pending_tokens: usize) -> bool {
        text_tokens(&["[current_input]\n", current])
            .and_then(|tokens| tokens.checked_add(pending_tokens))
            .is_some_and(|tokens| tokens <= self.tokens)
    }
}

#[cfg(test)]
pub(crate) fn admitted_item(
    storage: &AppStorage,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<IdleInputBatchItem>> {
    let Some(db) = storage.runtime_db()? else {
        return Ok(None);
    };
    Ok(db
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)?
        .and_then(|state| {
            state
                .open_attempt()
                .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
                .and_then(|attempt| attempt.input_projection.clone())
        }))
}

impl RuntimeHandle {
    pub(in crate::runtime) async fn prepare_input_projection(
        &self,
        message: &MessageEnvelope,
        state: &crate::types::AgentState,
        continuation: Option<&crate::types::ContinuationResolution>,
    ) -> Result<Option<IdleInputBatchItem>> {
        let mut config = self.current_context_config().await;
        config.prompt_budget_estimated_tokens = config
            .prompt_budget_estimated_tokens
            .checked_sub(crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS)
            .ok_or_else(|| anyhow::anyhow!("context budget cannot reserve runtime time"))?;
        // The scheduler may supply a pre-Claim binding. It selects the budget
        // owner only; loaded acquisition always captures the real publication.
        let work_item_id = state
            .current_execution_binding
            .as_ref()
            .map_or(state.current_work_item_id.as_deref(), |binding| {
                binding.work_item_id.as_deref()
            });
        let tokens = self
            .prepare_input_admission_tokens(
                message,
                continuation,
                crate::context::ContextAdmissionPlan {
                    work_item_id,
                    token_budget: config.prompt_budget_estimated_tokens,
                },
            )
            .await?;
        persisted_item_after(
            &self.inner.storage,
            message,
            tokens,
            INPUT_BATCH_BYTES.saturating_sub(BATCH_HEADER.len()),
            &format!("[current_input]\n{BATCH_HEADER}"),
        )
    }
}

#[cfg(test)]
pub(in crate::runtime) fn persisted_item(
    storage: &AppStorage,
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    persisted_item_after(storage, message, token_limit, byte_limit, "")
}

pub(in crate::runtime) fn persisted_item_after(
    storage: &AppStorage,
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
) -> Result<Option<IdleInputBatchItem>> {
    let Some(original) = storage.read_message_by_id(&message.id)? else {
        return Ok(None);
    };
    if original.agent_id != message.agent_id
        || original.body != message.body
        || original.origin != message.origin
        || original.authority_class != message.authority_class
        || original.routing_context != message.routing_context
        || original.source_refs != message.source_refs
        || original.work_item_id != message.work_item_id
        || original.task_id != message.task_id
    {
        return Ok(None);
    }
    project_item_after(message, token_limit, byte_limit, preceding)
}

#[cfg(test)]
pub(crate) fn project_item(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    project_item_after(message, token_limit, byte_limit, "")
}

fn project_item_after(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
) -> Result<Option<IdleInputBatchItem>> {
    project_item_limited(message, token_limit, byte_limit, preceding, None)
}

fn project_item_limited(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
    preview_chars: Option<usize>,
) -> Result<Option<IdleInputBatchItem>> {
    let source = PreparedProjection::new(message)?;
    let fits = |view: ProjectionView<'_>| -> Result<bool> {
        let (bytes, chars) = view.rendered.measure()?;
        let chars = preceding
            .chars()
            .count()
            .checked_add(chars)
            .ok_or_else(material_overflow)?;
        Ok(bytes <= byte_limit && chars.div_ceil(4).max(1) <= token_limit)
    };
    let selected = if fits(source.full())?
        && (preview_chars.is_none_or(|limit| limit >= source.body_chars) || !source.may_preview)
    {
        source.full()
    } else {
        // Instructions must not become partial executable requests.
        if !source.may_preview {
            return Ok(None);
        }
        if !fits(source.preview(0))? {
            return Ok(None);
        }
        let (mut low, mut high) = (
            0,
            preview_chars.unwrap_or(usize::MAX).min(source.body_chars),
        );
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if fits(source.preview(mid))? {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        source.preview(low)
    };
    Ok(Some(materialize(selected)?))
}

/// Validate the selected content and provenance against the immutable original.
pub(crate) fn validate_projection(
    message: &MessageEnvelope,
    item: &IdleInputBatchItem,
) -> Result<()> {
    let source = PreparedProjection::new(message)?;
    if item.message_id == message.id
        && item.source_ref == source.source_ref
        && source.full().rendered.matches(&item.rendered)
    {
        return Ok(());
    }
    let preview = item
        .rendered
        .strip_prefix(&source.prefix)
        .and_then(|text| text.strip_prefix(PREVIEW_BODY))
        .and_then(|text| text.strip_suffix(&source.suffix));
    if item.message_id != message.id
        || item.source_ref != source.source_ref
        || !source.may_preview
        || preview.is_none_or(|text| !source.body.starts_with(text))
    {
        anyhow::bail!(
            "source projection does not match original message {}",
            message.id
        );
    }
    Ok(())
}

/// Fit linked sources together without losing instruction bodies or metadata.
#[cfg(test)]
pub(in crate::runtime) fn persisted_group(
    storage: &AppStorage,
    messages: &[MessageEnvelope],
    fits: impl FnMut(&[ProjectionView<'_>]) -> Result<bool>,
) -> Result<Option<Vec<IdleInputBatchItem>>> {
    admit_slots::<PreparedProjection<'_>>(messages.len())?;
    let mut sources = Vec::with_capacity(messages.len());
    for message in messages {
        let Some(original) = storage.read_message_by_id(&message.id)? else {
            return Ok(None);
        };
        if !same_source(&original, message) {
            return Ok(None);
        }
        sources.push(PreparedProjection::new(message)?);
    }
    fit_group(&sources, fits)
}

pub(in crate::runtime) fn qualified_group(
    read: &super::read::IdleRead<'_, '_>,
    messages: &[super::read::QualifiedMessageRead],
    fits: impl FnMut(&[ProjectionView<'_>]) -> Result<bool>,
) -> Result<Option<Vec<IdleInputBatchItem>>> {
    admit_slots::<PreparedProjection<'_>>(messages.len())?;
    let mut sources = Vec::with_capacity(messages.len());
    for message in messages {
        if read.with_message(&message.id, |original| Ok(same_source(original, message)))?
            != Some(true)
        {
            return Ok(None);
        }
        sources.push(PreparedProjection::new(message)?);
    }
    fit_group(&sources, fits)
}

fn fit_group(
    sources: &[PreparedProjection<'_>],
    mut fits: impl FnMut(&[ProjectionView<'_>]) -> Result<bool>,
) -> Result<Option<Vec<IdleInputBatchItem>>> {
    resource_io::check_active()?;
    let mut checked_fits = |items: &[ProjectionView<'_>]| -> Result<bool> {
        let accepted = fits(items)?;
        resource_io::check_active()?;
        Ok(accepted)
    };
    admit_slots::<ProjectionView<'_>>(sources.len())?;
    let mut items = Vec::with_capacity(sources.len());
    items.extend(sources.iter().map(PreparedProjection::full));
    if !checked_fits(&items)? {
        for (item, source) in items.iter_mut().zip(sources) {
            *item = source.limited(0);
        }
        if !checked_fits(&items)? {
            return Ok(None);
        }
        for (index, source) in sources.iter().enumerate() {
            items[index] = source.full();
            if checked_fits(&items)? {
                continue;
            }
            let (mut low, mut high) = (0, source.body_chars);
            while low < high {
                let mid = low + (high - low).div_ceil(2);
                items[index] = source.limited(mid);
                if checked_fits(&items)? {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            items[index] = source.limited(low);
        }
    }
    // A failed callback must never be converted into a partial successful
    // selection, even if its caller catches the material error.
    resource_io::check_active()?;
    admit_slots::<IdleInputBatchItem>(items.len())?;
    let mut selected = Vec::with_capacity(items.len());
    for item in items {
        selected.push(materialize(item)?);
    }
    Ok(Some(selected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
        types::Priority,
    };

    fn assert_typed_resource_exhausted(error: &anyhow::Error) {
        assert!(
            error.chain().any(|source| source
                .downcast_ref::<crate::runtime_error::RuntimeError>()
                .is_some()),
            "material rejection must keep its typed RuntimeError source"
        );
        assert_eq!(
            crate::runtime_error::describe_runtime_error(error).code,
            "context_resource_exhausted"
        );
    }

    fn evidence(body: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "fixture".into(),
                event_type: None,
            },
            AuthorityClass::ExternalEvidence,
            Priority::Normal,
            MessageBody::Text { text: body.into() },
        )
    }

    fn required_reply(body: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: "waiting-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text { text: body.into() },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.correlation_id = Some("waiting-task".into());
        message.metadata = Some(serde_json::json!({
            "agent_message_delivery": {
                "delivery_id": "request-delivery",
                "caller_agent_id": "waiting-sender",
            },
            "agent_message_reply_expectation": {
                "mode": "required",
                "sender_agent_id": "waiting-sender",
                "waiting_task_id": "waiting-task",
                "request_message_id": message.id,
                "request_delivery_id": "request-delivery",
            },
        }));
        message
    }

    // Independent byte-shape oracle from the original owning formatter.
    fn original_item(
        message: &MessageEnvelope,
        preview: Option<usize>,
    ) -> Result<IdleInputBatchItem> {
        let source_ref = format!("message:{}", message.id);
        let body = super::super::message_text(&message.body);
        let content_version = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&message.body)?)
        );
        let provenance = serde_json::to_string(&serde_json::json!({
            "projection_version": 1,
            "content_version": content_version,
            "id": message.id,
            "agent_id": message.agent_id,
            "created_at": message.created_at,
            "kind": message.kind,
            "origin": message.origin,
            "authority_class": message.authority_class,
            "priority": message.priority,
            "delivery_surface": message.delivery_surface,
            "admission_context": message.admission_context,
            "work_item_id": message.work_item_id,
            "task_id": message.task_id,
            "routing_context": message.routing_context,
            "source_refs": message.source_refs,
            "correlation_id": message.correlation_id,
            "causation_id": message.causation_id,
            "full_metadata_source_ref": source_ref,
        }))?;
        let reply = crate::context::render_message_reply_expectation_context(message)
            .map(|text| format!("{text}\n"))
            .unwrap_or_default();
        let prefix = format!(
            "- reply_context_id: {}\n  source_ref: {source_ref}\n  provenance: {provenance}\n{reply}",
            message.id,
        );
        let rendered = match preview {
            None => format!("{prefix}  representation: full\n  body:\n{body}\n"),
            Some(chars) => {
                let body: String = body.chars().take(chars).collect();
                format!(
                    "{prefix}  representation: bounded_preview\n  body preview:\n{body}\n[bounded preview; original not fully read; retrieve with MemoryGet({source_ref})]\n"
                )
            }
        };
        Ok(IdleInputBatchItem {
            message_id: message.id.clone(),
            source_ref,
            rendered,
        })
    }

    fn material_fixture() -> Result<(InputEvidenceBudgetLedger, InputEvidenceScope)> {
        Ok((
            InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
            InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: 1,
            },
        ))
    }

    fn rendered_bytes(items: &[ProjectionView<'_>]) -> Result<usize> {
        items.iter().try_fold(0usize, |bytes, item| {
            bytes
                .checked_add(item.rendered.measure()?.0)
                .ok_or_else(material_overflow)
        })
    }

    #[test]
    fn prepared_projection_matches_original_bytes_for_all_body_shapes() -> Result<()> {
        let bodies = [
            MessageBody::Text {
                text: String::new(),
            },
            MessageBody::Text {
                text: "a更正🙂\n\"\\\0".into(),
            },
            MessageBody::Json {
                value: serde_json::json!({"z": [null, "更正🙂"], "a": {"quote": "\\\""}}),
            },
            MessageBody::Brief {
                title: Some("not part of the visible body".into()),
                text: "brief更正🙂\n".into(),
                attachments: Some(Vec::new()),
            },
        ];
        let origins = [
            MessageOrigin::Operator {
                actor_id: Some("actor".into()),
                actor_display_name: Some("显示🙂".into()),
            },
            MessageOrigin::Channel {
                channel_id: "channel".into(),
                sender_id: Some("sender".into()),
            },
            MessageOrigin::Webhook {
                source: "fixture".into(),
                event_type: Some("changed".into()),
            },
            MessageOrigin::Callback {
                descriptor_id: "callback".into(),
                source: Some("source".into()),
            },
            MessageOrigin::Timer {
                timer_id: "timer".into(),
            },
            MessageOrigin::System {
                subsystem: "fixture".into(),
            },
            MessageOrigin::Task {
                task_id: "waiting-task".into(),
            },
        ];
        for body in bodies {
            for origin in &origins {
                let mut message = required_reply("");
                message.authority_class = AuthorityClass::ExternalEvidence;
                message.body = body.clone();
                message.origin = origin.clone();
                message.work_item_id = Some("work".into());
                message.task_id = Some("task".into());
                message.source_refs.insert("z".into(), "last".into());
                message.source_refs.insert("a".into(), "first".into());
                message.routing_context = Some(serde_json::from_value(serde_json::json!({
                    "message_id": message.id,
                    "sender_agent_id": "sender",
                    "recipient_agent_id": "default",
                    "reply_to_agent_id": "reply",
                    "correlation_id": "correlation",
                    "in_reply_to_message_id": "original",
                    "original_sender_agent_id": "first-sender",
                    "original_reply_to_agent_id": "first-reply",
                }))?);
                let source = PreparedProjection::new(&message)?;
                let full = original_item(&message, None)?;
                assert_eq!(source.full().rendered.to_string(), full.rendered);
                assert_eq!(
                    source.full().rendered.measure()?,
                    (full.rendered.len(), full.rendered.chars().count())
                );
                assert_eq!(project_item(&message, usize::MAX, usize::MAX)?, Some(full));
                if source.may_preview {
                    for chars in [0, 1, source.body_chars.saturating_sub(1)] {
                        let expected = original_item(&message, Some(chars))?;
                        assert_eq!(
                            source.preview(chars).rendered.to_string(),
                            expected.rendered
                        );
                        validate_projection(&message, &expected)?;
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn group_trials_borrow_bodies_and_only_admit_selected_strings() -> Result<()> {
        let first = evidence(&"更正🙂\n".repeat(4096));
        let reply = required_reply("complete instruction");
        let body = match &first.body {
            MessageBody::Text { text } => text.as_ptr(),
            _ => unreachable!(),
        };
        let (ledger, scope) = material_fixture()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
        let mut checks = 0;
        let (items, before, after) = {
            let _active = phase.enter();
            let sources = [
                PreparedProjection::new(&first)?,
                PreparedProjection::new(&reply)?,
            ];
            assert!(matches!(&sources[0].body, Cow::Borrowed(_)));
            let limit = sources[0].preview(0).rendered.measure()?.0
                + sources[1].full().rendered.measure()?.0
                + 37;
            let before = phase.finish()?;
            let items = fit_group(&sources, |items| {
                checks += 1;
                assert_eq!(items[0].rendered.0[2].as_ptr(), body);
                let before = phase.finish()?;
                let fits = rendered_bytes(items)? <= limit;
                assert_eq!(
                    phase.finish()?,
                    before,
                    "trial measurement owns no material"
                );
                Ok(fits)
            })?
            .expect("complete reply and a bounded first source fit");
            (items, before, phase.finish()?)
        };
        assert!(checks > 4);
        assert!(items[0].rendered.contains("bounded_preview"));
        assert_eq!(items[1], original_item(&reply, None)?);
        let count = items.len();
        let records = 2 * (count + 1) + 3 * count;
        let metadata = size_of::<Vec<ProjectionView<'_>>>()
            + count * size_of::<ProjectionView<'_>>()
            + size_of::<Vec<IdleInputBatchItem>>()
            + count * size_of::<IdleInputBatchItem>()
            + 3 * count * size_of::<String>();
        let text: usize = items
            .iter()
            .map(|item| item.message_id.len() + item.source_ref.len() + item.rendered.len())
            .sum();
        assert_eq!(after.1 - before.1, records * 8);
        assert_eq!(after.0 - before.0, u64::try_from(metadata * 8 + text)?);
        Ok(())
    }

    #[test]
    fn projection_material_exact_limits_and_one_unit_short_are_latched() -> Result<()> {
        let message = evidence(&"界🙂\n".repeat(4096));
        let (ledger, scope) = material_fixture()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
        let expected = {
            let _active = phase.enter();
            project_item(&message, 400, 1600)?.expect("bounded preview fits")
        };
        let (bytes, records) = phase.finish()?;
        drop(phase);
        for (byte_limit, record_limit, accepted) in [
            (bytes, records, true),
            (bytes - 1, records, false),
            (bytes, records - 1, false),
        ] {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope, byte_limit, record_limit)?;
            let result = {
                let _active = phase.enter();
                project_item(&message, 400, 1600)
            };
            if accepted {
                assert_eq!(result?, Some(expected.clone()));
                assert_eq!(phase.finish()?, (bytes, records));
            } else {
                assert_typed_resource_exhausted(&result.unwrap_err());
                assert!(phase.finish().is_err());
                let _active = phase.enter();
                assert!(materialize((&expected).into()).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn group_material_exact_limits_and_one_unit_short_are_latched() -> Result<()> {
        let first = evidence(&"更正🙂".repeat(1024));
        let reply = required_reply("complete instruction");
        let run = || -> Result<Option<Vec<IdleInputBatchItem>>> {
            let sources = [
                PreparedProjection::new(&first)?,
                PreparedProjection::new(&reply)?,
            ];
            let limit = sources[0].preview(0).rendered.measure()?.0
                + sources[1].full().rendered.measure()?.0
                + 17;
            fit_group(&sources, |items| Ok(rendered_bytes(items)? <= limit))
        };
        let (ledger, scope) = material_fixture()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
        let expected = {
            let _active = phase.enter();
            run()?.expect("linked sources fit")
        };
        let (bytes, records) = phase.finish()?;
        drop(phase);
        for (byte_limit, record_limit, accepted) in [
            (bytes, records, true),
            (bytes - 1, records, false),
            (bytes, records - 1, false),
        ] {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope, byte_limit, record_limit)?;
            let result = {
                let _active = phase.enter();
                run()
            };
            if accepted {
                assert_eq!(result?, Some(expected.clone()));
                assert_eq!(phase.finish()?, (bytes, records));
            } else {
                assert_typed_resource_exhausted(&result.unwrap_err());
                assert!(phase.finish().is_err());
                let _active = phase.enter();
                assert!(resource_io::charge_owned_buffer(0).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn group_callback_cannot_hide_latched_material_failure() -> Result<()> {
        let message = evidence("source");
        let (ledger, scope) = material_fixture()?;
        for accepted in [true, false] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 4096)?;
            let _active = phase.enter();
            let sources = [PreparedProjection::new(&message)?];
            let result = fit_group(&sources, |_| {
                assert!(resource_io::charge_owned_buffer(u64::MAX).is_err());
                Ok(accepted)
            });
            assert_typed_resource_exhausted(&result.unwrap_err());
            assert!(phase.finish().is_err());
        }
        Ok(())
    }

    #[test]
    fn group_greedy_unicode_boundary_and_source_drift_are_preserved() -> Result<()> {
        let first = evidence(&"🙂更正".repeat(128));
        let reply = required_reply("complete instruction");
        let sources = [
            PreparedProjection::new(&first)?,
            PreparedProjection::new(&reply)?,
        ];
        let minimum =
            sources[0].preview(0).rendered.measure()?.0 + sources[1].full().rendered.measure()?.0;
        let items = fit_group(&sources, |items| Ok(rendered_bytes(items)? <= minimum + 4))?
            .expect("one complete Unicode scalar fits");
        assert_eq!(items[0], original_item(&first, Some(1))?);
        assert_eq!(items[1], original_item(&reply, None)?);
        assert!(fit_group(&sources, |items| Ok(rendered_bytes(items)? < minimum))?.is_none());
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        storage.append_message(&first)?;
        let mut changed = first.clone();
        changed.body = MessageBody::Text {
            text: "different body".into(),
        };
        let mut changed_owner = first.clone();
        changed_owner.work_item_id = Some("different owner".into());
        let mut changed_refs = first.clone();
        changed_refs
            .source_refs
            .insert("source".into(), "changed".into());
        let mut changed_origin = first.clone();
        changed_origin.origin = MessageOrigin::Task {
            task_id: "different task".into(),
        };
        let mut changed_route = first.clone();
        changed_route.routing_context = Some(serde_json::from_value(serde_json::json!({
            "message_id": first.id,
            "recipient_agent_id": "different recipient",
        }))?);
        let mut changed_authority = first.clone();
        changed_authority.authority_class = AuthorityClass::RuntimeInstruction;
        for changed in [
            changed,
            changed_owner,
            changed_refs,
            changed_origin,
            changed_route,
            changed_authority,
        ] {
            assert!(persisted_group(&storage, &[changed], |_| {
                panic!("changed source must be rejected before capacity trials")
            })?
            .is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn qualified_projection_rechecks_source_in_each_idle_read() -> Result<()> {
        let harness = crate::runtime::tests::support::LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let message = evidence("qualified source 更正🙂");
        runtime.enqueue(message.clone()).await?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: runtime.inner.identity_incarnation,
        };
        let first = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
        let loan = runtime.with_idle_read(None, &first, |read| {
            Ok(read
                .shared_message(&message.id)?
                .expect("qualified Message"))
        })?;
        let second = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
        let selected = runtime
            .with_idle_read(None, &second, |read| {
                let mut changed = (*loan).clone();
                changed.body = MessageBody::Text {
                    text: "changed body".into(),
                };
                assert!(
                    qualified_item_after(read, &changed, usize::MAX, usize::MAX, "",)?.is_none()
                );
                qualified_group(read, std::slice::from_ref(&loan), |_| Ok(true))
            })?
            .expect("unchanged source is freshly qualified in the second allowance");
        assert_eq!(selected, vec![original_item(&loan, None)?]);
        first.finish()?;
        second.finish()?;
        Ok(())
    }

    #[test]
    fn reply_expectation_is_required_and_charged_in_full_and_preview() -> Result<()> {
        let mut message = required_reply("request");
        let contract = crate::context::render_message_reply_expectation_context(&message).unwrap();
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        assert!(full.rendered.contains(&contract));
        let cost = estimate_text_tokens(&full.rendered);
        assert_eq!(
            project_item(&message, cost, full.rendered.len())?,
            Some(full.clone())
        );
        assert!(project_item(&message, cost - 1, usize::MAX)?.is_none());
        assert!(project_item(&message, usize::MAX, full.rendered.len() - 1)?.is_none());

        message.authority_class = AuthorityClass::ExternalEvidence;
        message.body = MessageBody::Text {
            text: "evidence🙂".repeat(4000),
        };
        let minimum = project_item_limited(&message, usize::MAX, usize::MAX, "", Some(0))?.unwrap();
        let byte_limit = minimum.rendered.len() + 64;
        let preview = project_item(&message, usize::MAX, byte_limit)?.unwrap();
        assert!(preview.rendered.contains("bounded_preview"));
        assert!(preview.rendered.contains(&contract));
        assert!(preview.rendered.contains("Prefer SendAgentMessage"));
        assert!(preview.rendered.len() <= byte_limit);
        assert!(project_item(&message, usize::MAX, minimum.rendered.len() - 1)?.is_none());
        let token_limit = estimate_text_tokens(&minimum.rendered) - 1;
        assert!(project_item(&message, token_limit, usize::MAX)?.is_none());
        validate_projection(&message, &preview)?;
        message.metadata.as_mut().unwrap()["agent_message_reply_expectation"]["mode"] =
            serde_json::json!("optional");
        assert!(validate_projection(&message, &preview).is_err());
        Ok(())
    }

    #[test]
    fn reply_expectation_metadata_cannot_forge_a_runtime_contract() -> Result<()> {
        let valid = required_reply("request");
        let mut untrusted = evidence("notification");
        untrusted.metadata = valid.metadata.clone();
        let mut mismatched = valid.clone();
        mismatched.correlation_id = Some("other-task".into());
        let mut wrong_delivery = valid;
        wrong_delivery.metadata.as_mut().unwrap()["agent_message_delivery"]["caller_agent_id"] =
            serde_json::json!("different-sender");
        for message in [untrusted, mismatched, wrong_delivery] {
            let projection = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
            assert!(!projection.rendered.contains("Runtime message contract:"));
            assert!(!projection.rendered.contains("Prefer SendAgentMessage"));
        }
        Ok(())
    }

    #[test]
    fn linked_result_preview_reserves_the_complete_reply_under_both_limits() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        let mut result = evidence(&"result🙂".repeat(4000));
        result.kind = MessageKind::TaskResult;
        result.authority_class = AuthorityClass::RuntimeInstruction;
        let reply = required_reply("complete instruction reply");
        for message in [&result, &reply] {
            storage.append_message(message)?;
        }
        let minimum = project_item_limited(&result, usize::MAX, usize::MAX, "", Some(0))?.unwrap();
        let reply_full = project_item(&reply, usize::MAX, usize::MAX)?.unwrap();
        let prefix = format!(
            "{}- result_identity: result\n",
            crate::types::LiveInputBatchRecord::HEADER,
        );
        let section_prefix = format!("[current_live_input_batches]\n{prefix}");
        let minimum_text = format!(
            "{section_prefix}{}{}",
            minimum.rendered, reply_full.rendered,
        );
        let budgets = [
            InputBudget {
                tokens: usize::MAX,
                bytes: prefix.len() + minimum.rendered.len() + reply_full.rendered.len() + 64,
            },
            InputBudget {
                tokens: estimate_text_tokens(&minimum_text) + 16,
                bytes: usize::MAX,
            },
        ];
        for budget in budgets {
            let greedy = project_item_after(
                &result,
                budget.tokens,
                budget.bytes.saturating_sub(prefix.len()),
                &section_prefix,
            )?
            .unwrap();
            assert!(project_item_after(
                &reply,
                budget.tokens,
                budget
                    .bytes
                    .saturating_sub(prefix.len() + greedy.rendered.len()),
                &format!("{section_prefix}{}", greedy.rendered),
            )?
            .is_none());
            let items = persisted_group(&storage, &[result.clone(), reply.clone()], |items| {
                let text = format!("{prefix}{}{}", items[0].rendered, items[1].rendered);
                Ok(text.len() <= budget.bytes && budget.fits("current_live_input_batches", &text))
            })?
            .expect("result preview and complete reply fit together");
            assert!(items[0].rendered.contains("bounded_preview"));
            assert_eq!(items[1], reply_full);
            validate_projection(&result, &items[0])?;
            validate_projection(&reply, &items[1])?;
        }
        Ok(())
    }

    #[test]
    fn full_projection_fits_at_exact_token_minimum() -> Result<()> {
        let message = evidence("更正🙂");
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        let cost = estimate_text_tokens(&full.rendered);
        assert_eq!(
            project_item(&message, cost, full.rendered.len())?,
            Some(full)
        );
        Ok(())
    }

    #[test]
    fn compact_is_utf8_safe_versioned_and_keeps_provenance() -> Result<()> {
        let message = evidence(&"更正🙂".repeat(4000));
        let compact = project_item(&message, 400, 1600)?.unwrap();
        assert!(compact.rendered.len() <= 1600);
        assert!(estimate_text_tokens(&compact.rendered) <= 400);
        for required in [
            "bounded_preview",
            "projection_version",
            "content_version",
            "sha256:",
            "authority_class",
            "routing_context",
            "work_item_id",
            "agent_id",
            "original not fully read",
        ] {
            assert!(compact.rendered.contains(required), "{required}");
        }
        assert!(compact.rendered.contains(&compact.source_ref));
        Ok(())
    }

    #[test]
    fn no_projection_can_discard_metadata_or_shorten_instructions() -> Result<()> {
        let mut message = evidence(&"instruction".repeat(1000));
        assert!(project_item(&message, 1, usize::MAX)?.is_none());
        message.authority_class = AuthorityClass::OperatorInstruction;
        message.origin = MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        };
        assert!(project_item(&message, 400, 1600)?.is_none());
        message.origin = MessageOrigin::System {
            subsystem: "fixture".into(),
        };
        message.authority_class = AuthorityClass::RuntimeInstruction;
        assert!(project_item(&message, 400, 1600)?.is_none());
        Ok(())
    }

    #[test]
    fn section_rounding_is_shared_at_unicode_exact_minimum() -> Result<()> {
        let mut message = evidence("更正🙂");
        message.authority_class = AuthorityClass::OperatorInstruction;
        let prefix = format!("[current_input]\n{BATCH_HEADER}");
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        let tokens = estimate_text_tokens(&format!("{prefix}{}", full.rendered));
        assert_eq!(
            project_item_after(&message, tokens, usize::MAX, &prefix)?,
            Some(full)
        );
        assert!(project_item_after(&message, tokens - 1, usize::MAX, &prefix)?.is_none());
        Ok(())
    }

    #[test]
    fn projections_bind_content_version_and_entire_reply_route() -> Result<()> {
        let message = evidence(&"evidence🙂".repeat(1000));
        let item = project_item(&message, 400, 1600)?.unwrap();
        validate_projection(&message, &item)?;
        let mut changed = message.clone();
        changed.body = crate::types::MessageBody::Text {
            text: "changed".into(),
        };
        assert!(validate_projection(&changed, &item).is_err());
        changed = message.clone();
        changed.work_item_id = Some("other-owner".into());
        assert!(validate_projection(&changed, &item).is_err());
        let mut forged = item;
        forged.rendered = forged
            .rendered
            .replace("body preview:\n", "body preview:\nforged");
        assert!(validate_projection(&message, &forged).is_err());
        Ok(())
    }
}
