//! Formatting and exact capacity trials for admitted terminal Task results.

use std::{collections::BTreeSet, fmt};

use holon_domain::{
    agent_message_reply_reference, execution_protocol::TaskRejoinSource, IdleInputBatchItem,
    MessageEnvelope,
};

use super::{body_preview, message_header, message_routing_context, sanitize_inline};

/// Prompt fields borrowed from an authorized terminal result.
#[derive(Clone, Copy)]
pub struct TaskResultHeader<'a> {
    pub result_identity: &'a str,
    pub task_id: &'a str,
    pub task_status: &'a str,
    pub work_item_id: Option<&'a str>,
    pub source: &'a TaskRejoinSource,
    pub rejoin_generation: u64,
}

/// The observed message ID and its rendered text. Capacity trials can stream
/// borrowed fragments into the formatter without owning a combined String.
#[derive(Clone, Copy)]
pub struct TaskResultProjection<'a> {
    pub message_id: &'a str,
    pub rendered: &'a dyn fmt::Display,
}

impl<'a> From<&'a IdleInputBatchItem> for TaskResultProjection<'a> {
    fn from(item: &'a IdleInputBatchItem) -> Self {
        Self {
            message_id: &item.message_id,
            rendered: &item.rendered,
        }
    }
}

pub const PENDING_TASK_RESULT_PREVIEW_LINES: usize = 11;

/// Immutable bounded text, shared by the prompt and idle capacity trials.
pub struct PendingTaskResultPreview {
    lines: Vec<String>,
    result_identity: String,
    source_id: Option<String>,
    source_lines: std::ops::Range<usize>,
    reply_id: Option<String>,
    reply_lines: std::ops::Range<usize>,
}

struct PendingTaskResultInline<'a>(&'a str);

impl std::fmt::Display for PendingTaskResultInline<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut words = self.0.split_whitespace();
        if let Some(first) = words.next() {
            formatter.write_str(first)?;
            for word in words {
                formatter.write_str(" ")?;
                formatter.write_str(word)?;
            }
        }
        Ok(())
    }
}

fn write_pending_task_result_header(
    record: TaskResultHeader<'_>,
    mut line: impl FnMut(std::fmt::Arguments<'_>) -> std::fmt::Result,
) -> std::fmt::Result {
    line(format_args!(
        "- result_identity: {}",
        PendingTaskResultInline(record.result_identity)
    ))?;
    line(format_args!(
        "  task_id: {}",
        PendingTaskResultInline(record.task_id)
    ))?;
    line(format_args!(
        "  task_status: {}",
        PendingTaskResultInline(record.task_status)
    ))?;
    if let Some(id) = record.work_item_id {
        line(format_args!("  owner: work_item:{id}"))
    } else {
        line(format_args!("  owner: agent_lifecycle"))
    }
}

fn write_pending_task_result_projection_header(
    record: TaskResultHeader<'_>,
    mut line: impl FnMut(std::fmt::Arguments<'_>) -> std::fmt::Result,
) -> std::fmt::Result {
    let (source_key, source_id) = match record.source {
        TaskRejoinSource::ParentTurn { parent_turn_id } => ("parent_turn_id", parent_turn_id),
        TaskRejoinSource::CommandCreation {
            command_creation_id,
        } => ("command_creation_id", command_creation_id),
    };
    line(format_args!(
        "  {source_key}: {}",
        PendingTaskResultInline(source_id)
    ))?;
    line(format_args!(
        "  rejoin_generation: {}",
        record.rejoin_generation
    ))
}

#[derive(Clone, Copy)]
enum PendingTaskResultPreviewEntry<'a> {
    Retained(&'a PendingTaskResultPreview),
    Projected {
        record: TaskResultHeader<'a>,
        source: TaskResultProjection<'a>,
        reply: Option<TaskResultProjection<'a>>,
    },
}

impl<'a> PendingTaskResultPreviewEntry<'a> {
    fn result_identity(self) -> &'a str {
        match self {
            Self::Retained(preview) => &preview.result_identity,
            Self::Projected { record, .. } => record.result_identity,
        }
    }

    fn source_id(self) -> Option<&'a str> {
        match self {
            Self::Retained(preview) => preview.source_id.as_deref(),
            Self::Projected { source, .. } => Some(source.message_id),
        }
    }

    fn reply_id(self) -> Option<&'a str> {
        match self {
            Self::Retained(preview) => preview.reply_id.as_deref(),
            Self::Projected { reply, .. } => reply.map(|reply| reply.message_id),
        }
    }

    fn write(
        self,
        writer: &mut impl std::fmt::Write,
        show_source: bool,
        show_reply: bool,
    ) -> std::fmt::Result {
        match self {
            Self::Retained(preview) => {
                for (index, line) in preview.lines.iter().enumerate() {
                    if (show_reply || !preview.reply_lines.contains(&index))
                        && (show_source || !preview.source_lines.contains(&index))
                    {
                        write!(writer, "\n{line}")?;
                    }
                }
            }
            Self::Projected {
                record,
                source,
                reply,
            } => {
                let mut line = |line: std::fmt::Arguments<'_>| write!(writer, "\n{line}");
                write_pending_task_result_header(record, &mut line)?;
                write_pending_task_result_projection_header(record, &mut line)?;
                if show_source {
                    line(format_args!("{}", source.rendered))?;
                }
                if let Some(reply) = reply.filter(|_| show_reply) {
                    line(format_args!("{}", reply.rendered))?;
                }
            }
        }
        Ok(())
    }
}

pub fn pending_task_result_preview(
    record: TaskResultHeader<'_>,
    message: Option<&MessageEnvelope>,
    reply: Option<&MessageEnvelope>,
    source_projection: Option<&IdleInputBatchItem>,
    reply_projection: Option<&IdleInputBatchItem>,
) -> PendingTaskResultPreview {
    // All optional fields fit this fixed shape; no growth copies prior lines.
    let mut lines = Vec::with_capacity(PENDING_TASK_RESULT_PREVIEW_LINES);
    write_pending_task_result_header(record, |line| {
        lines.push(line.to_string());
        Ok(())
    })
    .expect("String formatting cannot fail");
    if let Some(projection) = source_projection {
        write_pending_task_result_projection_header(record, |line| {
            lines.push(line.to_string());
            Ok(())
        })
        .expect("String formatting cannot fail");
        let source_start = lines.len();
        lines.push(projection.rendered.clone());
        let source_lines = source_start..lines.len();
        let reply_start = lines.len();
        if let Some(reply) = reply_projection {
            lines.push(reply.rendered.clone());
        }
        let reply_lines = reply_start..lines.len();
        return PendingTaskResultPreview {
            lines,
            result_identity: record.result_identity.to_owned(),
            source_id: Some(projection.message_id.clone()),
            source_lines,
            reply_id: reply_projection.map(|reply| reply.message_id.clone()),
            reply_lines,
        };
    }
    let mut reply_lines = 0..0;
    let reply_id = message.and_then(agent_message_reply_reference);
    if let Some(id) = reply_id {
        lines.push(format!(
            "  reply_source_ref: message:{}",
            sanitize_inline(id)
        ));
        let start = lines.len();
        if let Some(reply) = reply {
            lines.push(format!(
                "  received_agent_message: {}",
                message_header(reply)
            ));
            if let Some(route) = message_routing_context(reply) {
                lines.push(route);
            }
            lines.push(format!(
                "  reply_preview: {}",
                sanitize_inline(&body_preview(&reply.body))
            ));
        }
        reply_lines = start..lines.len();
    }
    let source_start = lines.len();
    if let Some(message) = message {
        lines.push(format!(
            "  summary: {}",
            sanitize_inline(&body_preview(&message.body))
        ));
        if let Some(output_ref) = message
            .source_refs
            .get("output_ref")
            .or_else(|| message.source_refs.get("stdout_ref"))
        {
            lines.push(format!("  output_ref: {}", sanitize_inline(output_ref)));
        }
    }
    let source_lines = source_start..lines.len();
    lines.push(format!(
        "  retrieval: use TaskStatus with task_id={} for exact lifecycle/result evidence; read only confirmed output refs/files boundedly",
        sanitize_inline(record.task_id)
    ));
    PendingTaskResultPreview {
        lines,
        result_identity: record.result_identity.to_owned(),
        source_id: message.map(|message| message.id.clone()),
        source_lines,
        reply_id: reply_id.map(str::to_owned),
        reply_lines,
    }
}

fn write_pending_task_result_previews<'a>(
    writer: &mut impl std::fmt::Write,
    pending: impl Iterator<Item = &'a PendingTaskResultPreview> + Clone,
    count: usize,
    current_message: &MessageEnvelope,
    excluded_messages: &BTreeSet<String>,
) -> std::fmt::Result {
    write_pending_task_result_preview_entries(
        writer,
        pending.map(PendingTaskResultPreviewEntry::Retained),
        count,
        current_message,
        excluded_messages,
    )
}

fn write_pending_task_result_preview_entries<'a>(
    writer: &mut impl std::fmt::Write,
    pending: impl Iterator<Item = PendingTaskResultPreviewEntry<'a>> + Clone,
    count: usize,
    current_message: &MessageEnvelope,
    excluded_messages: &BTreeSet<String>,
) -> std::fmt::Result {
    if count == 0 {
        return Ok(());
    }
    write!(writer, "Previously received terminal task results admitted to this execution (each result identity appears once):")?;
    // Revisit borrowed fragments instead of allocating dedup trees for each
    // capacity trial. Duplicate identities must not introduce new Message IDs.
    let unique = pending.clone().enumerate().filter(|(index, entry)| {
        !pending
            .clone()
            .take(*index)
            .any(|prior| prior.result_identity() == entry.result_identity())
    });
    for (index, entry) in unique.clone() {
        let seen = |id: &str| {
            pending
                .clone()
                .take(index)
                .enumerate()
                .any(|(prior_index, prior)| {
                    (prior.source_id() == Some(id) || prior.reply_id() == Some(id))
                        && !pending
                            .clone()
                            .take(prior_index)
                            .any(|first| first.result_identity() == prior.result_identity())
                })
        };
        let show_source = entry.source_id().is_some_and(|id| {
            id != current_message.id && !excluded_messages.contains(id) && !seen(id)
        });
        let show_reply = entry.reply_id().is_some_and(|id| {
            id != current_message.id
                && !excluded_messages.contains(id)
                && !seen(id)
                && !(show_source && entry.source_id() == Some(id))
        });
        entry.write(writer, show_source, show_reply)?;
    }
    Ok(())
}

pub fn render_pending_task_result_previews<'a>(
    pending: impl ExactSizeIterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
) -> Option<String> {
    render_pending_task_result_previews_excluding(pending, current_message, &BTreeSet::new())
}

pub fn render_pending_task_result_previews_excluding<'a>(
    pending: impl ExactSizeIterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
    excluded_messages: &BTreeSet<String>,
) -> Option<String> {
    let count = pending.len();
    if count == 0 {
        return None;
    }
    let mut rendered = String::new();
    write_pending_task_result_previews(
        &mut rendered,
        pending,
        count,
        current_message,
        excluded_messages,
    )
    .expect("String formatting cannot fail");
    Some(rendered)
}

pub fn pending_task_results_preview_bytes<'a>(
    pending: impl ExactSizeIterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
) -> usize {
    pending_task_results_preview_measure(pending, current_message).0
}

/// Exact bytes and Unicode scalar count from the final prompt formatter.
pub fn pending_task_results_preview_measure<'a>(
    pending: impl Iterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
) -> (usize, usize) {
    pending_task_result_preview_entries_measure(
        pending.map(PendingTaskResultPreviewEntry::Retained),
        current_message,
    )
}

pub fn measure_with_projected_result<'a, 'candidate>(
    pending: impl Iterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
    candidate: TaskResultHeader<'candidate>,
    source_projection: TaskResultProjection<'candidate>,
    reply_projection: Option<TaskResultProjection<'candidate>>,
) -> (usize, usize)
where
    'a: 'candidate,
{
    pending_task_result_preview_entries_measure(
        pending
            .map(|preview| PendingTaskResultPreviewEntry::Retained(preview))
            .chain(std::iter::once(PendingTaskResultPreviewEntry::Projected {
                record: candidate,
                source: source_projection,
                reply: reply_projection,
            })),
        current_message,
    )
}

fn pending_task_result_preview_entries_measure<'a>(
    pending: impl Iterator<Item = PendingTaskResultPreviewEntry<'a>> + Clone,
    current_message: &MessageEnvelope,
) -> (usize, usize) {
    struct TextCount(usize, usize);
    impl std::fmt::Write for TextCount {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            self.0 = self.0.checked_add(value.len()).ok_or(std::fmt::Error)?;
            self.1 = self
                .1
                .checked_add(value.chars().count())
                .ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    let count = pending.clone().count();
    let mut text = TextCount(0, 0);
    match write_pending_task_result_preview_entries(
        &mut text,
        pending,
        count,
        current_message,
        &BTreeSet::new(),
    ) {
        Ok(()) => (text.0, text.1),
        Err(_) => (usize::MAX, usize::MAX),
    }
}

#[cfg(test)]
mod tests;
