use super::overlay::draw_overlay;
#[cfg(test)]
use super::view_model::render_header_line;
use super::view_model::{render_model_detail, HeaderViewModel, StatusbarViewModel};
use super::*;
use crate::tui::input::{slash_menu_specs, SlashArgHint, SlashCommandSpec};
use crate::types::{TaskKind, TaskStatus};
use ratatui::style::Color;
use unicode_width::UnicodeWidthChar;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let area = frame.area();
    let slash_menu = slash_menu_lines(app);
    let prompt_height = prompt_pane_height(app.composer.as_str(), slash_menu.len(), area.width);
    let prompt_gap_height = prompt_top_gap_height();
    let status_height = status_bar_height();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(prompt_gap_height),
            Constraint::Length(prompt_height),
            Constraint::Length(status_height),
        ])
        .split(area);

    draw_header(frame, outer[0], app);
    draw_main_panels(frame, outer[1], app);
    draw_prompt_top_gap(frame, outer[2]);
    draw_prompt_pane(frame, outer[3], app, &slash_menu);
    draw_status_bar(frame, outer[4], app, !slash_menu.is_empty());
    draw_overlay(frame, app);
}

fn draw_main_panels(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    draw_chat(frame, area, app);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let view_model = HeaderViewModel::from_app(app);
    let paragraph = Paragraph::new(view_model.line)
        .block(Block::default().borders(Borders::BOTTOM))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_chat(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let body = chat_text_for_width(app, area.width.max(1));
    let max_scroll = paragraph_max_scroll_unframed(&body, area);
    app.chat_max_scroll = max_scroll;
    app.chat_scroll.apply_history_prepend_adjustment(max_scroll);
    let scroll = app.chat_scroll.effective_scroll(max_scroll);
    let paragraph = Paragraph::new(body)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_prompt_top_gap(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Block::default().style(prompt_pane_style()), area);
}

fn draw_prompt_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, slash_menu: &[Line<'static>]) {
    let prompt_style = prompt_pane_style();
    let prompt_scroll = prompt_pane_scroll(
        area,
        app.composer.as_str(),
        app.composer.cursor(),
        slash_menu.len() as u16,
    );
    let paragraph = Paragraph::new(render_prompt_text_for_width(
        app.composer.as_str(),
        slash_menu,
        area.width,
        prompt_scroll,
        area.height,
    ))
    .style(prompt_style)
    .scroll((0, 0));
    frame.render_widget(Block::default().style(prompt_style), area);
    frame.render_widget(paragraph, area);

    if matches!(app.overlay, OverlayState::None) {
        let (x, y) = prompt_cursor_position(
            area,
            app.composer.as_str(),
            app.composer.cursor(),
            slash_menu.len() as u16,
            prompt_scroll,
        );
        frame.set_cursor_position(ratatui::layout::Position { x, y });
    }
}

fn draw_status_bar(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, slash_visible: bool) {
    let view_model = StatusbarViewModel::from_app(app, slash_visible);
    let text = format!("{}\n{}", view_model.context_line, view_model.status_line);
    let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::TOP));
    frame.render_widget(paragraph, area);
}

pub(super) fn render_agent_state_text(app: &TuiApp) -> String {
    let Some(projection) = app.projection.as_ref() else {
        return "Projection not initialized yet.".into();
    };
    let agent = &projection.agent;

    let mut lines = vec![
        format!(
            "Agent: {} / {:?}",
            agent.identity.agent_id, agent.agent.status
        ),
        format!("Contract: {}", agent.identity.contract_badge()),
        format!(
            "Queue: pending {}  active tasks {}",
            agent.agent.pending, agent.active_task_count
        ),
        format!(
            "Closure: {:?} / {:?}",
            agent.closure.outcome, agent.closure.runtime_posture
        ),
    ];

    lines.push(String::new());
    // Model section
    let model_info = render_model_status(agent);
    // Strip the "model: " prefix from render_model_status
    let model_detail = model_info.strip_prefix("model: ").unwrap_or(&model_info);
    lines.push(format!("Model: {model_detail}"));

    lines.push(String::new());
    lines.push("Workspace".into());
    if let Some(entry) = projection.workspace.active_workspace_entry.as_ref() {
        lines.push(format!("  Id: {}", entry.workspace_id));
        lines.push(format!(
            "  Mode: {}/{}",
            workspace_projection_label(Some(entry.projection_kind)),
            workspace_access_mode_label(Some(entry.access_mode))
        ));
        lines.push(format!("  Cwd: {}", entry.cwd.display()));
    } else {
        lines.push("  <none>".into());
    }
    if let Some(worktree) = projection.workspace.worktree_session.as_ref() {
        lines.push(format!("  Worktree: {}", worktree.worktree_branch));
    }

    lines.push(String::new());
    lines.push("Work".into());
    if projection.work_items.is_empty() {
        lines.push("  No work items".into());
    } else {
        for item in projection.work_items.iter().take(3) {
            lines.push(format!(
                "  - [{:?}] {}",
                item.state,
                trim(&item.objective, 40)
            ));
        }
    }
    if let Some(work_item) = todo_summary_work_item(&projection.agent.agent, &projection.work_items)
    {
        let active = work_item
            .todo_list
            .iter()
            .find(|item| matches!(item.state, crate::types::TodoItemState::InProgress))
            .or_else(|| {
                work_item
                    .todo_list
                    .iter()
                    .find(|item| matches!(item.state, crate::types::TodoItemState::Pending))
            })
            .map(|item| trim(&item.text, 40))
            .unwrap_or_else(|| "<completed>".into());
        lines.push(format!("  Todo: {active}"));
    }
    lines.push(String::new());
    lines.push("Waiting".into());
    if projection.waiting_intents.is_empty() {
        lines.push("  No active waiting intents".into());
    } else {
        for waiting in projection.waiting_intents.iter().take(2) {
            lines.push(format!("  - {}", trim(&waiting.description, 44)));
        }
    }
    if !projection.external_triggers.is_empty() {
        lines.push(format!(
            "  External triggers: {}",
            projection
                .external_triggers
                .iter()
                .filter(|item| matches!(item.status, crate::types::ExternalTriggerStatus::Active))
                .count()
        ));
    }
    if !projection.operator_notifications.is_empty() {
        if let Some(notification) = projection.operator_notifications.last() {
            lines.push(format!(
                "  Last operator notification: {}",
                trim(&notification.summary, 44)
            ));
        }
    }
    if !projection.timers.is_empty() {
        let active_timers = projection
            .timers
            .iter()
            .filter(|timer| matches!(timer.status, crate::types::TimerStatus::Active))
            .count();
        lines.push(format!("  Timers: {} active", active_timers));
    }

    lines.push(String::new());
    lines.push("Skills".into());
    if agent.skills.discoverable_skills.is_empty() {
        lines.push("  No skills".into());
    } else {
        for skill in &agent.skills.discoverable_skills {
            lines.push(format!(
                "  - {} [{}]",
                skill.name,
                match skill.scope {
                    crate::types::SkillScope::Agent => "agent",
                    crate::types::SkillScope::User => "user",
                    crate::types::SkillScope::Workspace => "workspace",
                }
            ));
        }
    }

    lines.join("\n")
}

fn todo_summary_work_item<'a>(
    agent: &crate::types::AgentState,
    work_items: &'a [crate::types::WorkItemRecord],
) -> Option<&'a crate::types::WorkItemRecord> {
    agent
        .current_turn_work_item_id
        .as_deref()
        .or(agent.current_work_item_id.as_deref())
        .and_then(|current_id| work_items.iter().find(|item| item.id == current_id))
        .or_else(|| work_items.iter().find(|item| item.is_runnable()))
}

fn prompt_pane_height(buffer: &str, slash_menu_rows: usize, pane_width: u16) -> u16 {
    const MAX_PROMPT_PANE_HEIGHT: u16 = 12;

    let input_width = PromptPaneLayout::input_width_for_pane_width(pane_width);
    let prompt_rows =
        prompt_visual_row_count_up_to(buffer, input_width, usize::from(MAX_PROMPT_PANE_HEIGHT))
            as u32;
    let slash_menu_rows = slash_menu_rows.min(usize::from(MAX_PROMPT_PANE_HEIGHT)) as u32;
    let pane_rows = prompt_rows.max(1).saturating_add(slash_menu_rows);
    pane_rows.clamp(1, u32::from(MAX_PROMPT_PANE_HEIGHT)) as u16
}

fn status_bar_height() -> u16 {
    3
}

fn prompt_top_gap_height() -> u16 {
    1
}

fn render_prompt_buffer(buffer: &str) -> String {
    let mut rendered = String::new();
    for (index, line) in buffer.lines().enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        if index == 0 {
            rendered.push_str("> ");
        } else {
            rendered.push_str("  ");
        }
        rendered.push_str(line);
    }

    if buffer.is_empty() || buffer.ends_with('\n') {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        if rendered.is_empty() {
            rendered.push_str("> ");
        } else {
            rendered.push_str("  ");
        }
    }

    rendered
}

fn prompt_pane_style() -> Style {
    Style::default().fg(Color::Reset).bg(Color::Reset)
}

fn render_prompt_text(buffer: &str, slash_menu: &[Line<'static>]) -> Text<'static> {
    if !slash_menu.is_empty() {
        let mut lines = slash_menu.to_vec();
        lines.extend(
            render_prompt_buffer(buffer)
                .lines()
                .map(|line| Line::from(line.to_string())),
        );
        return Text::from(lines);
    }

    if !buffer.is_empty() {
        return Text::from(render_prompt_buffer(buffer));
    }

    Text::from(Line::from(vec![
        Span::raw("> "),
        Span::styled(
            "Message Holon. Enter sends. Shift+Enter inserts a new line. /help for commands.",
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]))
}

fn render_prompt_text_for_width(
    buffer: &str,
    slash_menu: &[Line<'static>],
    pane_width: u16,
    scroll: u16,
    viewport_rows: u16,
) -> Text<'static> {
    if buffer.is_empty() && slash_menu.is_empty() {
        return render_prompt_text(buffer, slash_menu);
    }

    let scroll = usize::from(scroll);
    let viewport_rows = usize::from(viewport_rows.max(1));
    let slash_skip = scroll.min(slash_menu.len());
    let mut lines = slash_menu
        .iter()
        .skip(slash_skip)
        .take(viewport_rows)
        .cloned()
        .collect::<Vec<_>>();
    let prompt_skip = scroll.saturating_sub(slash_menu.len());
    let remaining_rows = viewport_rows.saturating_sub(lines.len());
    lines.extend(
        prompt_visual_rows_range(
            buffer,
            PromptPaneLayout::input_width_for_pane_width(pane_width),
            prompt_skip,
            remaining_rows,
        )
        .into_iter()
        .map(Line::from),
    );
    Text::from(lines)
}

pub(super) fn slash_menu_lines(app: &TuiApp) -> Vec<Line<'static>> {
    if app.overlay != OverlayState::None {
        return Vec::new();
    }
    if app
        .slash_menu_dismissed_for
        .as_deref()
        .is_some_and(|dismissed| dismissed == app.composer.as_str())
    {
        return Vec::new();
    }

    let buffer = app.composer.as_str();
    if buffer.contains('\n')
        || !buffer.trim_start().starts_with('/')
        || buffer.trim_start().starts_with("//")
    {
        return Vec::new();
    }
    let specs = slash_menu_specs(buffer);
    if specs.is_empty() {
        return Vec::new();
    }
    if let Some(lines) = slash_argument_hint_lines(app, buffer, &specs) {
        return lines;
    }

    specs
        .iter()
        .take(8)
        .enumerate()
        .map(|(index, spec)| {
            let selected = index == app.slash_menu_selected.min(specs.len().saturating_sub(1));
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let prefix = if selected { "> " } else { "  " };
            Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(format!("{:<14}", spec.name), style),
                Span::styled(spec.description, style.add_modifier(Modifier::DIM)),
            ])
        })
        .collect()
}

fn slash_argument_hint_lines(
    app: &TuiApp,
    buffer: &str,
    specs: &[SlashCommandSpec],
) -> Option<Vec<Line<'static>>> {
    let trimmed = buffer.trim_start();
    let token = trimmed.split_whitespace().next().unwrap_or("/");
    let spec = specs.iter().copied().find(|spec| spec.name == token)?;
    let tail = trimmed.get(token.len()..).unwrap_or("");
    if !tail.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }

    let mut lines = vec![Line::from(vec![
        Span::styled("> ", Style::default().add_modifier(Modifier::REVERSED)),
        Span::styled(
            format!("{:<14}", spec.name),
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::styled(
            spec.usage,
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::DIM),
        ),
    ])];

    match spec.arg_hint {
        SlashArgHint::None => {}
        SlashArgHint::Values(values) => {
            lines.push(Line::from(vec![
                Span::styled("  args: ", Style::default().add_modifier(Modifier::DIM)),
                Span::raw(values.join("  ")),
            ]));
        }
        SlashArgHint::Agent => {
            lines.push(Line::from(vec![
                Span::styled("  args: ", Style::default().add_modifier(Modifier::DIM)),
                Span::raw(
                    "switch <agent-id>  pause [agent-id]  resume [agent-id]  stop [agent-id]",
                ),
            ]));
            let agents = app
                .agents
                .iter()
                .take(5)
                .map(|agent| agent.identity.agent_id.as_str())
                .collect::<Vec<_>>();
            if !agents.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  agents: ", Style::default().add_modifier(Modifier::DIM)),
                    Span::raw(agents.join("  ")),
                ]));
            }
        }
        SlashArgHint::SkillName => {
            lines.push(Line::from(vec![
                Span::styled("  args: ", Style::default().add_modifier(Modifier::DIM)),
                Span::raw("<name>"),
            ]));
        }
    }

    Some(lines)
}

fn prompt_pane_scroll(area: Rect, buffer: &str, cursor: usize, hint_rows: u16) -> u16 {
    let layout = PromptPaneLayout::new(area);
    let (_, prompt_cursor_row) = prompt_cursor_visual_position(buffer, cursor, layout.input_width);
    let cursor_row = hint_rows.saturating_add(prompt_cursor_row);
    cursor_row.saturating_sub(layout.viewport_rows.saturating_sub(1))
}

fn prompt_cursor_position(
    area: Rect,
    buffer: &str,
    cursor: usize,
    hint_rows: u16,
    scroll: u16,
) -> (u16, u16) {
    let layout = PromptPaneLayout::new(area);
    let (column, prompt_cursor_row) =
        prompt_cursor_visual_position(buffer, cursor, layout.input_width);
    let cursor_row = hint_rows.saturating_add(prompt_cursor_row);
    (
        layout
            .cursor_origin_x
            .saturating_add(column)
            .min(layout.max_cursor_x),
        layout
            .cursor_origin_y
            .saturating_add(cursor_row.saturating_sub(scroll))
            .min(layout.max_cursor_y),
    )
}

fn prompt_cursor_visual_position(buffer: &str, cursor: usize, input_width: u16) -> (u16, u16) {
    prompt_visual_position_at_end(&buffer[..cursor], input_width)
}

#[derive(Debug, Clone, Copy)]
struct PromptPaneLayout {
    input_width: u16,
    viewport_rows: u16,
    cursor_origin_x: u16,
    cursor_origin_y: u16,
    max_cursor_x: u16,
    max_cursor_y: u16,
}

impl PromptPaneLayout {
    fn new(area: Rect) -> Self {
        Self {
            input_width: Self::input_width_for_pane_width(area.width),
            viewport_rows: area.height.max(1),
            cursor_origin_x: area.x,
            cursor_origin_y: area.y,
            max_cursor_x: area.x.saturating_add(area.width.saturating_sub(1)),
            max_cursor_y: area.y.saturating_add(area.height.saturating_sub(1)),
        }
    }

    fn input_width_for_pane_width(pane_width: u16) -> u16 {
        pane_width.max(1)
    }
}

#[cfg(test)]
fn prompt_visual_rows(buffer: &str, visible_line_width: u16) -> Vec<String> {
    prompt_visual_rows_range(buffer, visible_line_width, 0, usize::MAX)
}

fn prompt_visual_rows_range(
    buffer: &str,
    visible_line_width: u16,
    skip: usize,
    take: usize,
) -> Vec<String> {
    if take == 0 {
        return Vec::new();
    }

    let visible_line_width = usize::from(visible_line_width.max(1));
    let end = skip.saturating_add(take);
    let mut rows = Vec::new();
    let mut row_index = 0usize;

    for (line_index, line) in buffer.split('\n').enumerate() {
        let prefix = if line_index == 0 { "> " } else { "  " };
        if !push_wrapped_prompt_line(
            prefix,
            line,
            visible_line_width,
            &mut row_index,
            skip,
            end,
            &mut rows,
        ) {
            break;
        }
    }

    rows
}

fn push_wrapped_prompt_line(
    prefix: &str,
    line: &str,
    visible_line_width: usize,
    row_index: &mut usize,
    skip: usize,
    end: usize,
    rows: &mut Vec<String>,
) -> bool {
    let mut row = String::new();
    let mut row_width = 0usize;

    for ch in prefix.chars().chain(line.chars()) {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if row_width > 0 && row_width.saturating_add(ch_width) > visible_line_width {
            if !push_prompt_row(std::mem::take(&mut row), row_index, skip, end, rows) {
                return false;
            }
            row_width = 0;
        }
        row.push(ch);
        row_width = row_width.saturating_add(ch_width);
    }

    push_prompt_row(row, row_index, skip, end, rows)
}

fn push_prompt_row(
    row: String,
    row_index: &mut usize,
    skip: usize,
    end: usize,
    rows: &mut Vec<String>,
) -> bool {
    if *row_index >= skip && *row_index < end {
        rows.push(row);
    }
    *row_index = row_index.saturating_add(1);
    *row_index < end
}

fn prompt_visual_row_count_up_to(buffer: &str, visible_line_width: u16, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }

    let visible_line_width = usize::from(visible_line_width.max(1));
    let mut rows = 0usize;
    for (line_index, line) in buffer.split('\n').enumerate() {
        let prefix = if line_index == 0 { "> " } else { "  " };
        rows = rows.saturating_add(wrapped_prompt_line_count_up_to(
            prefix,
            line,
            visible_line_width,
            limit.saturating_sub(rows),
        ));
        if rows >= limit {
            break;
        }
    }
    rows
}

fn wrapped_prompt_line_count_up_to(
    prefix: &str,
    line: &str,
    visible_line_width: usize,
    limit: usize,
) -> usize {
    if limit == 0 {
        return 0;
    }

    let mut rows = 1usize;
    let mut row_width = 0usize;
    for ch in prefix.chars().chain(line.chars()) {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if row_width > 0 && row_width.saturating_add(ch_width) > visible_line_width {
            if rows >= limit {
                return rows;
            }
            rows = rows.saturating_add(1);
            row_width = 0;
        }
        row_width = row_width.saturating_add(ch_width);
    }
    rows
}

fn prompt_visual_position_at_end(buffer: &str, visible_line_width: u16) -> (u16, u16) {
    let visible_line_width = usize::from(visible_line_width.max(1));
    let mut row = 0usize;
    let mut column = 0usize;

    for (line_index, line) in buffer.split('\n').enumerate() {
        if line_index > 0 {
            row = row.saturating_add(1);
            column = 0;
        }
        let prefix = if line_index == 0 { "> " } else { "  " };
        for ch in prefix.chars().chain(line.chars()) {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if column > 0 && column.saturating_add(ch_width) > visible_line_width {
                row = row.saturating_add(1);
                column = 0;
            }
            column = column.saturating_add(ch_width);
        }
    }

    (
        column.min(visible_line_width.saturating_sub(1)) as u16,
        row.min(usize::from(u16::MAX)) as u16,
    )
}

#[cfg(test)]
pub(super) fn render_header(agent: &AgentSummary) -> String {
    render_header_line(agent)
}

pub(super) fn render_projection_event_summary(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> String {
    let title_prefix = format!("{}:", event.presentation.title);
    let description =
        if event.presentation.title == event.summary || event.summary.starts_with(&title_prefix) {
            event.summary.clone()
        } else {
            format!("{}: {}", event.presentation.title, event.summary)
        };
    format!(
        "{} [{:?}] {}",
        event.ts.with_timezone(&Local).format("%H:%M:%S"),
        event.lane,
        trim(&description, 120)
    )
}

pub(super) fn render_transcript_entry(entry: &TranscriptEntry) -> String {
    format!(
        "{} {:?} {}",
        entry.created_at.with_timezone(&Local).format("%H:%M:%S"),
        entry.kind,
        trim(&compact_json(&entry.data), 220)
    )
}

pub(super) fn render_task(task: &TaskRecord) -> String {
    let summary = task
        .summary
        .clone()
        .unwrap_or_else(|| task.kind.as_str().to_string());
    let child = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("child_agent_id"))
        .and_then(Value::as_str)
        .map(|value| format!(" child={value}"))
        .unwrap_or_default();
    format!(
        "{} [{:?}] {} ({}){}",
        task.updated_at.with_timezone(&Local).format("%H:%M:%S"),
        task.status,
        trim(&summary, 72),
        task.kind,
        child
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskOverlayAction {
    FullOutput,
    FollowOutput,
    Stop,
    Input,
}

impl TaskOverlayAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::FullOutput => "full output",
            Self::FollowOutput => "follow/live output",
            Self::Stop => "stop/cancel",
            Self::Input => "input/continue",
        }
    }

    pub(super) fn tool_name(self) -> &'static str {
        match self {
            Self::FullOutput | Self::FollowOutput => "TaskOutput",
            Self::Stop => "TaskStop",
            Self::Input => "TaskInput",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TaskActionAvailability {
    pub(super) action: TaskOverlayAction,
    pub(super) enabled: bool,
    pub(super) reason: &'static str,
}

impl TaskActionAvailability {
    fn available(action: TaskOverlayAction) -> Self {
        Self {
            action,
            enabled: true,
            reason: "available",
        }
    }

    fn unavailable(action: TaskOverlayAction, reason: &'static str) -> Self {
        Self {
            action,
            enabled: false,
            reason,
        }
    }

    fn render(self) -> String {
        if self.enabled {
            format!(
                "[{}] {}: available ({})",
                self.key(),
                self.action.label(),
                self.action.tool_name()
            )
        } else {
            format!(
                "[{}] {}: unavailable — {}",
                self.key(),
                self.action.label(),
                self.reason
            )
        }
    }

    fn key(self) -> char {
        match self.action {
            TaskOverlayAction::FullOutput => 'f',
            TaskOverlayAction::FollowOutput => 'l',
            TaskOverlayAction::Stop => 'x',
            TaskOverlayAction::Input => 'i',
        }
    }
}

pub(super) fn task_action_availability(
    task: &TaskRecord,
    action: TaskOverlayAction,
) -> TaskActionAvailability {
    let has_output_path = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("output_path"))
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty());
    let has_input_target = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("input_target"))
        .and_then(Value::as_str)
        .is_some_and(|target| !target.is_empty());
    let accepts_input = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("accepts_input"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || has_input_target;
    let is_active_or_cancelling = matches!(
        task.status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    );
    match action {
        TaskOverlayAction::FullOutput if task.kind != TaskKind::CommandTask => {
            TaskActionAvailability::unavailable(action, "only command tasks expose full output")
        }
        TaskOverlayAction::FullOutput if !has_output_path => {
            TaskActionAvailability::unavailable(action, "no full output artifact is available yet")
        }
        TaskOverlayAction::FullOutput => TaskActionAvailability::available(action),
        TaskOverlayAction::FollowOutput
            if task.kind != TaskKind::CommandTask || task.status != TaskStatus::Running =>
        {
            TaskActionAvailability::unavailable(
                action,
                "only running command tasks can be followed",
            )
        }
        TaskOverlayAction::FollowOutput if !has_output_path => {
            TaskActionAvailability::unavailable(action, "no live output artifact is available yet")
        }
        TaskOverlayAction::FollowOutput => TaskActionAvailability::available(action),
        TaskOverlayAction::Stop if !is_active_or_cancelling => TaskActionAvailability::unavailable(
            action,
            "task is not queued, running, or cancelling",
        ),
        TaskOverlayAction::Stop => TaskActionAvailability::available(action),
        TaskOverlayAction::Input if !accepts_input || task.status != TaskStatus::Running => {
            TaskActionAvailability::unavailable(action, "task is not currently accepting input")
        }
        TaskOverlayAction::Input => TaskActionAvailability::available(action),
    }
}

pub(super) fn render_task_detail(task: &TaskRecord) -> String {
    let mut lines = vec![
        format!(
            "Summary: {}",
            task.summary.as_deref().unwrap_or(task.kind.as_str())
        ),
        format!("Kind: {}", task.kind),
        format!("Status: {:?}", task.status),
        format!(
            "Updated: {}",
            task.updated_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
        ),
    ];
    let actions = [
        TaskOverlayAction::FullOutput,
        TaskOverlayAction::FollowOutput,
        TaskOverlayAction::Stop,
        TaskOverlayAction::Input,
    ];
    lines.push(String::new());
    lines.push("Actions:".into());
    for action in actions {
        lines.push(format!(
            "  {}",
            task_action_availability(task, action).render()
        ));
    }

    if let Some(detail) = &task.detail {
        lines.push(String::new());
        lines.push("Detail:".into());
        lines.push(serde_json::to_string_pretty(detail).unwrap_or_else(|_| compact_json(detail)));
    }

    lines.join("\n")
}

pub(super) fn render_model_status(agent: &AgentSummary) -> String {
    format!("model: {}", render_model_detail(agent))
}

pub(super) fn render_summary(agent: &AgentSummary) -> String {
    let mut lines = vec![
        format!("Agent: {}", agent.identity.agent_id),
        format!("Kind: {:?}", agent.identity.kind),
        format!("Identity contract: {}", agent.identity.contract_badge()),
        format!("Contract summary: {}", agent.identity.contract_summary()),
        format!(
            "Spawn surface: {}",
            agent.identity.profile_preset.spawn_surface_summary()
        ),
        format!(
            "Cleanup ownership: {}",
            agent.identity.ownership.cleanup_summary()
        ),
        format!("Status: {:?}", agent.agent.status),
        format!("Resume required: {}", agent.lifecycle.resume_required),
        format!(
            "Model: {} ({:?})",
            agent.model.effective_model.as_string(),
            agent.model.source
        ),
        format!("Pending queue: {}", agent.agent.pending),
        format!("Active tasks: {}", agent.active_task_count),
        format!(
            "Closure: {:?} / posture {:?}",
            agent.closure.outcome, agent.closure.runtime_posture
        ),
    ];
    if let Some(model_override) = agent.model.override_model.as_ref() {
        lines.push(format!("Override: {}", model_override.as_string()));
        if let Some(effort) = agent.model.override_reasoning_effort.as_deref() {
            lines.push(format!("Override effort: {}", effort));
        }
    }
    if let Some(hint) = agent.lifecycle.operator_hint.as_deref() {
        lines.push(format!("Lifecycle hint: {}", hint));
    }
    if let Some(resume_cli_hint) = agent.lifecycle.resume_cli_hint.as_deref() {
        lines.push(format!("Resume: {}", resume_cli_hint));
    }
    lines.push(format!(
        "Runtime default: {}",
        agent.model.runtime_default_model.as_string()
    ));
    if !agent.model.effective_fallback_models.is_empty() {
        lines.push(format!(
            "Fallbacks: {}",
            agent
                .model
                .effective_fallback_models
                .iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if let Some(reason) = agent.closure.waiting_reason {
        lines.push(format!("Waiting reason: {:?}", reason));
    }
    if let Some(entry) = agent.agent.active_workspace_entry.as_ref() {
        lines.push(String::new());
        lines.push(format!("Workspace: {}", entry.workspace_id));
        lines.push(format!("Anchor: {}", entry.workspace_anchor.display()));
        lines.push(format!("Root: {}", entry.execution_root.display()));
        lines.push(format!(
            "Projection: {}",
            workspace_projection_label(Some(entry.projection_kind))
        ));
        lines.push(format!(
            "Access mode: {}",
            workspace_access_mode_label(Some(entry.access_mode))
        ));
        lines.push(format!("Cwd: {}", entry.cwd.display()));
    } else if !agent.agent.attached_workspaces.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Attached workspaces: {}",
            agent.agent.attached_workspaces.join(", ")
        ));
    }

    lines.push(String::new());
    lines.push(format!(
        "Children: {}",
        if agent.active_children.is_empty() {
            "none".into()
        } else {
            agent
                .active_children
                .iter()
                .map(|child| {
                    format!(
                        "{}:{:?}[{}]",
                        child.identity.agent_id,
                        child.status,
                        child.identity.contract_badge()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    lines.push(format!(
        "Waiting intents: {}",
        agent.active_waiting_intents.len()
    ));
    lines.push(format!(
        "External triggers: {}",
        agent.active_external_triggers.len()
    ));
    lines.push(format!(
        "Operator notifications: {}",
        agent.recent_operator_notifications.len()
    ));
    lines.push(String::new());
    lines.push(format!(
        "Execution backend: {}",
        crate::system::execution_backend_label(agent.execution.policy.backend)
    ));
    lines.push(format!(
        "Process execution: {}",
        crate::system::execution_guarantee_label(
            agent.execution.policy.resource_authority.process_execution
        )
    ));
    lines.push(crate::system::execution_confinement_summary_line(
        &agent.execution,
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        prompt_cursor_position, prompt_pane_height, prompt_pane_scroll, prompt_top_gap_height,
        prompt_visual_rows, render_header, render_model_status, render_prompt_buffer,
        render_prompt_text, render_prompt_text_for_width, render_summary, render_task_detail,
        status_bar_height, todo_summary_work_item,
    };
    use crate::system::{ExecutionProfile, ExecutionSnapshot};
    use crate::types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
        AgentTokenUsageSummary, AgentVisibility, ChildAgentObservabilitySnapshot, ChildAgentPhase,
        ChildAgentSummary, ClosureDecision, ClosureOutcome, LoadedAgentsMdView, RuntimePosture,
        SkillsRuntimeView, TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TokenUsage,
        WorkItemRecord, WorkItemState,
    };
    use chrono::{TimeZone, Utc};
    use ratatui::prelude::{Line, Rect};
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn sample_agent_summary() -> AgentSummary {
        let mut state = AgentState::new("default");
        state.status = crate::types::AgentStatus::AwakeIdle;
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
            active_task_count: 0,
            lifecycle: AgentLifecycleHint::default(),
            model: AgentModelState {
                effective_model: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6")
                    .unwrap(),
                requested_model: Some(
                    crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                ),
                active_model: Some(
                    crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                ),
                fallback_active: false,
                runtime_default_model: crate::config::ModelRef::parse(
                    "anthropic/claude-sonnet-4-6",
                )
                .unwrap(),
                override_model: None,
                override_reasoning_effort: None,
                source: AgentModelSource::RuntimeDefault,
                effective_fallback_models: Vec::new(),
                resolved_policy: crate::model_catalog::ResolvedRuntimeModelPolicy {
                    model_ref: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6")
                        .unwrap(),
                    display_name: "Claude Sonnet 4.6".into(),
                    description: "Sample policy".into(),
                    context_window_tokens: Some(200_000),
                    effective_context_window_percent: 90,
                    prompt_budget_estimated_tokens: 180_000,
                    compaction_trigger_estimated_tokens: 180_000,
                    compaction_keep_recent_estimated_tokens: 68_400,
                    runtime_max_output_tokens: 32_000,
                    tool_output_truncation_estimated_tokens: 2_500,
                    max_output_tokens_upper_limit: Some(128_000),
                    capabilities: crate::model_catalog::ModelCapabilityFlags {
                        image_input: true,
                        ..crate::model_catalog::ModelCapabilityFlags::default()
                    },
                    source: crate::model_catalog::ModelMetadataSource::BuiltInCatalog,
                },
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
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: None,
                workspace_anchor: PathBuf::from("/tmp"),
                execution_root: PathBuf::from("/tmp"),
                cwd: PathBuf::from("/tmp"),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            active_workspace_occupancy: None,
            loaded_agents_md: LoadedAgentsMdView::default(),
            skills: SkillsRuntimeView::default(),
            active_children: vec![ChildAgentSummary {
                identity: AgentIdentityView {
                    agent_id: "child_1".into(),
                    kind: AgentKind::Child,
                    visibility: AgentVisibility::Private,
                    ownership: AgentOwnership::ParentSupervised,
                    profile_preset: AgentProfilePreset::PrivateChild,
                    status: AgentRegistryStatus::Active,
                    is_default_agent: false,
                    parent_agent_id: Some("default".into()),
                    lineage_parent_agent_id: Some("default".into()),
                    delegated_from_task_id: Some("task-1".into()),
                },
                status: crate::types::AgentStatus::AwakeRunning,
                current_run_id: Some("run-1".into()),
                pending: 0,
                active_task_count: 1,
                observability: ChildAgentObservabilitySnapshot {
                    phase: ChildAgentPhase::Running,
                    blocked_reason: None,
                    waiting_reason: None,
                    current_work_item_id: Some("work-1".into()),
                    work_summary: Some("child running".into()),
                    last_progress_brief: None,
                    last_result_brief: None,
                },
            }],
            active_waiting_intents: Vec::new(),
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 0,
            recent_event_count: 0,
        }
    }

    #[test]
    fn prompt_buffer_renders_as_multiline_block() {
        let rendered = render_prompt_buffer("first\nsecond");
        assert_eq!(rendered, "> first\n  second");
    }

    #[test]
    fn prompt_buffer_keeps_empty_trailing_line_visible() {
        let rendered = render_prompt_buffer("first\n");
        assert_eq!(rendered, "> first\n  ");
    }

    #[test]
    fn prompt_pane_height_accounts_for_soft_wrapped_lines() {
        assert_eq!(prompt_pane_height("abcdefgh", 0, 10), 1);
        assert_eq!(prompt_pane_height("abcdefghabcdefgh", 0, 10), 2);
        assert_eq!(prompt_pane_height("abcdefgh", 2, 10), 3);
    }

    #[test]
    fn prompt_visual_rows_preserve_blank_lines_and_soft_wraps() {
        assert_eq!(
            prompt_visual_rows("first\n\nsecond", 20),
            vec!["> first", "  ", "  second"]
        );
        assert_eq!(
            prompt_visual_rows("abcdefghabcdefgh", 10),
            vec!["> abcdefgh", "abcdefgh"]
        );
    }

    #[test]
    fn prompt_pane_height_saturates_for_large_pastes() {
        let buffer = "abcdefghij\n".repeat(70_000);
        assert_eq!(prompt_pane_height(&buffer, usize::MAX, 4), 12);
        assert_eq!(prompt_pane_height(&"a".repeat(70_000), 0, 4), 12);
    }

    #[test]
    fn empty_prompt_renders_dim_placeholder() {
        let rendered = render_prompt_text("", &[]);
        let line = rendered.lines.first().expect("placeholder line");
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("Message Holon."));
    }

    #[test]
    fn render_header_uses_agent_status_without_identity_contract() {
        let rendered = render_header(&sample_agent_summary());
        assert_eq!(rendered, "default  idle");
        assert!(!rendered.contains("public/self_owned (public_named)"));
    }

    #[test]
    fn render_summary_uses_profile_and_ownership_semantics() {
        let rendered = render_summary(&sample_agent_summary());
        assert!(rendered.contains("Identity contract: public/self_owned (public_named)"));
        assert!(rendered.contains("public self-owned agent addressed directly by `agent_id`"));
        assert!(rendered.contains("SpawnAgent returns `agent_id` only"));
        assert!(
            rendered.contains("child_1:AwakeRunning[private/parent_supervised (private_child)]")
        );
        assert!(rendered.contains("Execution backend: host_local"));
        assert!(rendered.contains("Process execution: runtime_shaped"));
        assert!(rendered.contains(
            "Confinement: path_confinement=not_enforced, write_confinement=not_enforced, network_confinement=not_enforced, secret_isolation=not_enforced, child_process_containment=not_enforced"
        ));
    }

    #[test]
    fn model_status_distinguishes_inherited_override_and_fallback() {
        let inherited = sample_agent_summary();
        assert_eq!(
            render_model_status(&inherited),
            "model: anthropic/claude-sonnet-4-6"
        );

        let mut overridden = sample_agent_summary();
        overridden.model.override_model =
            Some(crate::config::ModelRef::parse("openai/gpt-5.4").unwrap());
        overridden.model.effective_model =
            crate::config::ModelRef::parse("openai/gpt-5.4").unwrap();
        overridden.model.active_model =
            Some(crate::config::ModelRef::parse("openai/gpt-5.4").unwrap());
        assert_eq!(
            render_model_status(&overridden),
            "model: openai/gpt-5.4 (agent override)"
        );

        let mut fallback = overridden;
        fallback.model.requested_model =
            Some(crate::config::ModelRef::parse("openai/gpt-5.4").unwrap());
        fallback.model.active_model =
            Some(crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap());
        fallback.model.fallback_active = true;
        assert_eq!(
            render_model_status(&fallback),
            "model: anthropic/claude-sonnet-4-6 (fallback from openai/gpt-5.4)"
        );
    }

    #[test]
    fn todo_summary_prefers_current_work_item_over_first_open_item() {
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some("work-current".into());
        let mut first_open = WorkItemRecord::new("default", "first open", WorkItemState::Open);
        first_open.id = "work-first".into();
        first_open.todo_list = vec![TodoItem {
            text: "wrong todo".into(),
            state: TodoItemState::InProgress,
        }];
        let mut current = WorkItemRecord::new("default", "selected work", WorkItemState::Open);
        current.id = "work-current".into();
        current.blocked_by = Some("waiting on review".into());
        current.todo_list = vec![TodoItem {
            text: "selected todo".into(),
            state: TodoItemState::Pending,
        }];

        let work_items = [first_open, current];
        let selected = todo_summary_work_item(&agent, &work_items)
            .expect("current work item should be selected");
        assert_eq!(selected.id, "work-current");
        assert_eq!(selected.todo_list[0].text, "selected todo");
    }

    #[test]
    fn prompt_cursor_tracks_multiline_end_position() {
        let area = Rect::new(10, 5, 40, 6);
        assert_eq!(
            prompt_cursor_position(area, "first\nsecond", "first\nsecond".len(), 0, 0),
            (18, 6)
        );
    }

    #[test]
    fn prompt_cursor_tracks_soft_wrapped_long_lines() {
        let area = Rect::new(10, 5, 10, 6);
        assert_eq!(
            prompt_cursor_position(area, "abcdefgh", "abcdefgh".len(), 0, 0),
            (19, 5)
        );
    }

    #[test]
    fn prompt_cursor_uses_display_width_for_wide_characters() {
        let area = Rect::new(10, 5, 10, 6);
        assert_eq!(
            prompt_cursor_position(area, "你好你好", "你好你好".len(), 0, 0),
            (19, 5)
        );
    }

    #[test]
    fn prompt_cursor_clamps_to_prompt_pane_height() {
        let area = Rect::new(10, 5, 10, 4);
        assert_eq!(
            prompt_cursor_position(area, "abcdefgh\nabcdefgh", "abcdefgh\nabcdefgh".len(), 0, 0),
            (19, 6)
        );
    }

    #[test]
    fn prompt_cursor_tracks_insert_position_inside_line() {
        let area = Rect::new(10, 5, 20, 6);
        assert_eq!(prompt_cursor_position(area, "hello", 2, 0, 0), (14, 5));
    }

    #[test]
    fn prompt_pane_scroll_keeps_multiline_cursor_visible() {
        let area = Rect::new(10, 5, 20, 3);
        let buffer = "first\n\nsecond\nthird\nfourth";
        let scroll = prompt_pane_scroll(area, buffer, buffer.len(), 0);
        assert!(scroll > 0);
        let (_, y) = prompt_cursor_position(area, buffer, buffer.len(), 0, scroll);
        assert!(y <= area.y + area.height.saturating_sub(1));
    }

    #[test]
    fn prompt_pane_scroll_keeps_single_line_visible_in_short_pane() {
        let area = Rect::new(10, 5, 20, 3);
        assert_eq!(prompt_pane_scroll(area, "hello", "hello".len(), 0), 0);
    }

    #[test]
    fn slash_menu_renders_above_prompt_buffer() {
        let rendered = render_prompt_text(
            "/de",
            &[
                Line::from("  /debug-prompt open debug prompt dialog"),
                Line::from("  /help         show slash command help"),
            ],
        );
        let joined = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("/debug-prompt open debug prompt dialog"));
        assert!(joined.contains("> /de"));
    }

    #[test]
    fn slash_menu_prompt_buffer_uses_real_text_lines() {
        let rendered = render_prompt_text(
            "/debug\nsecond",
            &[Line::from("  /debug-prompt open debug prompt dialog")],
        );
        let joined = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            joined,
            vec![
                "  /debug-prompt open debug prompt dialog",
                "> /debug",
                "  second"
            ]
        );
    }

    #[test]
    fn width_aware_prompt_text_uses_prewrapped_visual_lines() {
        let rendered = render_prompt_text_for_width("abcdefghabcdefgh", &[], 10, 0, 12);
        let rows = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, vec!["> abcdefgh", "abcdefgh"]);
    }

    #[test]
    fn prompt_cursor_offsets_for_slash_menu_rows() {
        let area = Rect::new(10, 5, 20, 8);
        assert_eq!(prompt_cursor_position(area, "/de", 3, 2, 0), (15, 7));
    }

    #[test]
    fn status_bar_uses_fixed_two_line_height() {
        assert_eq!(status_bar_height(), 3);
    }

    #[test]
    fn prompt_top_gap_uses_one_spacing_row() {
        assert_eq!(prompt_top_gap_height(), 1);
    }

    fn task(status: TaskStatus, kind: TaskKind, detail: Option<Value>) -> TaskRecord {
        let now = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        TaskRecord {
            id: "task-1".into(),
            agent_id: "agent-1".into(),
            kind,
            status,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("sample task".into()),
            detail,
            recovery: None,
        }
    }

    #[test]
    fn render_task_detail_marks_available_task_actions() {
        let task = task(
            TaskStatus::Running,
            TaskKind::CommandTask,
            Some(json!({
                "output_path": "target/task-output.log",
                "accepts_input": true
            })),
        );

        let detail = render_task_detail(&task);

        assert!(detail.contains("[f] full output: available (TaskOutput)"));
        assert!(detail.contains("[l] follow/live output: available (TaskOutput)"));
        assert!(detail.contains("[x] stop/cancel: available (TaskStop)"));
        assert!(detail.contains("[i] input/continue: available (TaskInput)"));
    }

    #[test]
    fn render_task_detail_marks_unavailable_task_actions() {
        let task = task(
            TaskStatus::Completed,
            TaskKind::SleepJob,
            Some(json!({ "accepts_input": false })),
        );

        let detail = render_task_detail(&task);

        assert!(detail.contains("[f] full output: unavailable"));
        assert!(detail.contains("[l] follow/live output: unavailable"));
        assert!(detail.contains("[x] stop/cancel: unavailable"));
        assert!(detail.contains("[i] input/continue: unavailable"));
    }
}

pub(super) fn trim(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        value.to_string()
    } else {
        let mut trimmed = value
            .chars()
            .take(max_len.saturating_sub(1))
            .collect::<String>();
        trimmed.push('…');
        trimmed
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".into())
}
