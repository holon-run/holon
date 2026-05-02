use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChatScrollState {
    follow_tail: bool,
    offset_from_bottom: u16,
}

impl ChatScrollState {
    pub(super) fn new() -> Self {
        Self {
            follow_tail: true,
            offset_from_bottom: 0,
        }
    }

    pub(super) fn follow_tail(&mut self) {
        self.follow_tail = true;
        self.offset_from_bottom = 0;
    }

    pub(super) fn scroll_with_key(&mut self, key: KeyCode, max_scroll: u16) {
        match key {
            KeyCode::Up => self.scroll_away_from_bottom(1),
            KeyCode::PageUp => self.scroll_away_from_bottom(10),
            KeyCode::Home => {
                self.follow_tail = false;
                self.offset_from_bottom = u16::MAX;
            }
            KeyCode::Down => self.scroll_toward_tail(1, max_scroll),
            KeyCode::PageDown => self.scroll_toward_tail(10, max_scroll),
            KeyCode::End => self.follow_tail(),
            _ => {}
        }
    }

    pub(super) fn effective_scroll(self, max_scroll: u16) -> u16 {
        if self.follow_tail {
            max_scroll
        } else {
            max_scroll.saturating_sub(self.offset_from_bottom.min(max_scroll))
        }
    }

    #[cfg(test)]
    pub(super) fn is_following_tail(self) -> bool {
        self.follow_tail
    }

    fn scroll_away_from_bottom(&mut self, delta: u16) {
        self.follow_tail = false;
        self.offset_from_bottom = self.offset_from_bottom.saturating_add(delta);
    }

    fn scroll_toward_tail(&mut self, delta: u16, max_scroll: u16) {
        if self.follow_tail {
            return;
        }
        if self.offset_from_bottom > max_scroll {
            self.offset_from_bottom = max_scroll.saturating_sub(delta);
        } else {
            self.offset_from_bottom = self.offset_from_bottom.saturating_sub(delta);
        }
        if self.offset_from_bottom == 0 {
            self.follow_tail = true;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CachedChatRole {
    Operator,
    Agent,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedChatItem {
    pub(super) created_at: DateTime<chrono::Utc>,
    pub(super) role: CachedChatRole,
    pub(super) speaker: String,
    pub(super) body: String,
}

#[derive(Clone)]
pub(super) struct CachedChatText {
    pub(super) items: Vec<CachedChatItem>,
    pub(super) text: Text<'static>,
}

pub(super) fn collect_chat_items(app: &TuiApp) -> Vec<CachedChatItem> {
    let mut items = Vec::new();

    for entry in &app.transcript {
        if entry.kind != TranscriptEntryKind::IncomingMessage {
            continue;
        }
        if !entry
            .data
            .get("origin")
            .is_some_and(is_operator_origin_value)
        {
            continue;
        }
        let body = entry
            .data
            .get("body")
            .and_then(render_message_body_value)
            .unwrap_or_else(|| compact_json(&entry.data));
        items.push(CachedChatItem {
            created_at: entry.created_at,
            role: CachedChatRole::Operator,
            speaker: "You".to_string(),
            body,
        });
    }

    for brief in &app.briefs {
        if matches!(brief.kind, crate::types::BriefKind::Ack) {
            continue;
        }
        items.push(CachedChatItem {
            created_at: brief.created_at,
            role: CachedChatRole::Agent,
            speaker: match brief.kind {
                crate::types::BriefKind::Result => "Holon".to_string(),
                crate::types::BriefKind::Failure => "Holon (failed)".to_string(),
                crate::types::BriefKind::Ack => unreachable!("ack briefs are filtered above"),
            },
            body: render_brief_body(brief),
        });
    }

    if let Some(projection) = app.projection.as_ref() {
        let mut visible_event_ids = std::collections::HashSet::new();

        for event in projection.durable_conversation_events() {
            if !is_chat_visible_conversation_event(&event.kind) {
                continue;
            }
            items.push(CachedChatItem {
                created_at: event.ts,
                role: CachedChatRole::System,
                speaker: conversation_event_speaker(&event.kind),
                body: conversation_event_body(event),
            });
            visible_event_ids.insert(event.id.as_str());
        }

        for event in projection.recent_activity_events() {
            if visible_event_ids.contains(event.id.as_str()) {
                continue;
            }
            items.push(CachedChatItem {
                created_at: event.ts,
                role: CachedChatRole::System,
                speaker: conversation_event_speaker(&event.kind),
                body: progress_event_body(event),
            });
        }
    }

    items.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| chat_role_rank(left.role).cmp(&chat_role_rank(right.role)))
            .then_with(|| left.speaker.cmp(&right.speaker))
            .then_with(|| left.body.cmp(&right.body))
    });
    items
}

pub(super) fn is_chat_visible_conversation_event(kind: &str) -> bool {
    matches!(kind, "operator_notification_requested" | "runtime_error")
}

pub(super) fn build_chat_text(items: &[CachedChatItem]) -> Text<'static> {
    let mut text = Text::default();
    for (index, item) in items.iter().enumerate() {
        append_chat_item(&mut text, item);
        if index + 1 < items.len() {
            text.lines.push(Line::default());
        }
    }

    text
}

pub(super) fn chat_text(app: &TuiApp) -> Text<'static> {
    let items = collect_chat_items(app);
    if items.is_empty() {
        *app.chat_text_cache.borrow_mut() = None;
        return Text::from("No chat history yet. Type a message to the selected agent.");
    }

    if let Some(cached) = app.chat_text_cache.borrow().as_ref() {
        if cached.items == items {
            return cached.text.clone();
        }
    }

    let text = build_chat_text(&items);
    *app.chat_text_cache.borrow_mut() = Some(CachedChatText {
        items,
        text: text.clone(),
    });
    text
}

fn append_chat_item(target: &mut Text<'static>, item: &CachedChatItem) {
    let body = render_markdown_text(&item.body);
    let body_lines = body.lines;
    let prefix = chat_prefix_text(item);
    let continuation_indent = " ".repeat(prefix.chars().count());

    if let Some((first, rest)) = body_lines.split_first() {
        let mut spans = Vec::with_capacity(first.spans.len() + 3);
        spans.push(Span::styled(
            chat_timestamp(item),
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(
            item.speaker.clone(),
            chat_speaker_style(item.role),
        ));
        spans.push(Span::raw("  "));
        spans.extend(first.spans.clone());
        target.lines.push(Line::from(spans).style(first.style));

        for line in rest {
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::raw(continuation_indent.clone()));
            spans.extend(line.spans.clone());
            target.lines.push(Line::from(spans).style(line.style));
        }
    } else {
        target.lines.push(Line::from(vec![
            Span::styled(
                chat_timestamp(item),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled(item.speaker.clone(), chat_speaker_style(item.role)),
        ]));
    }

    // Add extra spacing between messages for better readability.
    // Two blank lines makes message separation more visually distinct.
    target.lines.push(Line::from(""));
    target.lines.push(Line::from(""));
}

fn chat_prefix_text(item: &CachedChatItem) -> String {
    format!("{}{}  ", chat_timestamp(item), item.speaker)
}

fn chat_timestamp(item: &CachedChatItem) -> String {
    format!(
        "[{}] ",
        item.created_at.with_timezone(&Local).format("%H:%M")
    )
}

fn chat_speaker_style(role: CachedChatRole) -> Style {
    match role {
        CachedChatRole::Operator | CachedChatRole::Agent => {
            Style::default().add_modifier(Modifier::BOLD)
        }
        CachedChatRole::System => Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::DIM),
    }
}

fn chat_role_rank(role: CachedChatRole) -> u8 {
    match role {
        CachedChatRole::Operator => 0,
        CachedChatRole::Agent => 1,
        CachedChatRole::System => 2,
    }
}

pub(super) fn paragraph_max_scroll(text: &Text<'_>, area: Rect) -> u16 {
    let inner_width = area.width.saturating_sub(2).max(1);
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_height == 0 {
        return 0;
    }

    let line_count = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner_width);
    line_count
        .saturating_sub(inner_height)
        .min(u16::MAX as usize) as u16
}

pub(super) fn is_operator_origin_value(value: &Value) -> bool {
    value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "operator")
}

fn render_message_body_value(value: &Value) -> Option<String> {
    let body_type = value.get("type").and_then(Value::as_str)?;
    match body_type {
        "text" | "brief" => value
            .get("text")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        "json" => value.get("value").map(compact_json),
        _ => None,
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".into())
}

fn conversation_event_speaker(kind: &str) -> String {
    match kind {
        "task_created" | "task_status_updated" | "task_result_received" | "work_item_written" => {
            "System (work)".into()
        }
        "waiting_intent_created" | "waiting_intent_cancelled" | "callback_delivered" => {
            "System (waiting)".into()
        }
        "workspace_entered" | "workspace_exited" | "worktree_entered" | "worktree_exited" => {
            "System (workspace)".into()
        }
        "runtime_error" | "turn_terminal" => "System (runtime)".into(),
        _ => "System".into(),
    }
}

pub(super) fn conversation_event_body(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> String {
    let prefix = match event.kind.as_str() {
        "task_created" => "[task] ",
        "task_status_updated" => "[task] ",
        "task_result_received" => "[task] ",
        "work_item_written" => "[work-item] ",
        "waiting_intent_created" => "[external-trigger] ",
        "waiting_intent_cancelled" => "[external-trigger] ",
        "callback_delivered" => "[external-trigger] ",
        "workspace_entered" => "[workspace] ",
        "workspace_exited" => "[workspace] ",
        "worktree_entered" => "[worktree] ",
        "worktree_exited" => "[worktree] ",
        "runtime_error" => "[runtime-error] ",
        "turn_terminal" => "[turn] ",
        _ => "",
    };
    format!("{prefix}{}", event.summary)
}

fn progress_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if event.kind == "tool_executed" || event.kind == "tool_execution_failed" {
        return event
            .payload
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
            .map(|tool_name| {
                if tool_name == "ExecCommand" {
                    event
                        .payload
                        .get("exec_command_cmd")
                        .and_then(serde_json::Value::as_str)
                        .map(|cmd| {
                            if event.kind == "tool_execution_failed" {
                                format!("ExecCommand failed: {cmd}")
                            } else {
                                format!("ExecCommand: {cmd}")
                            }
                        })
                        .unwrap_or_else(|| event.summary.clone())
                } else {
                    event.summary.clone()
                }
            })
            .unwrap_or_else(|| event.summary.clone());
    }
    conversation_event_body(event)
}

fn render_brief_body(brief: &BriefRecord) -> String {
    if let Some(task_id) = brief.related_task_id.as_deref() {
        let preview = brief
            .text
            .lines()
            .next()
            .map(collapse_whitespace)
            .unwrap_or_default();
        let preview = trim_preview(&preview, 160);
        if brief.text.contains('\n') || brief.text.chars().count() > 160 {
            return format!(
                "Task {task_id}: {preview}\n_Task output is available in the Tasks pane._"
            );
        }
        return format!("Task {task_id}: {preview}");
    }
    brief.text.clone()
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_preview(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut trimmed = input
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    trimmed.push('…');
    trimmed
}

#[cfg(test)]
mod tests {
    use super::{is_chat_visible_conversation_event, progress_event_body};
    use crate::tui::projection::{ProjectionEventLane, ProjectionEventRecord};
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn progress_event_body_shows_full_exec_command() {
        let event = ProjectionEventRecord {
            id: "evt-1".into(),
            seq: 1,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: "tool_executed".into(),
            summary: "tool executed: ExecCommand".into(),
            payload: json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "git status --short --branch"
            }),
        };
        let rendered = progress_event_body(&event);
        assert_eq!(rendered, "ExecCommand: git status --short --branch");
    }

    #[test]
    fn progress_event_body_marks_failed_exec_command() {
        let event = ProjectionEventRecord {
            id: "evt-2".into(),
            seq: 2,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: "tool_execution_failed".into(),
            summary: "tool execution failed: ExecCommand".into(),
            payload: json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test tui"
            }),
        };

        let rendered = progress_event_body(&event);

        assert_eq!(rendered, "ExecCommand failed: cargo test tui");
    }

    #[test]
    fn progress_event_body_uses_summary_for_non_command_tools() {
        let event = ProjectionEventRecord {
            id: "evt-2".into(),
            seq: 2,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: "tool_executed".into(),
            summary: "tool executed: Sleep".into(),
            payload: json!({
                "tool_name": "Sleep"
            }),
        };
        let rendered = progress_event_body(&event);
        assert_eq!(rendered, "tool executed: Sleep");
    }

    #[test]
    fn chat_visible_conversation_events_are_user_facing_only() {
        assert!(is_chat_visible_conversation_event(
            "operator_notification_requested"
        ));
        assert!(is_chat_visible_conversation_event("runtime_error"));

        assert!(!is_chat_visible_conversation_event("work_item_written"));
        assert!(!is_chat_visible_conversation_event(
            "waiting_intent_created"
        ));
        assert!(!is_chat_visible_conversation_event(
            "waiting_intent_cancelled"
        ));
        assert!(!is_chat_visible_conversation_event("callback_delivered"));
        assert!(!is_chat_visible_conversation_event("workspace_attached"));
        assert!(!is_chat_visible_conversation_event(
            "provider_round_completed"
        ));
    }
}
