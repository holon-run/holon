use super::*;
use crossterm::event::KeyCode;

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
        status: Option<OperatorMessageStatus>,
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
    let mut visible_operator_message_ids = std::collections::BTreeSet::new();
    let operator_message_statuses = operator_message_statuses(app);

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
        let status = if let Some(message_id) = entry.related_message_id.as_deref() {
            visible_operator_message_ids.insert(message_id.to_string());
            operator_message_statuses.get(message_id).cloned()
        } else {
            None
        };
        cells.push(ConversationCell::UserMessage {
            created_at: entry.created_at,
            body,
            status,
        });
    }

    if let Some(projection) = app.projection.as_ref() {
        for message in &projection.operator_messages {
            push_pending_operator_message_cell(
                &mut cells,
                &mut visible_operator_message_ids,
                message,
            );
        }
    }
    for message in &app.optimistic_operator_messages {
        push_pending_operator_message_cell(&mut cells, &mut visible_operator_message_ids, message);
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
        for event in projection.visible_events(app.display_mode) {
            if event.kind == "message_enqueued" || event.kind == "brief_created" {
                continue;
            }
            if is_progress_event(event) && assistant_message_from_event(event).is_some() {
                cells.push(ConversationCell::AssistantMarkdown(AssistantMarkdownCell {
                    created_at: event.ts,
                    agent_id: "Holon (progress)".to_string(),
                    markdown: conversation_event_body(event),
                }));
            } else if !matches!(
                event.presentation.category,
                crate::operator_event::OperatorEventCategory::StateSync
            ) {
                cells.push(ConversationCell::SystemNotice {
                    created_at: event.ts,
                    speaker: conversation_event_speaker(event),
                    body: conversation_event_body(event),
                });
            }
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

fn operator_message_statuses(
    app: &TuiApp,
) -> std::collections::BTreeMap<String, OperatorMessageStatus> {
    let mut statuses = std::collections::BTreeMap::new();
    for message in &app.optimistic_operator_messages {
        statuses.insert(message.message_id.clone(), message.status.clone());
    }
    if let Some(projection) = app.projection.as_ref() {
        for message in &projection.operator_messages {
            statuses.insert(message.message_id.clone(), message.status.clone());
        }
    }
    statuses
}

fn push_pending_operator_message_cell(
    cells: &mut Vec<ConversationCell>,
    visible_operator_message_ids: &mut std::collections::BTreeSet<String>,
    message: &OperatorMessageRecord,
) {
    if !visible_operator_message_ids.insert(message.message_id.clone()) {
        return;
    }
    let body = render_operator_message_body(&message.body)
        .unwrap_or_else(|| compact_json(&serde_json::to_value(&message.body).unwrap_or_default()));
    cells.push(ConversationCell::UserMessage {
        created_at: message.created_at,
        body,
        status: Some(message.status.clone()),
    });
}

pub(super) fn is_chat_visible_conversation_event(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> bool {
    event.presentation.is_conversation_candidate()
        && matches!(
            event.presentation.visibility,
            crate::operator_event::OperatorVisibility::ActionRequired
                | crate::operator_event::OperatorVisibility::TurnResult
                | crate::operator_event::OperatorVisibility::WorkDone
        )
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
            Self::UserMessage {
                created_at,
                body,
                status,
            } => render_operator_message_lines(*created_at, body, status.clone(), width),
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
            return refresh_active_activity_marker(cached.text.clone());
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

fn render_operator_message_lines(
    created_at: DateTime<chrono::Utc>,
    body: &str,
    status: Option<OperatorMessageStatus>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines =
        render_prefixed_markdown_lines(created_at, body, CachedChatRole::Operator, width, false);
    if let Some(status) = status.and_then(operator_message_status_label) {
        if let Some(first) = lines.first_mut() {
            first.spans.insert(
                3,
                Span::styled(
                    format!("[{status}] "),
                    Style::default().add_modifier(Modifier::DIM),
                ),
            );
        }
    }
    lines
}

fn operator_message_status_label(status: OperatorMessageStatus) -> Option<&'static str> {
    match status {
        OperatorMessageStatus::Sending => Some("sending"),
        OperatorMessageStatus::Queued => Some("queued"),
        OperatorMessageStatus::WaitingForSafePoint => Some("waiting"),
        OperatorMessageStatus::Processing | OperatorMessageStatus::Processed => None,
        OperatorMessageStatus::Failed => Some("failed"),
        OperatorMessageStatus::Dropped => Some("dropped"),
    }
}

fn render_active_activity_lines(speaker: &str, body: &str) -> Vec<Line<'static>> {
    let status = active_activity_status_label(speaker).unwrap_or("Working");
    let mut lines = vec![Line::from(vec![
        Span::styled(
            active_activity_spinner(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(" "),
        Span::styled(status, Style::default().add_modifier(Modifier::BOLD)),
    ])];

    if body.trim().is_empty() {
        return lines;
    }

    let body = render_markdown_text(body);
    for line in body.lines {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::raw("  "));
        spans.extend(line.spans);
        lines.push(Line::from(spans).style(line.style));
    }
    lines
}

fn refresh_active_activity_marker(mut text: Text<'static>) -> Text<'static> {
    for line in &mut text.lines {
        let is_active_activity_header = line.spans.len() >= 3
            && line.spans.get(1).is_some_and(|span| span.content == " ")
            && line.spans.get(2).is_some_and(|span| {
                matches!(
                    span.content.as_ref(),
                    "Working" | "Queued" | "Starting" | "Waiting" | "Delegating"
                )
            });
        if is_active_activity_header {
            line.spans[0] = Span::styled(
                active_activity_spinner(),
                Style::default().add_modifier(Modifier::DIM),
            );
            break;
        }
    }
    text
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

    let hidden_events = projection
        .map(|projection| projection.hidden_current_turn_events(app.display_mode))
        .unwrap_or_default();
    let latest_action = latest_action_event(hidden_events.as_slice());
    let latest_assistant = latest_assistant_message(hidden_events.as_slice());
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
    .unwrap_or_else(stable_active_activity_timestamp);

    Some(ConversationCell::ActiveActivity {
        created_at,
        speaker: active_activity_speaker(agent),
        body: active_activity_body(latest_assistant.as_deref(), latest_action),
    })
}

fn stable_active_activity_timestamp() -> DateTime<chrono::Utc> {
    DateTime::<chrono::Utc>::from(std::time::SystemTime::UNIX_EPOCH)
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
    active_parent || agent.agent.pending > 0 || agent.active_task_count > 0 || active_child
}

fn latest_action_event<'a>(
    events: &'a [&'a crate::tui::projection::ProjectionEventRecord],
) -> Option<&'a crate::tui::projection::ProjectionEventRecord> {
    events.iter().rev().copied().find(|event| {
        event.presentation.is_current_activity_candidate() && !action_event_body(event).is_empty()
    })
}

fn latest_assistant_message(
    hidden_events: &[&crate::tui::projection::ProjectionEventRecord],
) -> Option<String> {
    hidden_events
        .iter()
        .rev()
        .find_map(|event| assistant_message_from_event(event))
}

fn assistant_message_from_event(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> Option<String> {
    match event.kind.as_str() {
        "assistant_round_recorded" => event
            .presentation
            .body
            .clone()
            .or_else(|| event.payload.get("text_preview").and_then(non_empty_value)),
        "provider_round_completed" => None,
        _ if is_progress_event(event) => event.presentation.body.clone(),
        _ => None,
    }
}

fn is_progress_event(event: &crate::tui::projection::ProjectionEventRecord) -> bool {
    matches!(
        event.presentation.category,
        crate::operator_event::OperatorEventCategory::AssistantProgress
    )
}

fn active_activity_speaker(agent: &AgentSummary) -> String {
    match agent.agent.status {
        crate::types::AgentStatus::Booting => "Holon (starting)".into(),
        crate::types::AgentStatus::AwaitingTask => "Holon (waiting)".into(),
        crate::types::AgentStatus::AwakeRunning => "Holon (working)".into(),
        crate::types::AgentStatus::AwakeIdle if agent.agent.pending > 0 => "Holon (queued)".into(),
        _ if !agent.active_children.is_empty() => "Holon (delegating)".into(),
        _ => "Holon (working)".into(),
    }
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
    latest_assistant: Option<&str>,
    latest_action: Option<&crate::tui::projection::ProjectionEventRecord>,
) -> String {
    let mut lines = Vec::new();
    if let Some(assistant) = latest_assistant {
        lines.push(format!("Assistant {}", trim_activity_line(assistant, 120)));
    }
    if let Some(action) = latest_action {
        lines.push(format!(
            "Action    {}",
            trim_activity_line(&action_event_body(action), 120)
        ));
    }
    lines.join("\n")
}

fn action_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if event.kind == "tool_executed" || event.kind == "tool_execution_failed" {
        progress_event_body(event)
    } else if is_progress_event(event) {
        assistant_message_from_event(event).unwrap_or_default()
    } else {
        event.summary.clone()
    }
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

#[cfg(test)]
pub(super) fn paragraph_max_scroll(text: &Text<'_>, area: Rect) -> u16 {
    paragraph_max_scroll_for_size(
        text,
        area.width.saturating_sub(2).max(1),
        area.height.saturating_sub(2),
    )
}

pub(super) fn paragraph_max_scroll_unframed(text: &Text<'_>, area: Rect) -> u16 {
    paragraph_max_scroll_for_size(text, area.width.max(1), area.height)
}

fn paragraph_max_scroll_for_size(text: &Text<'_>, width: u16, height: u16) -> u16 {
    let inner_width = width.max(1);
    let inner_height = height as usize;
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

fn render_operator_message_body(body: &MessageBody) -> Option<String> {
    match body {
        MessageBody::Text { text } | MessageBody::Brief { text, .. } => Some(text.clone()),
        MessageBody::Json { value } => Some(compact_json(value)),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".into())
}

fn conversation_event_speaker(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    match event.presentation.category {
        crate::operator_event::OperatorEventCategory::Task
        | crate::operator_event::OperatorEventCategory::WorkItem => "System (work)".into(),
        crate::operator_event::OperatorEventCategory::Waiting => "System (waiting)".into(),
        crate::operator_event::OperatorEventCategory::Workspace => "System (workspace)".into(),
        crate::operator_event::OperatorEventCategory::Runtime => "System (runtime)".into(),
        _ => "System".into(),
    }
}

pub(super) fn conversation_event_body(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> String {
    let prefix = match event.presentation.category {
        crate::operator_event::OperatorEventCategory::Task => "[task] ",
        crate::operator_event::OperatorEventCategory::WorkItem => "[work-item] ",
        crate::operator_event::OperatorEventCategory::Waiting => "[external-trigger] ",
        crate::operator_event::OperatorEventCategory::Workspace
            if event.kind.starts_with("worktree_") =>
        {
            "[worktree] "
        }
        crate::operator_event::OperatorEventCategory::Workspace => "[workspace] ",
        crate::operator_event::OperatorEventCategory::Skill => "[skill] ",
        crate::operator_event::OperatorEventCategory::Configuration => "[agent] ",
        crate::operator_event::OperatorEventCategory::Control => "[control] ",
        crate::operator_event::OperatorEventCategory::Context => "[context] ",
        crate::operator_event::OperatorEventCategory::Delivery => "[delivery] ",
        crate::operator_event::OperatorEventCategory::Runtime if event.kind == "runtime_error" => {
            "[runtime-error] "
        }
        crate::operator_event::OperatorEventCategory::Runtime => "[turn] ",
        _ => "",
    };
    format!("{prefix}{}", event.summary)
}

fn progress_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if matches!(
        event.presentation.category,
        crate::operator_event::OperatorEventCategory::Tool
    ) {
        return event.summary.clone();
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
    use super::{
        assistant_message_from_event, is_chat_visible_conversation_event, latest_action_event,
        progress_event_body,
    };
    use crate::operator_event::{present_operator_event, OperatorPresentationContext};
    use crate::tui::projection::{ProjectionEventLane, ProjectionEventRecord};
    use chrono::Utc;
    use serde_json::{json, Value};

    fn event(kind: &str, summary: &str, payload: Value) -> ProjectionEventRecord {
        let presentation = present_operator_event(
            kind,
            &payload,
            summary,
            &OperatorPresentationContext::default(),
        );
        ProjectionEventRecord {
            id: "evt-1".into(),
            seq: 1,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: kind.into(),
            summary: presentation.summary.clone(),
            presentation,
            payload,
        }
    }

    #[test]
    fn progress_event_body_shows_full_exec_command() {
        let event = event(
            "tool_executed",
            "tool executed: ExecCommand",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "git status --short --branch"
            }),
        );
        let rendered = progress_event_body(&event);
        assert_eq!(rendered, "Command finished: git status --short --branch");
    }

    #[test]
    fn progress_event_body_marks_failed_exec_command() {
        let event = event(
            "tool_execution_failed",
            "tool execution failed: ExecCommand",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test tui"
            }),
        );

        let rendered = progress_event_body(&event);

        assert_eq!(rendered, "Command failed: cargo test tui");
    }

    #[test]
    fn progress_event_body_uses_summary_for_non_command_tools() {
        let event = event(
            "tool_executed",
            "tool executed: Sleep",
            json!({
                "tool_name": "Sleep"
            }),
        );
        let rendered = progress_event_body(&event);
        assert_eq!(rendered, "Slept");
    }

    #[test]
    fn chat_visible_conversation_events_are_user_facing_only() {
        assert!(is_chat_visible_conversation_event(&event(
            "operator_notification_requested",
            "needs input",
            json!({})
        )));
        assert!(is_chat_visible_conversation_event(&event(
            "runtime_error",
            "runtime error",
            json!({})
        )));

        assert!(!is_chat_visible_conversation_event(&event(
            "workspace_attached",
            "workspace",
            json!({})
        )));
        assert!(!is_chat_visible_conversation_event(&event(
            "provider_round_completed",
            "round",
            json!({})
        )));
    }

    #[test]
    fn empty_provider_round_is_not_activity_content() {
        let empty_round = event(
            "provider_round_completed",
            "provider round completed",
            json!({ "round": 1, "stop_reason": "end_turn" }),
        );
        let command = event(
            "process_execution_requested",
            "process_execution_requested",
            json!({
                "surface": "ExecCommand",
                "cmd_preview": "cargo test tui::chat"
            }),
        );
        let events = vec![&empty_round, &command];

        assert!(assistant_message_from_event(&empty_round).is_none());
        assert_eq!(
            latest_action_event(events.as_slice()).map(|event| event.summary.as_str()),
            Some("Command started: cargo test tui::chat")
        );
    }

    #[test]
    fn assistant_round_recorded_is_activity_content() {
        let assistant = event(
            "assistant_round_recorded",
            "assistant round",
            json!({ "text_preview": "I will inspect the event path first." }),
        );

        assert_eq!(
            assistant_message_from_event(&assistant).as_deref(),
            Some("I will inspect the event path first.")
        );
    }
}
