use crate::types::{BriefKind, BriefRecord, MessageEnvelope};

pub fn make_ack(agent_id: &str, message: &MessageEnvelope) -> BriefRecord {
    let preview = preview_message(message);
    BriefRecord::new(
        agent_id,
        BriefKind::Ack,
        format!("Queued work: {preview}"),
        Some(message.id.clone()),
        None,
    )
}

pub fn make_result(
    agent_id: &str,
    message: &MessageEnvelope,
    text: impl Into<String>,
) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Result,
        text,
        Some(message.id.clone()),
        None,
    )
}

pub fn make_failure(
    agent_id: &str,
    message: &MessageEnvelope,
    text: impl Into<String>,
) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Failure,
        text,
        Some(message.id.clone()),
        None,
    )
}

pub fn make_task_result(agent_id: &str, task_id: &str, text: impl Into<String>) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Result,
        text,
        None,
        Some(task_id.to_string()),
    )
}

fn preview_message(message: &MessageEnvelope) -> String {
    match &message.body {
        crate::types::MessageBody::Text { text } => truncate(text, 80),
        crate::types::MessageBody::Json { value } => truncate(&value.to_string(), 80),
        crate::types::MessageBody::Brief { text, .. } => truncate(text, 80),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    format!("{truncated}...")
}
