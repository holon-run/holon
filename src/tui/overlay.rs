use super::*;
use crate::tui::composer::ComposerState;
use unicode_width::UnicodeWidthStr;

pub(super) const MODEL_REASONING_EFFORT_OPTIONS: [&str; 5] =
    ["inherit", "low", "medium", "high", "xhigh"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayState {
    None,
    Agents {
        selected: usize,
    },
    Events {
        selected_event_id: Option<String>,
        detail_scroll: u16,
    },
    Transcript {
        scroll: u16,
    },
    AgentState {
        scroll: u16,
    },
    Tasks {
        selected: usize,
        detail_scroll: u16,
    },
    ModelPicker {
        filter: String,
        selected: usize,
    },
    ModelEffortPicker {
        model: String,
        selected: usize,
        return_filter: String,
        return_selected: usize,
    },
    DebugPromptInput {
        composer: ComposerState,
    },
    DebugPromptView {
        title: String,
        dump: String,
        scroll: u16,
    },
    HelpView {
        scroll: u16,
    },
}

pub(super) fn draw_overlay(frame: &mut Frame<'_>, app: &TuiApp) {
    match &app.overlay {
        OverlayState::None => {}
        OverlayState::Agents { selected } => draw_agents_overlay(frame, app, *selected),
        OverlayState::Events {
            selected_event_id,
            detail_scroll,
        } => draw_events_overlay(frame, app, selected_event_id.as_deref(), *detail_scroll),
        OverlayState::Transcript { scroll } => draw_transcript_overlay(frame, app, *scroll),
        OverlayState::AgentState { scroll } => draw_agent_state_overlay(frame, app, *scroll),
        OverlayState::Tasks {
            selected,
            detail_scroll,
        } => draw_tasks_overlay(frame, app, *selected, *detail_scroll),
        OverlayState::ModelPicker { filter, selected } => {
            draw_model_picker_overlay(frame, app, filter, *selected)
        }
        OverlayState::ModelEffortPicker {
            model, selected, ..
        } => draw_model_effort_picker_overlay(frame, app, model, *selected),
        OverlayState::DebugPromptInput { composer } => draw_input_modal(
            frame,
            "Debug Prompt",
            "Generate the effective prompt for the selected agent.",
            composer,
            76,
            7,
        ),
        OverlayState::DebugPromptView {
            title,
            dump,
            scroll,
        } => draw_large_text_overlay(frame, title, dump, *scroll),
        OverlayState::HelpView { scroll } => draw_help_overlay(frame, *scroll),
    }
}

fn draw_agents_overlay(frame: &mut Frame<'_>, app: &TuiApp, selected: usize) {
    let popup = centered_rect(92, 80, frame.area());
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(0)])
        .split(popup);
    frame.render_widget(Clear, popup);

    let items = if app.agents.is_empty() {
        vec![ListItem::new("No public agents")]
    } else {
        app.agents
            .iter()
            .map(|agent| {
                let marker = if agent.identity.is_default_agent {
                    "*"
                } else {
                    " "
                };
                let label = format!(
                    "{} {} [{}]",
                    marker,
                    agent.identity.agent_id,
                    render::trim(&format!("{:?}", agent.agent.status), 12)
                );
                ListItem::new(label)
            })
            .collect::<Vec<_>>()
    };
    let list = List::new(items)
        .block(Block::default().title("Agents").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    let selected = (!app.agents.is_empty()).then(|| selected.min(app.agents.len() - 1));
    state.select(selected);
    frame.render_stateful_widget(list, layout[0], &mut state);

    let text = app
        .agents
        .get(selected.unwrap_or(0))
        .map(render::render_summary)
        .unwrap_or_else(|| "No agent selected.".to_string());
    let detail = Paragraph::new(text)
        .block(Block::default().title("Agent Detail").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, layout[1]);
}

fn draw_events_overlay(
    frame: &mut Frame<'_>,
    app: &TuiApp,
    selected_event_id: Option<&str>,
    detail_scroll: u16,
) {
    let popup = centered_rect(94, 82, frame.area());
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(44), Constraint::Min(0)])
        .split(popup);
    frame.render_widget(Clear, popup);

    let events = app
        .projection
        .as_ref()
        .map(|projection| projection.event_log())
        .unwrap_or(&[]);

    let items = if events.is_empty() {
        vec![ListItem::new("No raw events observed yet")]
    } else {
        events
            .iter()
            .rev()
            .map(render::render_projection_event_summary)
            .map(ListItem::new)
            .collect::<Vec<_>>()
    };
    let mut state = ListState::default();
    if !events.is_empty() {
        state.select(Some(
            selected_event_id
                .and_then(|event_id| event_reverse_index(events, event_id))
                .unwrap_or(0)
                .min(events.len().saturating_sub(1)),
        ));
    }
    let list = List::new(items)
        .block(Block::default().title("Raw Events").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, layout[0], &mut state);

    let detail_text = events
        .iter()
        .rev()
        .nth(
            selected_event_id
                .and_then(|event_id| event_reverse_index(events, event_id))
                .unwrap_or(0),
        )
        .map(|event| {
            let payload = serde_json::to_string_pretty(&event.payload).unwrap_or_else(|_| {
                serde_json::to_string(&event.payload).unwrap_or_else(|_| "<invalid json>".into())
            });
            format!(
                "Id: {}\nSeq: {}\nTime: {}\nLane: {:?}\nType: {}\nSummary: {}\n\nPayload:\n{}",
                event.id,
                event.seq,
                event.ts.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S"),
                event.lane,
                event.kind,
                event.summary,
                payload
            )
        })
        .unwrap_or_else(|| "No event selected.".to_string());
    let detail = Paragraph::new(detail_text)
        .block(Block::default().title("Event Detail").borders(Borders::ALL))
        .scroll((detail_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, layout[1]);
}

fn event_reverse_index(
    events: &[crate::tui::projection::ProjectionEventRecord],
    event_id: &str,
) -> Option<usize> {
    events.iter().rev().position(|event| event.id == event_id)
}

fn draw_transcript_overlay(frame: &mut Frame<'_>, app: &TuiApp, scroll: u16) {
    let popup = centered_rect(92, 82, frame.area());
    frame.render_widget(Clear, popup);
    let lines = app
        .transcript
        .iter()
        .rev()
        .map(render::render_transcript_entry)
        .collect::<Vec<_>>();
    let body = if lines.is_empty() {
        "No transcript entries yet.".to_string()
    } else {
        lines.join("\n\n")
    };
    let widget = Paragraph::new(body)
        .block(
            Block::default()
                .title("Transcript (Esc closes)")
                .borders(Borders::ALL),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);
}

fn draw_agent_state_overlay(frame: &mut Frame<'_>, app: &TuiApp, scroll: u16) {
    let popup = centered_rect(92, 82, frame.area());
    frame.render_widget(Clear, popup);
    let widget = Paragraph::new(render::render_agent_state_text(app))
        .block(
            Block::default()
                .title("Agent State (Esc closes)")
                .borders(Borders::ALL),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);
}

fn draw_tasks_overlay(frame: &mut Frame<'_>, app: &TuiApp, selected: usize, detail_scroll: u16) {
    let popup = centered_rect(92, 80, frame.area());
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(38), Constraint::Min(0)])
        .split(popup);
    frame.render_widget(Clear, popup);

    let items = if app.tasks.is_empty() {
        vec![ListItem::new("No active tasks")]
    } else {
        app.tasks
            .iter()
            .rev()
            .map(render::render_task)
            .map(ListItem::new)
            .collect::<Vec<_>>()
    };
    let mut state = ListState::default();
    if !app.tasks.is_empty() {
        state.select(Some(selected.min(app.tasks.len().saturating_sub(1))));
    }
    let list = List::new(items)
        .block(Block::default().title("Tasks").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, layout[0], &mut state);

    let detail_text = app
        .tasks
        .iter()
        .rev()
        .nth(selected)
        .map(render::render_task_detail)
        .unwrap_or_else(|| "No active task selected.".to_string());
    let detail = Paragraph::new(detail_text)
        .block(Block::default().title("Task Detail").borders(Borders::ALL))
        .scroll((detail_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, layout[1]);
}

fn draw_model_picker_overlay(frame: &mut Frame<'_>, app: &TuiApp, filter: &str, selected: usize) {
    let popup = centered_rect(92, 80, frame.area());
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(popup);
    frame.render_widget(Clear, popup);

    let filter_text = if filter.is_empty() {
        "Filter: ".to_string()
    } else {
        format!("Filter: {filter}")
    };
    let filter_widget =
        Paragraph::new(filter_text).block(Block::default().title("Model").borders(Borders::ALL));
    frame.render_widget(filter_widget, layout[0]);

    let rows = crate::tui::model_picker::model_picker_rows(
        app.selected_agent_summary(),
        &app.model_availability,
        filter,
    );
    let items = if rows.is_empty() {
        vec![ListItem::new(
            "No runtime-provided model availability matches the filter",
        )]
    } else {
        rows.iter()
            .map(|row| {
                let status = if row.available { " " } else { "!" };
                ListItem::new(format!("{status} {}\n  {}", row.title, row.detail))
            })
            .collect::<Vec<_>>()
    };
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(selected.min(rows.len().saturating_sub(1))));
    }
    let list = List::new(items)
        .block(Block::default().title("Models").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, layout[1], &mut state);

    let current = app
        .selected_agent_summary()
        .map(render::render_model_status)
        .unwrap_or_else(|| "model: <no agent selected>".into());
    let help = Paragraph::new(format!(
        "{current}\nType to filter, Backspace edits, Up/Down moves, Enter selects, Esc cancels"
    ))
    .block(Block::default().borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(help, layout[2]);
}

fn draw_model_effort_picker_overlay(
    frame: &mut Frame<'_>,
    app: &TuiApp,
    model: &str,
    selected: usize,
) {
    let popup = centered_rect(70, 42, frame.area());
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(popup);
    frame.render_widget(Clear, popup);

    let header = Paragraph::new(format!("Model: {model}")).block(
        Block::default()
            .title("Reasoning Effort")
            .borders(Borders::ALL),
    );
    frame.render_widget(header, layout[0]);

    let items = MODEL_REASONING_EFFORT_OPTIONS
        .iter()
        .map(|option| {
            let detail = if *option == "inherit" {
                "use provider/runtime default"
            } else {
                "force reasoning effort"
            };
            ListItem::new(format!("{option}\n  {detail}"))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(
        selected.min(MODEL_REASONING_EFFORT_OPTIONS.len().saturating_sub(1)),
    ));
    let list = List::new(items)
        .block(Block::default().title("Effort").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, layout[1], &mut state);

    let current = app
        .selected_agent_summary()
        .map(render::render_model_status)
        .unwrap_or_else(|| "model: <no agent selected>".into());
    let help = Paragraph::new(format!(
        "{current}\nUp/Down moves, Enter confirms, Esc goes back"
    ))
    .block(Block::default().borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(help, layout[2]);
}

fn draw_large_text_overlay(frame: &mut Frame<'_>, title: &str, text: &str, scroll: u16) {
    let popup = centered_rect(90, 80, frame.area());
    frame.render_widget(Clear, popup);
    let widget = Paragraph::new(text.to_string())
        .block(Block::default().title(title).borders(Borders::ALL))
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);
}

fn draw_help_overlay(frame: &mut Frame<'_>, scroll: u16) {
    let popup = centered_rect(88, 80, frame.area());
    frame.render_widget(Clear, popup);
    let mut help_lines = [
        "Main View",
        "  Type directly into the composer",
        "  Enter sends the current draft",
        "  Shift+Enter inserts a new line",
        "  Prefix with / to run a local TUI command",
        "  Esc clears the current draft",
        "",
        "Input History (composer must be empty)",
        "  Up browse older input history",
        "  Down browse newer input history",
        "",
        "Editing Shortcuts (work when composer has content)",
        "  Ctrl+A move cursor to start",
        "  Ctrl+E move cursor to end",
        "  Ctrl+B move cursor left",
        "  Ctrl+F move cursor right",
        "  Ctrl+K delete to end of line",
        "  Ctrl+U delete to start of line",
        "  Ctrl+W delete previous word",
        "  Ctrl+H delete previous character (backspace)",
        "  Ctrl+D delete next character",
        "",
        "Overlays",
        "  Esc closes the current overlay",
        "",
        "Quick Help",
        "  ? open this help when the composer is empty",
        "",
        "Scrolling",
        "  PgUp/PgDn scroll the active text view",
        "  Home/End jump to top/bottom",
        "",
        "Exit",
        "  Ctrl+C quit",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    help_lines.extend(["".into()]);
    help_lines.extend(crate::tui::input::slash_help_lines());
    help_lines.extend(["".into(), "Default Keymap".into()]);
    help_lines.extend(
        crate::tui::keymap::DEFAULT_BINDING_HINTS
            .iter()
            .map(|hint| {
                format!(
                    "  {}: {} ({})",
                    hint.context.label(),
                    hint.action,
                    hint.keys
                )
            }),
    );
    let help = help_lines.join("\n");
    let widget = Paragraph::new(help)
        .block(Block::default().title("Help").borders(Borders::ALL))
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);
}

fn draw_input_modal(
    frame: &mut Frame<'_>,
    title: &str,
    help: &str,
    composer: &ComposerState,
    width_percent: u16,
    height_rows: u16,
) {
    let popup = centered_rect_rows(width_percent, height_rows, frame.area());
    frame.render_widget(Clear, popup);
    let text = format!("{help}\n\n{}", composer.as_str());
    let widget = Paragraph::new(text)
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);

    let prefix = format!("{help}\n\n{}", &composer.as_str()[..composer.cursor()]);
    let (x, y) = modal_cursor_position(popup, &prefix);
    frame.set_cursor_position(ratatui::layout::Position { x, y });
}

fn modal_cursor_position(area: Rect, rendered_prefix: &str) -> (u16, u16) {
    let input_width = area.width.saturating_sub(2).max(1);
    let lines = rendered_prefix.split('\n').collect::<Vec<_>>();
    let wrapped_rows_before = lines
        .iter()
        .take(lines.len().saturating_sub(1))
        .map(|line| wrapped_rows(line, input_width))
        .sum::<u16>();
    let last_line = lines.last().copied().unwrap_or("");
    let last_line_width = display_width(last_line);
    let soft_wrap_row = if last_line_width == 0 {
        0
    } else {
        last_line_width.saturating_sub(1) / input_width
    };
    let column = if last_line_width == 0 {
        0
    } else {
        last_line_width - soft_wrap_row * input_width
    };
    let max_x = area.x + area.width.saturating_sub(2);
    let max_y = area.y + area.height.saturating_sub(2);
    (
        (area.x + 1 + column).min(max_x),
        (area.y + 1 + wrapped_rows_before + soft_wrap_row).min(max_y),
    )
}

fn wrapped_rows(line: &str, visible_line_width: u16) -> u16 {
    let line_width = display_width(line);
    let rows = (line_width + visible_line_width.saturating_sub(1)) / visible_line_width;
    rows.max(1)
}

fn display_width(text: &str) -> u16 {
    UnicodeWidthStr::width(text).min(u16::MAX as usize) as u16
}

pub(super) fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let width_percent = width_percent.min(100);
    let height_percent = height_percent.min(100);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

pub(super) fn centered_rect_rows(width_percent: u16, height_rows: u16, area: Rect) -> Rect {
    let width_percent = width_percent.min(100);
    let height_rows = height_rows.clamp(1, area.height.max(1));
    let top = area.height.saturating_sub(height_rows) / 2;
    let bottom = area.height.saturating_sub(height_rows).saturating_sub(top);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top),
            Constraint::Length(height_rows),
            Constraint::Length(bottom),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}
