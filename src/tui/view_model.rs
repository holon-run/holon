use std::path::{Path, PathBuf};

use ratatui::{style::Style, text::Span};
use unicode_width::UnicodeWidthStr;

use crate::types::{AgentStatus, AgentSummary, WaitingReason, AGENT_HOME_WORKSPACE_ID};

use super::{
    app::ComposerEditMode,
    keymap::{status_hint, KeyContext},
    overlay::OverlayState,
    TuiApp,
};

pub(super) struct HeaderViewModel {
    pub(super) line: String,
}

impl HeaderViewModel {
    pub(super) fn from_app(app: &TuiApp) -> Self {
        let line = app
            .selected_agent_summary()
            .map(render_header_line)
            .unwrap_or_else(|| "No agent selected.".to_string());
        Self { line }
    }
}

pub(super) struct StatusbarViewModel {
    pub(super) context_line: String,
    pub(super) status_line: String,
}

impl StatusbarViewModel {
    pub(super) fn from_app(app: &TuiApp, slash_visible: bool) -> Self {
        let context_line = format!("{} · {}", execution_root_summary(app), model_summary(app));
        let status_line = statusbar_detail(app, slash_visible);
        Self {
            context_line,
            status_line,
        }
    }

    pub(super) fn for_width(app: &TuiApp, slash_visible: bool, width: u16) -> Self {
        let width = usize::from(width);
        let mut view_model = Self::from_app(app, slash_visible);
        let agent = app
            .selected_agent_summary()
            .map(|agent| agent.identity.display_name())
            .unwrap_or("<no agent>");
        let model = model_summary(app);
        let root = execution_root_summary(app);
        let important_width = agent.width() + model.width();
        view_model.context_line = if important_width + 7 <= width {
            format!(
                "{agent} · {} · {model}",
                ellipsize(&root, width - important_width - 6)
            )
        } else {
            fit_statusbar_pair(agent, &model, width)
        };

        if view_model.status_line.width() > width {
            // Transport addresses are less useful here than connection failures or shortcuts.
            let connection = app.connection_label();
            let state = connection
                .split_once(" via ")
                .map(|(state, _)| state)
                .unwrap_or(&connection);
            view_model.status_line =
                fit_statusbar_pair(state, &statusbar_hint(app, slash_visible), width);
        }
        view_model
    }
}

fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_string();
    }
    let mut result = String::new();
    let mut used = 0;
    let span = Span::raw(text);
    for grapheme in span.styled_graphemes(Style::default()) {
        let next_width = grapheme.symbol.width();
        if used + next_width > width - 1 {
            break;
        }
        result.push_str(grapheme.symbol);
        used += next_width;
    }
    result.push('…');
    result
}

fn fit_statusbar_pair(first: &str, second: &str, width: usize) -> String {
    if width < 5 {
        return ellipsize(first, width);
    }
    let available = width - 3;
    let first_width = first
        .width()
        .min(available - second.width().min(available / 2));
    format!(
        "{} · {}",
        ellipsize(first, first_width),
        ellipsize(second, available - first_width)
    )
}

pub(super) fn render_header_line(agent: &AgentSummary) -> String {
    format!(
        "{}  {}",
        agent.identity.display_name(),
        agent_status_label(agent)
    )
}

fn agent_status_label(agent: &AgentSummary) -> &'static str {
    if agent.closure.waiting_reason == Some(WaitingReason::AwaitingOperatorInput) {
        return "waiting for you";
    }
    match agent.agent.status {
        AgentStatus::Booting => "booting",
        AgentStatus::AwakeIdle => "idle",
        AgentStatus::AwakeRunning => "running",
        AgentStatus::AwaitingTask => "waiting for task",
        AgentStatus::Asleep => "sleeping",
        AgentStatus::Stopped => "stopped",
    }
}

fn execution_root_summary(app: &TuiApp) -> String {
    // Try the unified workspace snapshot first, then fall back to the agent state's
    // active_workspace_entry. Extract the fields we need into a common shape.
    let (workspace_id, anchor, exec_root): (String, Option<PathBuf>, Option<PathBuf>) = app
        .projection
        .as_ref()
        .and_then(|projection| projection.workspace.active_workspace())
        .map(|ws| {
            (
                ws.workspace_id.clone(),
                ws.workspace_anchor.as_ref().map(PathBuf::from),
                ws.execution_root.as_ref().map(PathBuf::from),
            )
        })
        .or_else(|| {
            app.selected_agent_summary()
                .and_then(|agent| agent.agent.active_workspace_entry.as_ref())
                .map(|entry| {
                    (
                        entry.workspace_id.clone(),
                        Some(entry.workspace_anchor.clone()),
                        Some(entry.execution_root.clone()),
                    )
                })
        })
        .unwrap_or_else(|| (String::new(), None, None));

    let Some(anchor) = anchor else {
        return "workspace not ready".into();
    };
    let exec_root = exec_root.unwrap_or_else(|| anchor.clone());
    let label = workspace_label(&workspace_id, &anchor);
    format!("{} ({})", label, shorten_home_path(&exec_root))
}

fn workspace_label(workspace_id: &str, workspace_anchor: &Path) -> String {
    if workspace_id == AGENT_HOME_WORKSPACE_ID {
        return AGENT_HOME_WORKSPACE_ID.to_string();
    }
    workspace_anchor
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn shorten_home_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    shorten_home_path_with_home(path, home.as_deref())
}

fn shorten_home_path_with_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if let Ok(relative) = path.strip_prefix(home) {
            if relative.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn model_summary(app: &TuiApp) -> String {
    app.selected_agent_summary()
        .map(render_model_detail)
        .unwrap_or_else(|| "<no model>".into())
}

pub(super) fn render_model_detail(agent: &AgentSummary) -> String {
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
            "{} (fallback from {})",
            model.as_compact_display(),
            requested.as_compact_display()
        );
    }
    if agent.model.override_model.is_some() {
        if let Some(effort) = agent.model.override_reasoning_effort.as_deref() {
            return format!(
                "{} (agent override, effort={})",
                model.as_compact_display(),
                effort
            );
        }
        return format!("{} (agent override)", model.as_compact_display());
    }
    model.as_compact_display()
}

fn statusbar_detail(app: &TuiApp, slash_visible: bool) -> String {
    format!(
        "{} · {}",
        app.connection_label(),
        statusbar_hint(app, slash_visible)
    )
}

fn statusbar_hint(app: &TuiApp, slash_visible: bool) -> String {
    let status_line = app.status_line.trim();
    app.connection_detail()
        .filter(|detail| !detail.trim().is_empty())
        .map(ToString::to_string)
        .or_else(|| overlay_hint(app, slash_visible).map(ToString::to_string))
        .or_else(|| (!status_line.is_empty()).then(|| status_line.to_string()))
        .or_else(|| active_tasks_hint(app))
        .or_else(|| composer_edit_hint(app).map(ToString::to_string))
        .unwrap_or_else(|| "Type / for commands · /help for shortcuts".into())
}

fn overlay_hint(app: &TuiApp, slash_visible: bool) -> Option<&'static str> {
    if slash_visible {
        return Some(status_hint(KeyContext::SlashMenu, true));
    }
    let context = match &app.overlay {
        OverlayState::None => return None,
        OverlayState::Agents { .. } => KeyContext::AgentsOverlay,
        OverlayState::Events { .. } => KeyContext::EventsOverlay,
        OverlayState::Transcript { .. }
        | OverlayState::AgentState { .. }
        | OverlayState::DebugPromptView { .. }
        | OverlayState::HelpView { .. } => KeyContext::ScrollOverlay,
        OverlayState::Tasks { .. } | OverlayState::SkillCatalog { .. } => KeyContext::TasksOverlay,
        OverlayState::TurnDetails { .. } => {
            return Some("Up/Down select · Enter fold/unfold · PgUp/PgDn · Esc");
        }
        OverlayState::TemplateCatalog { .. } => KeyContext::TemplatesOverlay,
        OverlayState::ModelPicker { .. } => KeyContext::ModelPicker,
        OverlayState::ModelEffortPicker { .. } => KeyContext::ModelEffortPicker,
        OverlayState::DeleteConfirm { .. } => KeyContext::ScrollOverlay,
        OverlayState::DebugPromptInput { .. } | OverlayState::TemplateUrlInput { .. } => {
            KeyContext::DebugPromptInput
        }
    };
    Some(status_hint(context, false))
}

fn composer_edit_hint(app: &TuiApp) -> Option<&'static str> {
    match app.composer_edit_mode {
        ComposerEditMode::Default => None,
        ComposerEditMode::VimInsert => Some("VIM INSERT · Esc normal · Enter submit"),
        ComposerEditMode::VimNormal => Some("VIM NORMAL · i/a insert · Enter submit · /vim off"),
    }
}

fn active_tasks_hint(app: &TuiApp) -> Option<String> {
    let count = app
        .projection
        .as_ref()
        .map(|projection| projection.tasks.len())
        .unwrap_or(0);
    match count {
        0 => None,
        1 => Some("1 active task · /tasks to inspect".into()),
        count => Some(format!("{count} active tasks · /tasks to inspect")),
    }
}

#[cfg(test)]
mod tests {
    use super::{ellipsize, shorten_home_path_with_home, workspace_label, StatusbarViewModel};
    use crate::{
        client::LocalClient,
        tui::{
            logging::TuiLogWriter,
            overlay::OverlayState,
            runtime::TuiConnectionState,
            tests::{sample_agent_summary, test_config},
            TuiApp,
        },
        types::{ActiveWorkspaceEntry, AGENT_HOME_WORKSPACE_ID},
    };
    use std::{path::Path, time::Duration};
    use unicode_width::UnicodeWidthStr;

    fn statusbar_app(execution_root: &str) -> TuiApp {
        let mut app = TuiApp::new(
            LocalClient::new(test_config()).unwrap(),
            TuiLogWriter::new_temp().unwrap(),
        );
        let mut agent = sample_agent_summary("statusbar-agent");
        agent.identity.name = Some("助手-agent👩‍💻".into());
        agent.agent.active_workspace_entry = Some(ActiveWorkspaceEntry {
            workspace_id: "ws-statusbar".into(),
            workspace_anchor: "/tmp/project".into(),
            execution_root_id: "worktree:statusbar".into(),
            execution_root: execution_root.into(),
            projection_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
            cwd: execution_root.into(),
            occupancy_id: None,
            projection_metadata: None,
        });
        app.agents = vec![agent];
        app.connection_state = TuiConnectionState::Streaming;
        app.status_line = "Ready".into();
        app
    }

    #[test]
    fn statusbar_reserves_agent_and_model_before_long_paths() {
        let root = format!("/tmp/{}/project", "很长的路径-with-unicode/".repeat(12));
        let mut app = statusbar_app(&root);
        app.status_line = "操作已完成-detail ".repeat(30);

        for width in [80, 84, 120] {
            let view_model = StatusbarViewModel::for_width(&app, false, width);
            assert!(view_model.context_line.width() <= usize::from(width));
            assert!(view_model.status_line.width() <= usize::from(width));
            assert!(view_model.context_line.contains("助手-agent👩‍💻"));
            assert!(view_model
                .context_line
                .contains("anthropic/claude-sonnet-4-6"));
            assert!(view_model.context_line.contains('…'));
            assert!(view_model.status_line.starts_with("streaming · "));
            assert!(view_model.status_line.ends_with('…'));
            assert!(!view_model.context_line.contains('\n'));
            assert!(!view_model.status_line.contains('\n'));
        }
    }

    #[test]
    fn statusbar_keeps_wide_details_and_legacy_view_model() {
        let app = statusbar_app("/tmp/project");
        let legacy = StatusbarViewModel::from_app(&app, false);
        let fitted = StatusbarViewModel::for_width(&app, false, 120);

        assert_eq!(
            fitted.context_line,
            format!("助手-agent👩‍💻 · {}", legacy.context_line)
        );
        assert_eq!(fitted.status_line, legacy.status_line);
        assert!(legacy.context_line.starts_with("project ("));
        assert!(!legacy.context_line.contains("助手-agent👩‍💻"));
        assert!(!fitted.context_line.contains('…'));
    }

    #[test]
    fn statusbar_handles_zero_and_tiny_widths() {
        let app = statusbar_app("/tmp/project");
        for width in 0..=12 {
            let view_model = StatusbarViewModel::for_width(&app, false, width);
            assert!(view_model.context_line.width() <= usize::from(width));
            assert!(view_model.status_line.width() <= usize::from(width));
            if width == 0 {
                assert!(view_model.context_line.is_empty());
                assert!(view_model.status_line.is_empty());
            } else if width == 1 {
                assert_eq!(view_model.context_line, "…");
                assert_eq!(view_model.status_line, "…");
            }
        }
    }

    #[test]
    fn statusbar_keeps_model_visible_with_a_long_agent_name() {
        let mut app = statusbar_app("/tmp/project");
        app.agents[0].identity.name = Some("助手👩‍💻-long-agent-name-".repeat(20));
        for width in [80, 84, 120] {
            let view_model = StatusbarViewModel::for_width(&app, false, width);
            assert!(view_model.context_line.width() <= usize::from(width));
            assert!(view_model
                .context_line
                .ends_with("anthropic/claude-sonnet-4-6"));
            assert!(view_model.context_line.contains('…'));
        }
    }

    #[test]
    fn statusbar_ellipsizes_display_columns_without_splitting_graphemes() {
        assert_eq!(ellipsize("界a👩‍💻e\u{301}Z", 6), "界a👩‍💻…");
        assert_eq!(ellipsize("e\u{301}模型", 2), "e\u{301}…");
        assert_eq!(ellipsize("界", 2), "界");
        assert_eq!(ellipsize("界", 1), "…");
        assert_eq!(ellipsize("界", 0), "");
        assert_eq!(ellipsize("", 1), "");

        for width in 0..=20 {
            assert!(ellipsize("abc中文👩‍💻e\u{301}xyz", width).width() <= width);
        }
    }

    #[test]
    fn statusbar_prioritizes_connection_failures_over_stale_success_and_overlay_hints() {
        let mut app = statusbar_app("/tmp/project");
        app.status_line = "Successfully loaded agents".into();
        app.overlay = OverlayState::Tasks {
            selected: 0,
            detail_scroll: 0,
        };
        for (state, label) in [
            (
                TuiConnectionState::Reconnecting {
                    attempt: 3,
                    retry_after: Duration::from_secs(2),
                    last_error: format!("连接失败: socket closed {}", "detail/".repeat(40)),
                },
                "reconnecting",
            ),
            (
                TuiConnectionState::RefreshRequired {
                    reason: format!("连接失败: snapshot expired {}", "detail/".repeat(40)),
                },
                "refresh-required",
            ),
            (
                TuiConnectionState::Disconnected {
                    reason: format!("连接失败: daemon stopped {}", "detail/".repeat(40)),
                },
                "disconnected",
            ),
        ] {
            app.connection_state = state;
            let legacy = StatusbarViewModel::from_app(&app, false);
            assert!(legacy.status_line.contains("连接失败:"));
            assert!(!legacy.status_line.contains("Successfully"));
            assert!(!legacy.status_line.contains("Tasks:"));
            for width in [80, 84, 120] {
                let view_model = StatusbarViewModel::for_width(&app, false, width);
                assert!(view_model.status_line.starts_with(label));
                assert!(view_model.status_line.contains("连接失败:"));
                assert!(view_model.status_line.width() <= usize::from(width));
                assert!(view_model.status_line.ends_with('…'));
            }
        }
    }

    #[test]
    fn statusbar_explains_turn_detail_navigation_and_folding() {
        let mut app = statusbar_app("/tmp/project");
        app.overlay = OverlayState::TurnDetails {
            selected_id: None,
            detail_scroll: 0,
        };
        let view_model = StatusbarViewModel::for_width(&app, false, 120);
        assert!(view_model.status_line.contains("Up/Down select"));
        assert!(view_model.status_line.contains("Enter fold/unfold"));
        assert!(view_model.status_line.contains("PgUp/PgDn"));
        assert!(view_model.status_line.contains("Esc"));
    }

    #[test]
    fn statusbar_preserves_healthy_overlay_and_transient_status_precedence() {
        let mut app = statusbar_app("/tmp/project");
        app.status_line = "Successfully loaded agents".into();
        assert!(StatusbarViewModel::from_app(&app, false)
            .status_line
            .contains("Successfully loaded agents"));
        app.overlay = OverlayState::Tasks {
            selected: 0,
            detail_scroll: 0,
        };
        let view_model = StatusbarViewModel::for_width(&app, false, 120);
        assert!(view_model.status_line.contains("Tasks:"));
        assert!(!view_model.status_line.contains("Successfully"));
    }

    #[test]
    fn workspace_label_uses_anchor_name_not_random_workspace_id() {
        assert_eq!(
            workspace_label(
                "ws-123456",
                Path::new("/Users/example/opensource/src/github.com/holon-run/holon")
            ),
            "holon"
        );
        assert_eq!(
            workspace_label(AGENT_HOME_WORKSPACE_ID, Path::new("/tmp/default")),
            "agent_home"
        );
    }

    #[test]
    fn shorten_home_path_uses_tilde_for_home_relative_paths() {
        assert_eq!(
            shorten_home_path_with_home(
                Path::new("/Users/example/opensource/holon"),
                Some(Path::new("/Users/example"))
            ),
            "~/opensource/holon"
        );
    }
}
