use super::overlay::draw_overlay;
use super::*;
use crate::tui::input::slash_menu_specs;
use unicode_width::UnicodeWidthStr;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let area = frame.area();
    let slash_menu = slash_menu_lines(app);
    let prompt_height = prompt_pane_height(app.composer.as_str(), slash_menu.len());
    let status_height = status_bar_height(&app.status_line);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(4),
            Constraint::Length(prompt_height),
            Constraint::Length(status_height),
        ])
        .split(area);

    draw_header(frame, outer[0], app);
    draw_main_panels(frame, outer[1], app);
    draw_activity_pane(frame, outer[2], app);
    draw_prompt_pane(frame, outer[3], app, &slash_menu);
    draw_status_bar(frame, outer[4], app);
    draw_overlay(frame, app);
}

fn draw_main_panels(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let layout = if area.width >= 110 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area)
    };
    draw_chat(frame, layout[0], app);
    draw_runtime_state(frame, layout[1], app);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let text = match app.selected_agent_summary() {
        Some(agent) => render_header(agent),
        None => "No agent selected.".to_string(),
    };
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::BOTTOM))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_chat(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let body = chat_text(app);
    let max_scroll = paragraph_max_scroll(&body, area);
    app.chat_max_scroll = max_scroll;
    let scroll = app.chat_scroll.effective_scroll(max_scroll);
    let paragraph = Paragraph::new(body)
        .block(Block::default().title("Conversation").borders(Borders::ALL))
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_runtime_state(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let paragraph = Paragraph::new(render_runtime_state_text(app))
        .block(
            Block::default()
                .title("Runtime State")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_activity_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let paragraph = Paragraph::new(render_activity_text(app))
        .block(Block::default().title("Logs").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_prompt_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, slash_menu: &[Line<'static>]) {
    let paragraph = Paragraph::new(render_prompt_text(app.composer.as_str(), slash_menu))
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    if matches!(app.overlay, OverlayState::None) {
        let (x, y) = prompt_cursor_position(
            area,
            app.composer.as_str(),
            app.composer.cursor(),
            slash_menu.len() as u16,
        );
        frame.set_cursor_position(ratatui::layout::Position { x, y });
    }
}

fn draw_status_bar(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let help = match app.overlay {
        OverlayState::None if !slash_menu_lines(app).is_empty() => {
            "Slash: Up/Down select  Tab complete  Enter run  Esc close"
        }
        OverlayState::None => {
            "/help commands  Left/Right/Home/End edit  Up/Down scroll  Ctrl+A agents  Ctrl+E events  Ctrl+J tasks  Ctrl+C quit"
        }
        OverlayState::Agents => "Agents: Up/Down, Esc",
        OverlayState::Events { .. } => "Events: Up/Down, PgUp/PgDn, Home/End, Esc",
        OverlayState::Transcript { .. } => "Transcript: Up/Down, PgUp/PgDn, Home/End, Esc",
        OverlayState::Tasks { .. } => "Tasks: Up/Down, PgUp/PgDn, Home/End, Esc",
        OverlayState::ModelPicker { .. } => {
            "Model: type filter, Backspace edit, Up/Down move, Enter select, Esc cancel"
        }
        OverlayState::DebugPromptInput { .. } => "Debug prompt: Enter confirm, Esc cancel",
        OverlayState::DebugPromptView { .. } => "Debug prompt: Up/Down, PgUp/PgDn, Home/End, Esc",
        OverlayState::HelpView { .. } => "Help: Up/Down, PgUp/PgDn, Home/End, Esc",
    };

    let refreshed = app
        .last_refresh_at
        .map(|timestamp| timestamp.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "never".into());
    let last_event = app
        .last_event_at
        .map(|timestamp| timestamp.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "never".into());
    let stale = app
        .stale_slice_summary()
        .map(|summary| format!("  Projection stale: {summary}"))
        .unwrap_or_default();
    let connection = if let Some(detail) = app.connection_detail() {
        format!(
            "{} ({detail})  snapshot {refreshed}  event {last_event}{stale}",
            app.connection_label()
        )
    } else {
        format!(
            "{}  snapshot {refreshed}  event {last_event}{stale}",
            app.connection_label()
        )
    };
    let model = app
        .selected_agent_summary()
        .map(render_model_status)
        .unwrap_or_else(|| "model: <no agent selected>".into());
    let text = if app.status_line.trim().is_empty() {
        format!("{help}\n{connection}  {model}")
    } else {
        format!("{help}\n{connection}  {model}\n{}", app.status_line)
    };
    let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::TOP));
    frame.render_widget(paragraph, area);
}

fn render_runtime_state_text(app: &TuiApp) -> String {
    let Some(projection) = app.projection.as_ref() else {
        return "Projection not initialized yet.".into();
    };
    let agent = &projection.agent;

    let mut lines = vec![
        format!(
            "Agent: {} / {:?}",
            agent.identity.agent_id, agent.agent.status
        ),
        format!(
            "Queue: pending {}  active tasks {}",
            agent.agent.pending,
            agent.agent.active_task_ids.len()
        ),
        format!(
            "Closure: {:?} / {:?}",
            agent.closure.outcome, agent.closure.runtime_posture
        ),
        render_model_status(agent),
    ];

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
            let summary = item
                .summary
                .as_deref()
                .or(item.progress_note.as_deref())
                .unwrap_or(item.id.as_str());
            lines.push(format!("  - [{:?}] {}", item.status, trim(summary, 40)));
        }
    }
    if let Some(plan) = projection.work_plan.as_ref() {
        let active = plan
            .items
            .iter()
            .find(|item| matches!(item.status, crate::types::WorkPlanStepStatus::InProgress))
            .or_else(|| {
                plan.items
                    .iter()
                    .find(|item| matches!(item.status, crate::types::WorkPlanStepStatus::Pending))
            })
            .map(|item| trim(&item.step, 40))
            .unwrap_or_else(|| "<completed>".into());
        lines.push(format!("  Plan: {active}"));
    }
    lines.push(String::new());
    lines.push("Waiting".into());
    if projection.waiting_intents.is_empty() {
        lines.push("  No active waiting intents".into());
    } else {
        for waiting in projection.waiting_intents.iter().take(2) {
            lines.push(format!("  - {}", trim(&waiting.summary, 44)));
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

    lines.join("\n")
}

fn render_activity_text(app: &TuiApp) -> String {
    let Some(projection) = app.projection.as_ref() else {
        return render_status_line_or_default(app, "Bootstrapping snapshot and stream...");
    };
    let agent_id = projection.agent.identity.agent_id.as_str();
    let events = projection.recent_log_events(4);
    if events.is_empty() {
        if let Some(cached) = app.activity_text_cache.borrow().as_ref() {
            if cached.agent_id == agent_id {
                return cached.text.clone();
            }
        }
        // Show "No in-flight activity" only when turn has ended, otherwise show cached text
        if projection.session.current_run_id.is_none() {
            return "No in-flight activity.".into();
        }
        // During active turn, show the last cached text for this agent or empty
        return app.activity_text_cache
            .borrow()
            .as_ref()
            .filter(|c| c.agent_id == agent_id)
            .map(|c| c.text.clone())
            .unwrap_or_default();
    }

    let text = events
        .into_iter()
        .map(|event| {
            format_activity_event(event)
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        *app.activity_text_cache.borrow_mut() = Some(super::CachedActivityText {
            agent_id: agent_id.to_string(),
            text: text.clone(),
        });
        text
    } else {
        app.activity_text_cache
            .borrow()
            .as_ref()
            .filter(|cached| cached.agent_id == agent_id)
            .filter(|cached| !cached.text.is_empty())
            .map(|cached| cached.text.clone())
            .unwrap_or_default()
    }
}

fn render_status_line_or_default(app: &TuiApp, default: &str) -> String {
    if app.status_line.is_empty() {
        default.to_string()
    } else {
        app.status_line.clone()
    }
}

fn prompt_pane_height(buffer: &str, slash_menu_rows: usize) -> u16 {
    let mut prompt_lines = buffer.lines().count();
    if buffer.is_empty() || buffer.ends_with('\n') {
        prompt_lines += 1;
    }
    let prompt_lines = prompt_lines.max(1) as u16;
    (prompt_lines + slash_menu_rows as u16 + 2).clamp(3, 12)
}

fn status_bar_height(status_line: &str) -> u16 {
    if status_line.trim().is_empty() {
        3
    } else {
        4
    }
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

fn render_prompt_text(buffer: &str, slash_menu: &[Line<'static>]) -> Text<'static> {
    if !slash_menu.is_empty() {
        let mut lines = slash_menu.to_vec();
        lines.push(Line::from(render_prompt_buffer(buffer)));
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

fn slash_menu_lines(app: &TuiApp) -> Vec<Line<'static>> {
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
        let token = buffer.trim_start().split_whitespace().next().unwrap_or("/");
        return vec![Line::from(vec![
            Span::styled("  ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                format!("no command matches {token}"),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ])];
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

fn prompt_cursor_position(area: Rect, buffer: &str, cursor: usize, hint_rows: u16) -> (u16, u16) {
    let input_width = area.width.saturating_sub(2).max(1);
    let rendered = render_prompt_buffer(&buffer[..cursor]);
    let lines = rendered.split('\n').collect::<Vec<_>>();
    let wrapped_rows_before = lines
        .iter()
        .take(lines.len().saturating_sub(1))
        .map(|line| wrapped_prompt_rows(line, input_width))
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
        (area.y + 1 + hint_rows + 1 + wrapped_rows_before + soft_wrap_row).min(max_y),
    )
}

fn wrapped_prompt_rows(line: &str, visible_line_width: u16) -> u16 {
    let line_width = display_width(line);
    let rows = (line_width + visible_line_width.saturating_sub(1)) / visible_line_width;
    rows.max(1)
}

fn display_width(text: &str) -> u16 {
    UnicodeWidthStr::width(text).min(u16::MAX as usize) as u16
}

fn format_activity_event(event: &crate::tui::projection::ProjectionEventRecord) -> String {
    let detail = if event.kind == "tool_executed" || event.kind == "tool_execution_failed" {
        event
            .payload
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool_name| {
                if tool_name == "ExecCommand" {
                    event
                        .payload
                        .get("exec_command_cmd")
                        .and_then(Value::as_str)
                        .map(|cmd| format!("ExecCommand: {cmd}"))
                        .unwrap_or_else(|| event.summary.clone())
                } else {
                    event.summary.clone()
                }
            })
            .unwrap_or_else(|| event.summary.clone())
    } else {
        event.summary.clone()
    };

    format!(
        "[{}] {}",
        event.ts.with_timezone(&Local).format("%H:%M:%S"),
        detail
    )
}

pub(super) fn render_header(agent: &AgentSummary) -> String {
    let workspace = agent
        .agent
        .active_workspace_entry
        .as_ref()
        .map(|entry| {
            format!(
                "{} ({}/{})",
                entry.workspace_id,
                workspace_projection_label(Some(entry.projection_kind)),
                workspace_access_mode_label(Some(entry.access_mode))
            )
        })
        .unwrap_or_else(|| "none".to_string());
    let mut line = format!(
        "{}  {:?}  {}  pending {}  tasks {}",
        agent.identity.agent_id,
        agent.agent.status,
        agent.identity.contract_badge(),
        agent.agent.pending,
        agent.agent.active_task_ids.len(),
    );
    if agent.lifecycle.resume_required {
        line.push_str("  resume required");
    }
    line.push_str(&format!("  workspace {workspace}"));
    line
}

pub(super) fn render_projection_event_summary(
    event: &crate::tui::projection::ProjectionEventRecord,
) -> String {
    let description = if event.summary == event.kind {
        event.summary.clone()
    } else {
        format!("{}: {}", event.kind, event.summary)
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

    if let Some(detail) = &task.detail {
        lines.push(String::new());
        lines.push("Detail:".into());
        lines.push(serde_json::to_string_pretty(detail).unwrap_or_else(|_| compact_json(detail)));
    }

    lines.join("\n")
}

pub(super) fn render_model_status(agent: &AgentSummary) -> String {
    let model = agent
        .model
        .active_model
        .as_ref()
        .unwrap_or(&agent.model.effective_model);
    if agent.model.fallback_active {
        let requested = agent
            .model
            .requested_model
            .as_ref()
            .unwrap_or(&agent.model.effective_model);
        return format!(
            "model: {} (fallback from {})",
            model.as_string(),
            requested.as_string()
        );
    }
    if agent.model.override_model.is_some() {
        return format!("model: {} (agent override)", model.as_string());
    }
    format!("model: {}", model.as_string())
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
        format!("Active tasks: {}", agent.agent.active_task_ids.len()),
        format!(
            "Closure: {:?} / posture {:?}",
            agent.closure.outcome, agent.closure.runtime_posture
        ),
    ];
    if let Some(model_override) = agent.model.override_model.as_ref() {
        lines.push(format!("Override: {}", model_override.as_string()));
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
        "Execution backend: {:?}",
        agent.execution.policy.backend
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        format_activity_event, prompt_cursor_position, render_header, render_model_status,
        render_prompt_buffer, render_prompt_text, render_summary, status_bar_height,
    };
    use crate::system::{ExecutionProfile, ExecutionSnapshot};
    use crate::tui::projection::{ProjectionEventLane, ProjectionEventRecord};
    use crate::types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
        AgentTokenUsageSummary, AgentVisibility, ChildAgentObservabilitySnapshot, ChildAgentPhase,
        ChildAgentSummary, ClosureDecision, ClosureOutcome, LoadedAgentsMdView, RuntimePosture,
        SkillsRuntimeView, TokenUsage,
    };
    use chrono::Utc;
    use ratatui::prelude::{Line, Rect};
    use serde_json::json;
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
                available_models: Vec::new(),
                model_availability: Vec::new(),
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
                    active_work_item_id: Some("work-1".into()),
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
    fn render_header_uses_agent_contract_badge() {
        let rendered = render_header(&sample_agent_summary());
        assert!(rendered.contains("public/self_owned (public_named)"));
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
    fn prompt_cursor_tracks_multiline_end_position() {
        let area = Rect::new(10, 5, 40, 6);
        assert_eq!(
            prompt_cursor_position(area, "first\nsecond", "first\nsecond".len(), 0),
            (19, 8)
        );
    }

    #[test]
    fn prompt_cursor_tracks_soft_wrapped_long_lines() {
        let area = Rect::new(10, 5, 10, 6);
        assert_eq!(
            prompt_cursor_position(area, "abcdefgh", "abcdefgh".len(), 0),
            (13, 8)
        );
    }

    #[test]
    fn prompt_cursor_uses_display_width_for_wide_characters() {
        let area = Rect::new(10, 5, 10, 6);
        assert_eq!(
            prompt_cursor_position(area, "你好你好", "你好你好".len(), 0),
            (13, 8)
        );
    }

    #[test]
    fn prompt_cursor_clamps_to_prompt_pane_height() {
        let area = Rect::new(10, 5, 10, 4);
        assert_eq!(
            prompt_cursor_position(area, "abcdefgh\nabcdefgh", "abcdefgh\nabcdefgh".len(), 0),
            (13, 7)
        );
    }

    #[test]
    fn prompt_cursor_tracks_insert_position_inside_line() {
        let area = Rect::new(10, 5, 20, 6);
        assert_eq!(prompt_cursor_position(area, "hello", 2, 0), (15, 7));
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
    fn prompt_cursor_offsets_for_slash_menu_rows() {
        let area = Rect::new(10, 5, 20, 8);
        assert_eq!(prompt_cursor_position(area, "/de", 3, 2), (16, 9));
    }

    #[test]
    fn empty_status_line_uses_compact_status_bar_height() {
        assert_eq!(status_bar_height(""), 3);
        assert_eq!(status_bar_height("Action failed"), 4);
    }

    #[test]
    fn activity_shows_full_exec_command() {
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

        let rendered = format_activity_event(&event);
        assert!(rendered.contains("ExecCommand: git status --short --branch"));
    }

    #[test]
    fn activity_uses_summary_for_non_command_tools() {
        let event = ProjectionEventRecord {
            id: "evt-2".into(),
            seq: 2,
            ts: Utc::now(),
            lane: ProjectionEventLane::Debug,
            kind: "tool_executed".into(),
            summary: "tool executed: read_file".into(),
            payload: json!({
                "tool_name": "read_file"
            }),
        };

        let rendered = format_activity_event(&event);
        assert!(rendered.contains("tool executed: read_file"));
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
