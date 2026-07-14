use super::*;
use crate::presentation::{PresentationItem, PresentationReducer, Renderable};
use crate::tui::projection::{
    is_presentation_reducer_event, LiveWorkingActivityRecord, ProjectionEventRecord,
};
use crossterm::event::KeyCode;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LocalCommandOutput {
    pub(super) created_at: DateTime<chrono::Utc>,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChatScrollState {
    follow_tail: bool,
    offset_from_bottom: u16,
    pending_prepend_anchor: Option<HistoryPrependAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryPrependAnchor {
    max_scroll: u16,
    scroll: u16,
}

impl ChatScrollState {
    pub(super) fn new() -> Self {
        Self {
            follow_tail: true,
            offset_from_bottom: 0,
            pending_prepend_anchor: None,
        }
    }

    pub(super) fn follow_tail(&mut self) {
        self.follow_tail = true;
        self.offset_from_bottom = 0;
        self.pending_prepend_anchor = None;
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

    pub(super) fn is_at_top(self, max_scroll: u16) -> bool {
        !self.follow_tail && self.effective_scroll(max_scroll) == 0
    }

    pub(super) fn prepare_for_history_prepend(&mut self, max_scroll: u16) {
        if self.follow_tail {
            return;
        }
        self.offset_from_bottom = self.offset_from_bottom.min(max_scroll);
        self.pending_prepend_anchor = Some(HistoryPrependAnchor {
            max_scroll,
            scroll: self.effective_scroll(max_scroll),
        });
    }

    pub(super) fn apply_history_prepend_adjustment(&mut self, max_scroll: u16) {
        let Some(anchor) = self.pending_prepend_anchor.take() else {
            return;
        };
        let added_scroll_rows = max_scroll.saturating_sub(anchor.max_scroll);
        let preserved_scroll = anchor.scroll.saturating_add(added_scroll_rows);
        self.offset_from_bottom = max_scroll.saturating_sub(preserved_scroll.min(max_scroll));
    }

    pub(super) fn preserve_across_refresh(&mut self, max_scroll: u16) {
        if self.follow_tail {
            return;
        }
        self.offset_from_bottom = self.offset_from_bottom.min(max_scroll);
        self.pending_prepend_anchor = None;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ConversationCell {
    UserMessage {
        created_at: DateTime<chrono::Utc>,
        body: String,
        status: Option<OperatorMessageStatus>,
    },
    ActiveActivity {
        created_at: DateTime<chrono::Utc>,
        speaker: String,
        body: String,
    },
    SystemNotice {
        created_at: DateTime<chrono::Utc>,
        event_seq: u64,
        speaker: String,
        body: String,
        display_kind: ConversationDisplayKind,
        group_id: Option<String>,
        header_hint: Option<String>,
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
    let mut durable_operator_message_bodies = std::collections::BTreeMap::new();
    let mut projected_cell_keys = std::collections::BTreeSet::new();
    let mut visible_operator_message_ids = std::collections::BTreeSet::new();
    let durable_operator_message_ids = app
        .projection
        .as_ref()
        .map(|projection| projection.durable_operator_message_ids())
        .unwrap_or_default();
    let selected_agent_id = app.selected_agent_id().or_else(|| {
        app.projection
            .as_ref()
            .map(|projection| projection.agent.identity.agent_id.as_str())
    });

    if let Some(projection) = app.projection.as_ref() {
        for message in projection.hydrated_operator_messages() {
            let Some(body) = render_operator_message_body(&message.body) else {
                continue;
            };
            push_projected_conversation_cell(
                &mut cells,
                &mut durable_operator_message_bodies,
                &mut projected_cell_keys,
                &format!("message:{}", message.id),
                ConversationCell::UserMessage {
                    created_at: message.created_at,
                    body,
                    status: None,
                },
            );
        }

        let level = app.display_mode.display_level();
        let agent_speaker = selected_agent_id.unwrap_or("agent");
        let events: Vec<ProjectionEventRecord> = projection
            .presentation_events(app.display_mode)
            .into_iter()
            .filter(is_presentation_reducer_event)
            .collect();

        let mut reducer = PresentationReducer::new();
        let brief_lookup = crate::presentation::BriefTextLookup(&projection.brief_text_cache);
        let mut timed_items = reducer.reduce(events.as_slice(), &brief_lookup);
        timed_items.extend(reducer.flush());
        let mut pending_agent_header_hint: Option<String> = None;

        for timed in &timed_items {
            if timed.item.is_visible_at(level) {
                if let Some(hint) = resume_header_hint(&timed.item) {
                    pending_agent_header_hint = Some(hint);
                    continue;
                }
                let display_kind = conversation_display_kind(&timed.item);
                let group_id = timed
                    .turn_index
                    .map(|turn_index| format!("agent:{agent_speaker}:turn:{turn_index}"));
                let header_hint = if group_id.is_some() {
                    pending_agent_header_hint.take()
                } else {
                    None
                };
                for rendered in timed.item.render(level) {
                    push_projected_conversation_cell(
                        &mut cells,
                        &mut durable_operator_message_bodies,
                        &mut projected_cell_keys,
                        &timed.dedupe_key,
                        rendered_to_conversation_cell(
                            &rendered,
                            timed.ts,
                            timed.event_seq,
                            agent_speaker,
                            display_kind,
                            group_id.clone(),
                            header_hint.clone(),
                        ),
                    );
                }
            }
        }
    }

    for output in &app.local_command_outputs {
        cells.push(ConversationCell::SystemNotice {
            created_at: output.created_at,
            event_seq: 0,
            speaker: if output.is_error {
                "command error".into()
            } else {
                "command".into()
            },
            body: output.body.clone(),
            display_kind: ConversationDisplayKind::Narrative,
            group_id: Some(format!(
                "local-command:{}:{}",
                output.created_at.timestamp_nanos_opt().unwrap_or_default(),
                output.title
            )),
            header_hint: Some(output.title.clone()),
        });
    }

    for message in &app.optimistic_operator_messages {
        if Some(message.agent_id.as_str()) != selected_agent_id {
            continue;
        }
        if durable_operator_message_ids.contains(&message.message_id) {
            continue;
        }
        push_pending_operator_message_cell(
            &mut cells,
            &mut visible_operator_message_ids,
            &mut durable_operator_message_bodies,
            message,
        );
    }

    cells.sort_by(|left, right| {
        left.created_at()
            .cmp(&right.created_at())
            .then_with(|| left.event_seq().cmp(&right.event_seq()))
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

fn push_pending_operator_message_cell(
    cells: &mut Vec<ConversationCell>,
    visible_operator_message_ids: &mut std::collections::BTreeSet<String>,
    durable_operator_message_bodies: &mut std::collections::BTreeMap<String, usize>,
    message: &OperatorMessageRecord,
) {
    if !visible_operator_message_ids.insert(message.message_id.clone()) {
        return;
    }
    let body = render_operator_message_body(&message.body)
        .unwrap_or_else(|| compact_json(&serde_json::to_value(&message.body).unwrap_or_default()));
    let body_key = normalized_operator_message_body_key(&body);
    if let Some(count) = durable_operator_message_bodies.get_mut(&body_key) {
        if *count > 0 {
            *count -= 1;
            return;
        }
    }
    cells.push(ConversationCell::UserMessage {
        created_at: message.created_at,
        body,
        status: Some(message.status.clone()),
    });
}

fn push_projected_conversation_cell(
    cells: &mut Vec<ConversationCell>,
    durable_operator_message_bodies: &mut std::collections::BTreeMap<String, usize>,
    projected_cell_keys: &mut std::collections::BTreeSet<String>,
    source_key: &str,
    cell: ConversationCell,
) {
    if !projected_cell_keys.insert(projected_conversation_cell_key(source_key, &cell)) {
        return;
    }
    if let ConversationCell::UserMessage { body, .. } = &cell {
        *durable_operator_message_bodies
            .entry(normalized_operator_message_body_key(body))
            .or_insert(0) += 1;
    }
    cells.push(cell);
}

fn projected_conversation_cell_key(source_key: &str, cell: &ConversationCell) -> String {
    format!(
        "{}|{:?}|{:?}|{}|{}|{}",
        source_key,
        cell.role(),
        cell.display_kind(),
        cell.sort_speaker(),
        cell.header_hint().unwrap_or(""),
        normalized_operator_message_body_key(cell.sort_body())
    )
}

fn normalized_operator_message_body_key(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl ConversationCell {
    pub(super) fn created_at(&self) -> DateTime<chrono::Utc> {
        match self {
            Self::UserMessage { created_at, .. }
            | Self::ActiveActivity { created_at, .. }
            | Self::SystemNotice { created_at, .. } => *created_at,
        }
    }

    fn event_seq(&self) -> u64 {
        match self {
            Self::SystemNotice { event_seq, .. } => *event_seq,
            Self::UserMessage { .. } | Self::ActiveActivity { .. } => 0,
        }
    }

    fn display_kind(&self) -> ConversationDisplayKind {
        match self {
            Self::UserMessage { .. } => ConversationDisplayKind::Operator,
            Self::ActiveActivity { .. } => ConversationDisplayKind::Activity,
            Self::SystemNotice { display_kind, .. } => *display_kind,
        }
    }

    fn role(&self) -> CachedChatRole {
        match self {
            Self::UserMessage { .. } => CachedChatRole::Operator,
            Self::ActiveActivity { .. } | Self::SystemNotice { .. } => CachedChatRole::System,
        }
    }

    fn sort_speaker(&self) -> &str {
        match self {
            Self::UserMessage { .. } => "operator",
            Self::ActiveActivity { speaker, .. } | Self::SystemNotice { speaker, .. } => speaker,
        }
    }

    fn sort_body(&self) -> &str {
        match self {
            Self::UserMessage { body, .. }
            | Self::ActiveActivity { body, .. }
            | Self::SystemNotice { body, .. } => body,
        }
    }

    fn header_hint(&self) -> Option<&str> {
        match self {
            Self::SystemNotice { header_hint, .. } => header_hint.as_deref(),
            Self::UserMessage { .. } | Self::ActiveActivity { .. } => None,
        }
    }

    fn groups_with_previous(&self, previous: &Self) -> bool {
        match (previous, self) {
            (
                Self::SystemNotice {
                    speaker: previous_speaker,
                    group_id: previous_group_id,
                    ..
                },
                Self::SystemNotice {
                    speaker, group_id, ..
                },
            ) => {
                previous_group_id.is_some()
                    && previous_speaker == speaker
                    && previous_group_id == group_id
            }
            _ => false,
        }
    }

    fn needs_section_break_after(&self, next: &Self) -> bool {
        next.groups_with_previous(self)
            && self.display_kind() != next.display_kind()
            && self.display_kind() != ConversationDisplayKind::Operator
            && next.display_kind() != ConversationDisplayKind::Operator
    }

    fn render_lines(&self, width: u16, include_header: bool) -> Vec<Line<'static>> {
        match self {
            Self::UserMessage {
                created_at,
                body,
                status,
            } => render_operator_message_lines(*created_at, body, status.clone(), width),
            Self::ActiveActivity { speaker, body, .. } => {
                render_active_activity_lines(speaker, body)
            }
            Self::SystemNotice {
                created_at,
                speaker,
                body,
                display_kind,
                header_hint,
                ..
            } => render_message_block_lines(
                *created_at,
                ChatBlockRole::Agent,
                speaker,
                body,
                *display_kind,
                header_hint.as_deref(),
                width,
                false,
                include_header,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CachedChatRole {
    Operator,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatBlockRole {
    Operator,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConversationDisplayKind {
    Operator,
    Narrative,
    Activity,
}

#[cfg(test)]
pub(super) fn build_chat_text(items: &[ConversationCell]) -> Text<'static> {
    build_chat_text_for_width(items, u16::MAX)
}

pub(super) fn build_chat_text_for_width(items: &[ConversationCell], width: u16) -> Text<'static> {
    let mut text = Text::default();
    for (index, item) in items.iter().enumerate() {
        let grouped_with_previous = index > 0 && item.groups_with_previous(&items[index - 1]);
        text.lines
            .extend(item.render_lines(width, !grouped_with_previous));
        if index + 1 < items.len() {
            let groups_with_next = items[index + 1].groups_with_previous(item);
            if item.needs_section_break_after(&items[index + 1]) {
                text.lines.push(Line::default());
            } else if !groups_with_next {
                text.lines.push(Line::default());
            }
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

fn render_message_block_lines(
    created_at: DateTime<chrono::Utc>,
    role: ChatBlockRole,
    speaker: &str,
    body: &str,
    display_kind: ConversationDisplayKind,
    header_hint: Option<&str>,
    width: u16,
    spaced_markdown: bool,
    include_header: bool,
) -> Vec<Line<'static>> {
    let body = if spaced_markdown && width >= 48 {
        render_markdown_text_spaced(body)
    } else {
        render_markdown_text(body)
    };
    let body_lines = body.lines;
    let mut lines = if include_header {
        vec![Line::from(chat_header_spans(
            created_at,
            role,
            speaker,
            header_hint,
        ))]
    } else {
        Vec::new()
    };

    for line in body_lines {
        if line.spans.iter().all(|span| span.content.is_empty()) {
            lines.push(Line::default());
            continue;
        }

        lines.extend(wrap_body_line(
            &line,
            message_body_indent(display_kind),
            width,
        ));
    }
    lines
}

fn wrap_body_line(line: &Line<'static>, indent: &'static str, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if width == usize::from(u16::MAX) || width == 0 {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::raw(indent));
        spans.extend(line.spans.clone());
        return vec![Line::from(spans).style(line.style)];
    }

    let indent_width = display_width(indent);
    let max_width = width.max(indent_width + 8);
    let mut lines = Vec::new();
    let mut spans = vec![Span::raw(indent)];
    let mut current_width = indent_width;

    for span in &line.spans {
        let mut chunk = String::new();
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + ch_width > max_width && current_width > indent_width {
                if !chunk.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                lines.push(Line::from(spans).style(line.style));
                spans = vec![Span::raw(indent)];
                current_width = indent_width;
            }
            chunk.push(ch);
            current_width += ch_width;
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, span.style));
        }
    }

    lines.push(Line::from(spans).style(line.style));
    lines
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn message_body_indent(display_kind: ConversationDisplayKind) -> &'static str {
    match display_kind {
        ConversationDisplayKind::Activity => "    ",
        ConversationDisplayKind::Operator | ConversationDisplayKind::Narrative => "  ",
    }
}

fn render_operator_message_lines(
    created_at: DateTime<chrono::Utc>,
    body: &str,
    status: Option<OperatorMessageStatus>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = render_message_block_lines(
        created_at,
        ChatBlockRole::Operator,
        "operator",
        body,
        ConversationDisplayKind::Operator,
        None,
        width,
        false,
        true,
    );
    if let Some(status) = status.and_then(operator_message_status_label) {
        if let Some(first) = lines.first_mut() {
            first.spans.push(Span::raw(" "));
            first.spans.push(Span::styled(
                format!("[{status}]"),
                Style::default().add_modifier(Modifier::DIM),
            ));
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

fn chat_header_spans(
    created_at: DateTime<chrono::Utc>,
    role: ChatBlockRole,
    speaker: &str,
    header_hint: Option<&str>,
) -> Vec<Span<'static>> {
    let (marker, marker_style) = match role {
        ChatBlockRole::Operator => ("> ", Style::default().add_modifier(Modifier::BOLD)),
        ChatBlockRole::Agent => (
            "\u{2022} ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
        ),
    };

    let mut spans = vec![
        Span::styled(marker, marker_style),
        Span::styled(
            speaker.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            chat_timestamp(created_at),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ];
    if let Some(hint) = header_hint.filter(|hint| !hint.trim().is_empty()) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            hint.trim().to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    spans
}

fn chat_timestamp(created_at: DateTime<chrono::Utc>) -> String {
    created_at.with_timezone(&Local).format("%H:%M").to_string()
}

fn conversation_display_kind(item: &PresentationItem) -> ConversationDisplayKind {
    match item {
        PresentationItem::UserMessage { .. } => ConversationDisplayKind::Operator,
        PresentationItem::AssistantResult { .. }
        | PresentationItem::AssistantProgress { .. }
        | PresentationItem::PlanShown { .. } => ConversationDisplayKind::Narrative,
        PresentationItem::SystemAlert { .. }
        | PresentationItem::WaitingNotice { .. }
        | PresentationItem::WorkItemCard { .. }
        | PresentationItem::ResumeNotice { .. }
        | PresentationItem::ActionGroup { .. }
        | PresentationItem::CommandExecuted { .. }
        | PresentationItem::FileRead { .. }
        | PresentationItem::FileChange { .. }
        | PresentationItem::PatchFailure { .. }
        | PresentationItem::ToolAction { .. }
        | PresentationItem::ProviderRound { .. }
        | PresentationItem::InternalTransition { .. }
        | PresentationItem::TaskLifecycle { .. }
        | PresentationItem::WorkItemBookkeeping { .. }
        | PresentationItem::WorkspaceChange { .. }
        | PresentationItem::ContinuationDetail { .. }
        | PresentationItem::GenericEvent { .. } => ConversationDisplayKind::Activity,
    }
}

fn resume_header_hint(item: &PresentationItem) -> Option<String> {
    let PresentationItem::ResumeNotice { reason, .. } = item else {
        return None;
    };
    let reason = reason.trim();
    if let Some(trigger) = reason
        .strip_prefix("Continuation triggered by ")
        .and_then(|tail| tail.split(';').next())
    {
        return Some(match trigger.trim() {
            "operator_input" => "operator input".to_string(),
            "task_result" => "task result".to_string(),
            "system_tick" => "system tick".to_string(),
            "timer" => "timer".to_string(),
            "external_trigger" => "external trigger".to_string(),
            other if !other.is_empty() => other.replace('_', " "),
            _ => "resumed".to_string(),
        });
    }
    if reason.starts_with("Timer fired") {
        return Some("timer".to_string());
    }
    if reason.starts_with("External event") {
        return Some("external trigger".to_string());
    }
    Some("resumed".to_string())
}

fn active_activity_status_label(speaker: &str) -> Option<&'static str> {
    if speaker.starts_with("Holon (working)") {
        Some("Working")
    } else if speaker.starts_with("Holon (queued)") {
        Some("Queued")
    } else if speaker.starts_with("Holon (continuing)") {
        Some("Continuing")
    } else if speaker.starts_with("Holon (starting)") {
        Some("Starting")
    } else if speaker.starts_with("Holon (waiting task)") {
        Some("Waiting task")
    } else if speaker.starts_with("Holon (waiting external)") {
        Some("Waiting external")
    } else if speaker.starts_with("Holon (waiting)") {
        Some("Waiting")
    } else if speaker.starts_with("Holon (needs input)") {
        Some("Needs input")
    } else if speaker.starts_with("Holon (blocked)") {
        Some("Blocked")
    } else if speaker.starts_with("Holon (delegating)") {
        Some("Delegating")
    } else if speaker.starts_with("Holon (sleeping)") {
        Some("Sleeping")
    } else if speaker.starts_with("Holon (idle)") {
        Some("Idle")
    } else if speaker.starts_with("Holon (paused)") {
        Some("Paused")
    } else if speaker.starts_with("Holon (stopped)") {
        Some("Stopped")
    } else {
        None
    }
}

fn chat_role_rank(role: CachedChatRole) -> u8 {
    match role {
        CachedChatRole::Operator => 0,
        CachedChatRole::System => 1,
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

    let hidden_events = if app.display_mode == crate::operator_event::OperatorDisplayMode::Info {
        projection
            .map(|projection| projection.live_working_activity_events(app.display_mode))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
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
    match agent.scheduling_posture.posture {
        crate::types::AgentSchedulingPosture::Unknown => {}
        crate::types::AgentSchedulingPosture::Archived
        | crate::types::AgentSchedulingPosture::Idle => {
            return false;
        }
        crate::types::AgentSchedulingPosture::ActiveTurn
        | crate::types::AgentSchedulingPosture::HasQueuedInput
        | crate::types::AgentSchedulingPosture::HasRunnableWork
        | crate::types::AgentSchedulingPosture::WaitingForTask
        | crate::types::AgentSchedulingPosture::WaitingForExternal
        | crate::types::AgentSchedulingPosture::WaitingForOperator
        | crate::types::AgentSchedulingPosture::Blocked => return true,
    }

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
    events: &'a [&'a LiveWorkingActivityRecord],
) -> Option<&'a LiveWorkingActivityRecord> {
    events.iter().rev().copied().find(|event| {
        event.event.presentation.is_current_activity_candidate()
            && !is_progress_event(&event.event)
            && !event.rendered_body.is_empty()
    })
}

fn latest_assistant_message(hidden_events: &[&LiveWorkingActivityRecord]) -> Option<String> {
    hidden_events
        .iter()
        .rev()
        .find_map(|event| assistant_message_from_event(&event.event))
}

fn assistant_message_from_event(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> Option<String> {
    match event.kind.as_str() {
        "assistant_round_recorded" => event.payload.get("text_preview").and_then(non_empty_value),
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
    match agent.scheduling_posture.posture {
        crate::types::AgentSchedulingPosture::Unknown => {}
        crate::types::AgentSchedulingPosture::Archived => return "Holon (stopped)".into(),
        crate::types::AgentSchedulingPosture::ActiveTurn => return "Holon (working)".into(),
        crate::types::AgentSchedulingPosture::HasQueuedInput => return "Holon (queued)".into(),
        crate::types::AgentSchedulingPosture::HasRunnableWork => {
            return "Holon (continuing)".into()
        }
        crate::types::AgentSchedulingPosture::WaitingForTask => {
            return "Holon (waiting task)".into()
        }
        crate::types::AgentSchedulingPosture::WaitingForExternal => {
            return "Holon (waiting external)".into();
        }
        crate::types::AgentSchedulingPosture::WaitingForOperator => {
            return "Holon (needs input)".into();
        }
        crate::types::AgentSchedulingPosture::Blocked => return "Holon (blocked)".into(),
        crate::types::AgentSchedulingPosture::Idle => return "Holon (idle)".into(),
    }

    match agent.agent.status {
        crate::types::AgentStatus::Booting => "Holon (starting)".into(),
        crate::types::AgentStatus::AwaitingTask => "Holon (waiting)".into(),
        crate::types::AgentStatus::AwakeRunning => "Holon (working)".into(),
        crate::types::AgentStatus::AwakeIdle if agent.agent.pending > 0 => "Holon (queued)".into(),
        _ if !agent.active_children.is_empty() => "Holon (delegating)".into(),
        crate::types::AgentStatus::AwakeIdle => "Holon (idle)".into(),
        crate::types::AgentStatus::Asleep => "Holon (sleeping)".into(),
        crate::types::AgentStatus::Stopped => "Holon (stopped)".into(),
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
    latest_action: Option<&LiveWorkingActivityRecord>,
) -> String {
    let mut lines = Vec::new();
    if let Some(text) = latest_assistant {
        lines.push(format!("Assistant {}", trim_activity_line(&text, 120)));
    }
    if let Some(action) = latest_action {
        lines.push(format!(
            "Action    {}",
            trim_activity_line(&action.rendered_body, 120)
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
fn action_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if event.kind == "tool_executed" || event.kind == "tool_execution_failed" {
        if is_sleep_tool_event(event) {
            return String::new();
        }
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

#[cfg(test)]
pub(super) fn is_operator_origin_value(value: &Value) -> bool {
    value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "operator")
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

#[cfg(test)]
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

#[cfg(test)]
fn is_sleep_tool_event(event: &crate::tui::projection::ProjectionEventRecord) -> bool {
    event.payload.get("tool_name").and_then(Value::as_str) == Some(crate::tool::names::SLEEP)
}

#[cfg(test)]
fn progress_event_body(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    if matches!(
        event.presentation.category,
        crate::operator_event::OperatorEventCategory::Tool
    ) {
        return event.summary.clone();
    }
    conversation_event_body(event)
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
        action_event_body, active_activity_speaker, agent_has_active_activity,
        assistant_message_from_event, latest_action_event, progress_event_body,
    };
    use crate::operator_event::{present_operator_event, OperatorPresentationContext};
    use crate::tui::projection::{
        LiveWorkingActivityRecord, ProjectionEventLane, ProjectionEventRecord,
    };
    use crate::types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentPostureProjection, AgentProfilePreset, AgentRegistryStatus,
        AgentSchedulingPosture, AgentState, AgentStatus, AgentSummary, AgentTokenUsageSummary,
        AgentVisibility, ClosureDecision, ClosureOutcome, RuntimePosture, TokenUsage,
    };
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
            event_seq: 1,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: kind.into(),
            summary: presentation.summary.clone(),
            presentation,
            payload,
        }
    }

    fn activity_record(event: &ProjectionEventRecord) -> LiveWorkingActivityRecord {
        LiveWorkingActivityRecord {
            event: event.clone(),
            rendered_body: action_event_body(event),
        }
    }

    fn model_ref() -> crate::config::ModelRouteRef {
        crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6").unwrap()
    }

    fn agent_summary(status: AgentStatus, posture: AgentSchedulingPosture) -> AgentSummary {
        let mut state = AgentState::new("default");
        state.status = status;
        AgentSummary {
            identity: AgentIdentityView {
                agent_id: "default".into(),
                kind: AgentKind::Default,
                visibility: AgentVisibility::Public,
                ownership: AgentOwnership::SelfOwned,
                profile_preset: AgentProfilePreset::PublicNamed,
                status: AgentRegistryStatus::Active,
                is_default_agent: true,
                parent_agent_id: None,
                lineage_parent_agent_id: None,
                delegated_from_task_id: None,
            },
            agent: state,
            scheduling_posture: AgentPostureProjection {
                posture,
                reason: "test posture".into(),
                work_item_id: None,
                task_id: None,
                run_id: None,
            },
            active_task_count: 0,
            lifecycle: AgentLifecycleHint::default(),
            model: AgentModelState {
                source: AgentModelSource::RuntimeDefault,
                runtime_default_model: model_ref(),
                effective_model: model_ref(),
                requested_model: None,
                active_model: None,
                fallback_active: false,
                effective_fallback_models: Vec::new(),
                override_model: None,
                override_reasoning_effort: None,
                resolved_policy: crate::model_catalog::ResolvedRuntimeModelPolicy::default(),
            },
            token_usage: AgentTokenUsageSummary {
                total: TokenUsage::new(0, 0),
                total_model_rounds: 0,
                last_turn: None,
            },
            closure: ClosureDecision {
                outcome: ClosureOutcome::Completed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture: RuntimePosture::Awake,
                evidence: Vec::new(),
            },
            execution: crate::system::ExecutionSnapshot {
                profile: crate::system::ExecutionProfile::default(),
                policy: crate::system::ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: None,
                workspace_anchor: Default::default(),
                execution_root: Default::default(),
                cwd: Default::default(),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
                execution_roots: Vec::new(),
            },
            active_workspace_occupancy: None,
            loaded_agents_md: Default::default(),
            skills: Default::default(),
            active_children: Vec::new(),
            active_wait_conditions: Vec::new(),
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 0,
            recent_event_count: 0,
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
    fn chat_scroll_preserves_view_after_history_prepend() {
        let mut scroll = super::ChatScrollState::new();
        scroll.scroll_with_key(crossterm::event::KeyCode::Home, 20);
        assert_eq!(scroll.effective_scroll(20), 0);

        scroll.prepare_for_history_prepend(20);
        scroll.apply_history_prepend_adjustment(35);

        assert_eq!(scroll.effective_scroll(35), 15);
    }

    #[test]
    fn chat_scroll_clamps_non_tail_refresh_without_following_tail() {
        let mut scroll = super::ChatScrollState::new();
        scroll.scroll_with_key(crossterm::event::KeyCode::PageUp, 20);
        assert!(!scroll.is_following_tail());

        scroll.preserve_across_refresh(5);

        assert!(!scroll.is_following_tail());
        assert_eq!(scroll.effective_scroll(5), 0);
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
    fn sleep_tool_is_not_activity_content() {
        let event = event(
            "tool_executed",
            "tool executed: Sleep",
            json!({
                "tool_name": "Sleep"
            }),
        );
        assert!(action_event_body(&event).is_empty());
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
        let empty_round_activity = activity_record(&empty_round);
        let command_activity = activity_record(&command);
        let events = vec![&empty_round_activity, &command_activity];

        assert!(assistant_message_from_event(&empty_round).is_none());
        assert_eq!(
            latest_action_event(events.as_slice()).map(|event| event.event.summary.as_str()),
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

    #[test]
    fn assistant_round_tool_request_is_not_activity_message() {
        let assistant = event(
            "assistant_round_recorded",
            "Assistant requested tools: ExecCommand",
            json!({
                "text_preview": null,
                "tool_names": ["ExecCommand"]
            }),
        );

        assert!(assistant_message_from_event(&assistant).is_none());
    }

    #[test]
    fn posture_runnable_overrides_asleep_activity_label() {
        let agent = agent_summary(AgentStatus::Asleep, AgentSchedulingPosture::HasRunnableWork);

        assert!(agent_has_active_activity(&agent));
        assert_eq!(active_activity_speaker(&agent), "Holon (continuing)");
    }

    #[test]
    fn posture_waiting_and_blocked_labels_are_distinct() {
        let waiting_external = agent_summary(
            AgentStatus::Asleep,
            AgentSchedulingPosture::WaitingForExternal,
        );
        let needs_input = agent_summary(
            AgentStatus::Asleep,
            AgentSchedulingPosture::WaitingForOperator,
        );
        let blocked = agent_summary(AgentStatus::Asleep, AgentSchedulingPosture::Blocked);

        assert_eq!(
            active_activity_speaker(&waiting_external),
            "Holon (waiting external)"
        );
        assert_eq!(active_activity_speaker(&needs_input), "Holon (needs input)");
        assert_eq!(active_activity_speaker(&blocked), "Holon (blocked)");
    }

    #[test]
    fn unknown_posture_keeps_legacy_activity_fallback() {
        let mut agent = agent_summary(AgentStatus::AwakeIdle, AgentSchedulingPosture::Unknown);
        agent.agent.pending = 1;

        assert!(agent_has_active_activity(&agent));
        assert_eq!(active_activity_speaker(&agent), "Holon (queued)");
    }
}

// ── Presentation item → ConversationCell conversion ───────────────────────

/// Convert a surface-neutral `RenderedCell` into a TUI-specific `ConversationCell`.
pub(super) fn rendered_to_conversation_cell(
    cell: &crate::presentation::RenderedCell,
    ts: chrono::DateTime<chrono::Utc>,
    event_seq: u64,
    agent_speaker: &str,
    display_kind: ConversationDisplayKind,
    group_id: Option<String>,
    header_hint: Option<String>,
) -> ConversationCell {
    if cell.is_live {
        ConversationCell::ActiveActivity {
            created_at: ts,
            speaker: cell.speaker.clone(),
            body: cell.body.clone(),
        }
    } else if cell.speaker == "You" {
        ConversationCell::UserMessage {
            created_at: ts,
            body: cell.body.clone(),
            status: None,
        }
    } else {
        ConversationCell::SystemNotice {
            created_at: ts,
            event_seq,
            speaker: agent_speaker.to_string(),
            body: cell.body.clone(),
            display_kind,
            group_id,
            header_hint,
        }
    }
}
