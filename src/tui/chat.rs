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
pub(super) struct AssistantMarkdownCell {
    created_at: DateTime<chrono::Utc>,
    agent_id: String,
    markdown: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ConversationCell {
    UserMessage {
        created_at: DateTime<chrono::Utc>,
        body: String,
    },
    AssistantMarkdown(AssistantMarkdownCell),
    ActiveActivity {
        created_at: DateTime<chrono::Utc>,
        speaker: String,
        body: String,
    },
    SystemNotice {
        created_at: DateTime<chrono::Utc>,
        speaker: String,
        body: String,
    },
}

#[derive(Clone)]
pub(super) struct CachedChatText {
    pub(super) cells: Vec<ConversationCell>,
    pub(super) width: u16,
    pub(super) text: Text<'static>,
}

pub(super) fn collect_chat_items(app: &TuiApp) -> Vec<ConversationCell> {
    let mut cells = Vec::new();

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
        cells.push(ConversationCell::UserMessage {
            created_at: entry.created_at,
            body,
        });
    }

    for brief in &app.briefs {
        if matches!(brief.kind, crate::types::BriefKind::Ack) {
            continue;
        }
        cells.push(ConversationCell::AssistantMarkdown(AssistantMarkdownCell {
            created_at: brief.created_at,
            agent_id: match brief.kind {
                crate::types::BriefKind::Result => "Holon".to_string(),
                crate::types::BriefKind::Failure => "Holon (failed)".to_string(),
                crate::types::BriefKind::Ack => unreachable!("ack briefs are filtered above"),
            },
            markdown: render_brief_body(brief),
        }));
    }

    if let Some(projection) = app.projection.as_ref() {
        for event in projection.durable_conversation_events() {
            if !is_chat_visible_conversation_event(&event.kind) {
                continue;
            }
            cells.push(ConversationCell::SystemNotice {
                created_at: event.ts,
                speaker: conversation_event_speaker(&event.kind),
                body: conversation_event_body(event),
            });
        }
    }

    cells.sort_by(|left, right| {
        left.created_at()
            .cmp(&right.created_at())
            .then_with(|| chat_role_rank(left.role()).cmp(&chat_role_rank(right.role())))
            .then_with(|| left.sort_speaker().cmp(right.sort_speaker()))
            .then_with(|| left.sort_body().cmp(right.sort_body()))
    });
    let fallback_ts = cells.last().map(ConversationCell::created_at);
    if let Some(active_item) = active_activity_item(app, fallback_ts) {
        cells.push(active_item);
    }
    cells
}

pub(super) fn is_chat_visible_conversation_event(kind: &str) -> bool {
    matches!(kind, "operator_notification_requested" | "runtime_error")
}

impl ConversationCell {
    pub(super) fn created_at(&self) -> DateTime<chrono::Utc> {
        match self {
            Self::UserMessage { created_at, .. }
            | Self::ActiveActivity { created_at, .. }
            | Self::SystemNotice { created_at, .. } => *created_at,
            Self::AssistantMarkdown(cell) => cell.created_at,
        }
    }

    fn role(&self) -> CachedChatRole {
        match self {
            Self::UserMessage { .. } => CachedChatRole::Operator,
            Self::AssistantMarkdown(_) => CachedChatRole::Agent,
            Self::ActiveActivity { .. } | Self::SystemNotice { .. } => CachedChatRole::System,
        }
    }

    fn sort_speaker(&self) -> &str {
        match self {
            Self::UserMessage { .. } => "You",
            Self::AssistantMarkdown(cell) => &cell.agent_id,
            Self::ActiveActivity { speaker, .. } | Self::SystemNotice { speaker, .. } => speaker,
        }
    }

    fn sort_body(&self) -> &str {
        match self {
            Self::UserMessage { body, .. }
            | Self::ActiveActivity { body, .. }
            | Self::SystemNotice { body, .. } => body,
            Self::AssistantMarkdown(cell) => &cell.markdown,
        }
    }

    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            Self::UserMessage { created_at, body } => render_prefixed_markdown_lines(
                *created_at,
                body,
                CachedChatRole::Operator,
                width,
                false,
            ),
            Self::AssistantMarkdown(cell) => cell.render_lines(width),
            Self::ActiveActivity { speaker, body, .. } => {
                render_active_activity_lines(speaker, body)
            }
            Self::SystemNotice {
                created_at, body, ..
            } => render_prefixed_markdown_lines(
                *created_at,
                body,
                CachedChatRole::System,
                width,
                false,
            ),
        }
    }
}

impl AssistantMarkdownCell {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        render_prefixed_markdown_lines(
            self.created_at,
            &self.markdown,
            CachedChatRole::Agent,
            width,
            true,
        )
    }
}

#[cfg(test)]
pub(super) fn build_chat_text(items: &[ConversationCell]) -> Text<'static> {
    build_chat_text_for_width(items, u16::MAX)
}

pub(super) fn build_chat_text_for_width(items: &[ConversationCell], width: u16) -> Text<'static> {
    let mut text = Text::default();
    for (index, item) in items.iter().enumerate() {
        text.lines.extend(item.render_lines(width));
        if index + 1 < items.len() {
            text.lines.push(Line::default());
        }
    }

    text
}

#[cfg(test)]
pub(super) fn chat_text(app: &TuiApp) -> Text<'static> {
    chat_text_for_width(app, u16::MAX)
}

pub(super) fn chat_text_for_width(app: &TuiApp, width: u16) -> Text<'static> {
    let items = collect_chat_items(app);
    if items.is_empty() {
        *app.chat_text_cache.borrow_mut() = None;
        return Text::from("No chat history yet. Type a message to the selected agent.");
    }

    if let Some(cached) = app.chat_text_cache.borrow().as_ref() {
        if cached.cells == items && cached.width == width {
            return cached.text.clone();
        }
    }

    let text = build_chat_text_for_width(&items, width);
    *app.chat_text_cache.borrow_mut() = Some(CachedChatText {
        cells: items,
        width,
        text: text.clone(),
    });
    text
}

fn render_prefixed_markdown_lines(
    created_at: DateTime<chrono::Utc>,
    body: &str,
    role: CachedChatRole,
    width: u16,
    spaced_markdown: bool,
) -> Vec<Line<'static>> {
    let body = if spaced_markdown && width >= 48 {
        render_markdown_text_spaced(body)
    } else {
        render_markdown_text(body)
    };
    let body_lines = body.lines;
    let prefix = chat_prefix_spans(created_at, role);
    let continuation_indent = chat_continuation_indent(created_at);
    let mut lines = Vec::new();

    if let Some((first, rest)) = body_lines.split_first() {
        let mut spans = Vec::with_capacity(prefix.len() + first.spans.len());
        spans.extend(prefix);
        spans.extend(first.spans.clone());
        lines.push(Line::from(spans).style(first.style));

        for line in rest {
            if line.spans.iter().all(|span| span.content.is_empty()) {
                lines.push(Line::default());
                continue;
            }

            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::raw(continuation_indent.clone()));
            spans.extend(line.spans.clone());
            lines.push(Line::from(spans).style(line.style));
        }
    } else {
        lines.push(Line::from(prefix));
    }
    lines
}

fn render_active_activity_lines(speaker: &str, body: &str) -> Vec<Line<'static>> {
    let status = active_activity_status_label(speaker).unwrap_or("Working");
    let marker = active_activity_display_marker(speaker);
    let mut lines = vec![Line::from(vec![
        Span::styled(marker, Style::default().add_modifier(Modifier::DIM)),
        Span::raw(" "),
        Span::styled(status, Style::default().add_modifier(Modifier::BOLD)),
    ])];

    let body = render_markdown_text(body);
    for line in body.lines {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::raw("  "));
        spans.extend(line.spans);
        lines.push(Line::from(spans).style(line.style));
    }
    lines
}

fn chat_prefix_spans(
    created_at: DateTime<chrono::Utc>,
    role: CachedChatRole,
) -> Vec<Span<'static>> {
    let (marker, marker_style) = match role {
        CachedChatRole::Operator => ("› ", Style::default().add_modifier(Modifier::BOLD)),
        CachedChatRole::Agent => ("• ", Style::default().add_modifier(Modifier::DIM)),
        CachedChatRole::System => (
            "! ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
        ),
    };

    vec![
        Span::styled(marker, marker_style),
        Span::styled(
            chat_timestamp(created_at),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(" "),
    ]
}

fn chat_continuation_indent(created_at: DateTime<chrono::Utc>) -> String {
    let prefix_width = 2 + chat_timestamp(created_at).chars().count() + 1;
    " ".repeat(prefix_width)
}

fn chat_timestamp(created_at: DateTime<chrono::Utc>) -> String {
    created_at.with_timezone(&Local).format("%H:%M").to_string()
}

fn active_activity_status_label(speaker: &str) -> Option<&'static str> {
    if speaker.starts_with("Holon (working)") {
        Some("Working")
    } else if speaker.starts_with("Holon (queued)") {
        Some("Queued")
    } else if speaker.starts_with("Holon (starting)") {
        Some("Starting")
    } else if speaker.starts_with("Holon (waiting)") {
        Some("Waiting")
    } else if speaker.starts_with("Holon (delegating)") {
        Some("Delegating")
    } else {
        None
    }
}

fn active_activity_display_marker(speaker: &str) -> &'static str {
    match speaker.rsplit_once(' ').map(|(_, marker)| marker) {
        Some("-") => "-",
        Some("\\") => "\\",
        Some("|") => "|",
        Some("/") => "/",
        _ => "◦",
    }
}

fn chat_role_rank(role: CachedChatRole) -> u8 {
    match role {
        CachedChatRole::Operator => 0,
        CachedChatRole::Agent => 1,
        CachedChatRole::System => 2,
    }
}

fn active_activity_item(
    app: &TuiApp,
    fallback_ts: Option<DateTime<chrono::Utc>>,
) -> Option<ConversationCell> {
    let projection = app.projection.as_ref();
    let agent = projection
        .map(|projection| &projection.agent)
        .or_else(|| app.selected_agent_summary())?;
    if !agent_has_active_activity(agent) {
        return None;
    }

    let latest_action = projection.and_then(latest_action_event);
    let latest_assistant = latest_assistant_message(app, projection);
    let latest_event_ts =
        projection.and_then(|projection| projection.event_log().last().map(|event| event.ts));
    let created_at = [
        latest_event_ts,
        agent.agent.last_brief_at,
        agent
            .agent
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.completed_at),
        fallback_ts,
        app.last_event_at.map(|ts| ts.with_timezone(&chrono::Utc)),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or_else(chrono::Utc::now);

    Some(ConversationCell::ActiveActivity {
        created_at,
        speaker: active_activity_speaker(agent),
        body: active_activity_body(
            agent,
            projection.map(|projection| projection.tasks.as_slice()),
            latest_assistant.as_deref(),
            latest_action,
        ),
    })
}

fn agent_has_active_activity(agent: &AgentSummary) -> bool {
    let active_parent = matches!(
        agent.agent.status,
        crate::types::AgentStatus::Booting
            | crate::types::AgentStatus::AwakeRunning
            | crate::types::AgentStatus::AwaitingTask
    );
    let active_child = agent.active_children.iter().any(|child| {
        matches!(
            child.status,
            crate::types::AgentStatus::Booting
                | crate::types::AgentStatus::AwakeRunning
                | crate::types::AgentStatus::AwaitingTask
        ) || child.pending > 0
            || child.active_task_count > 0
    });
    active_parent
        || agent.agent.pending > 0
        || !agent.agent.active_task_ids.is_empty()
        || active_child
}

fn latest_action_event(
    projection: &crate::tui::projection::TuiProjection,
) -> Option<&crate::tui::projection::ProjectionEventRecord> {
    projection
        .event_log()
        .iter()
        .rev()
        .find(|event| is_active_action_event_kind(&event.kind))
}

fn is_active_action_event_kind(kind: &str) -> bool {
    matches!(
        kind,
        "tool_executed"
            | "tool_execution_failed"
            | "task_created"
            | "task_status_updated"
            | "task_result_received"
            | "work_item_written"
            | "waiting_intent_created"
            | "waiting_intent_cancelled"
            | "callback_delivered"
            | "operator_notification_requested"
            | "workspace_entered"
            | "workspace_exited"
            | "workspace_detached"
            | "worktree_entered"
            | "worktree_exited"
            | "worktree_auto_cleaned_up"
            | "worktree_auto_cleanup_failed"
            | "task_worktree_branch_cleanup_retained"
            | "skill_activated"
            | "system_tick_emitted"
            | "message_admitted"
            | "message_processing_started"
            | "control_applied"
            | "brief_created"
            | "turn_terminal"
            | "runtime_error"
            | "max_output_tokens_recovery"
            | "turn_local_checkpoint_recorded"
    )
}

fn latest_assistant_message(
    app: &TuiApp,
    projection: Option<&crate::tui::projection::TuiProjection>,
) -> Option<String> {
    projection
        .and_then(|projection| {
            projection
                .event_log()
                .iter()
                .rev()
                .find_map(assistant_message_from_event)
        })
        .or_else(|| {
            app.transcript
                .iter()
                .rev()
                .find_map(assistant_message_from_transcript)
        })
}

fn assistant_message_from_event(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> Option<String> {
    if event.kind != "provider_round_completed" && event.kind != "text_only_round_observed" {
        return None;
    }
    event.payload.get("text_preview").and_then(non_empty_value)
}

fn assistant_message_from_transcript(entry: &TranscriptEntry) -> Option<String> {
    if !matches!(
        entry.kind,
        TranscriptEntryKind::AssistantRound | TranscriptEntryKind::SubagentAssistantRound
    ) {
        return None;
    }
    let blocks = entry.data.get("blocks")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .filter_map(|text| non_empty(Some(text)).map(ToString::to_string))
        .collect::<Vec<_>>()
        .join(" ");
    non_empty(Some(&text)).map(ToString::to_string)
}

fn active_activity_speaker(agent: &AgentSummary) -> String {
    let state: String = match agent.agent.status {
        crate::types::AgentStatus::Booting => "Holon (starting)".into(),
        crate::types::AgentStatus::AwaitingTask => "Holon (waiting)".into(),
        crate::types::AgentStatus::AwakeRunning => "Holon (working)".into(),
        crate::types::AgentStatus::AwakeIdle if agent.agent.pending > 0 => "Holon (queued)".into(),
        _ if !agent.active_children.is_empty() => "Holon (delegating)".into(),
        _ => "Holon (working)".into(),
    };
    format!("{state} {}", active_activity_spinner())
}

fn active_activity_spinner() -> &'static str {
    match (Local::now().timestamp_millis() / 250).rem_euclid(4) {
        0 => "-",
        1 => "\\",
        2 => "|",
        _ => "/",
    }
}

fn active_activity_body(
    agent: &AgentSummary,
    tasks: Option<&[TaskRecord]>,
    latest_assistant: Option<&str>,
    latest_action: Option<&crate::tui::projection::ProjectionEventRecord>,
) -> String {
    let current = current_activity_summary(agent, tasks);
    let assistant = latest_assistant
        .map(|text| trim_activity_line(text, 120))
        .unwrap_or_else(|| "...".into());
    let action = latest_action
        .map(|event| trim_activity_line(&action_event_body(event), 120))
        .unwrap_or_else(|| "Waiting for activity".into());

    [
        format!("Current   {}", trim_activity_line(&current, 120)),
        format!("Assistant {}", assistant),
        format!("Action    {}", action),
    ]
    .join("\n")
}

fn current_activity_summary(agent: &AgentSummary, tasks: Option<&[TaskRecord]>) -> String {
    let memory = &agent.agent.working_memory.current_working_memory;

    non_empty(memory.work_summary.as_deref())
        .map(ToString::to_string)
        .or_else(|| active_task_summary(agent, tasks))
        .or_else(|| active_child_summary(agent))
        .unwrap_or_else(|| match agent.agent.status {
            crate::types::AgentStatus::Booting => "Starting runtime".into(),
            crate::types::AgentStatus::AwaitingTask => "Waiting for active task progress".into(),
            crate::types::AgentStatus::AwakeRunning => "Working on the current turn".into(),
            crate::types::AgentStatus::AwakeIdle if agent.agent.pending > 0 => {
                "Queued work is waiting to run".into()
            }
            _ => "Work is still active".into(),
        })
}

fn action_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if event.kind == "tool_executed" || event.kind == "tool_execution_failed" {
        progress_event_body(event)
    } else {
        event.summary.clone()
    }
}

fn active_task_summary(agent: &AgentSummary, tasks: Option<&[TaskRecord]>) -> Option<String> {
    let tasks = tasks?;
    agent.agent.active_task_ids.iter().find_map(|task_id| {
        tasks
            .iter()
            .find(|task| task.id == *task_id)
            .and_then(|task| task.summary.clone())
    })
}

fn active_child_summary(agent: &AgentSummary) -> Option<String> {
    agent.active_children.iter().find_map(|child| {
        child
            .observability
            .work_summary
            .clone()
            .or_else(|| child.observability.last_progress_brief.clone())
    })
}

fn trim_activity_line(input: &str, max_chars: usize) -> String {
    trim_preview(&collapse_whitespace(input), max_chars)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn non_empty_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .and_then(|text| non_empty(Some(text)))
        .map(ToString::to_string)
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
