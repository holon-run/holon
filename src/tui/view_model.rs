use std::path::{Path, PathBuf};

use crate::types::{AgentStatus, AgentSummary, WaitingReason, AGENT_HOME_WORKSPACE_ID};

use super::{
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
    pub(super) fn from_app(app: &TuiApp) -> Self {
        let context_line = format!("{} · {}", execution_root_summary(app), model_summary(app));
        let status_line = statusbar_detail(app);
        Self {
            context_line,
            status_line,
        }
    }
}

pub(super) fn render_header_line(agent: &AgentSummary) -> String {
    let mut line = format!("{}  {}", agent.identity.agent_id, agent_status_label(agent));
    if agent.lifecycle.resume_required {
        line.push_str(" · resume required");
    }
    line
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
        AgentStatus::Paused => "paused",
        AgentStatus::Stopped => "stopped",
    }
}

fn execution_root_summary(app: &TuiApp) -> String {
    let active_entry = app
        .projection
        .as_ref()
        .and_then(|projection| projection.workspace.active_workspace_entry.as_ref())
        .or_else(|| {
            app.selected_agent_summary()
                .and_then(|agent| agent.agent.active_workspace_entry.as_ref())
        });
    let Some(entry) = active_entry else {
        return "workspace not ready".into();
    };
    let label = workspace_label(
        entry.workspace_id.as_str(),
        entry.workspace_anchor.as_path(),
    );
    format!("{} ({})", label, shorten_home_path(&entry.execution_root))
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
        .map(agent_model_summary)
        .unwrap_or_else(|| "<no model>".into())
}

fn agent_model_summary(agent: &AgentSummary) -> String {
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
            model.as_string(),
            requested.as_string()
        );
    }
    if agent.model.override_model.is_some() {
        if let Some(effort) = agent.model.override_reasoning_effort.as_deref() {
            return format!("{} (agent override, effort={})", model.as_string(), effort);
        }
        return format!("{} (agent override)", model.as_string());
    }
    model.as_string()
}

fn statusbar_detail(app: &TuiApp) -> String {
    let status_line = app.status_line.trim();
    let detail = (!status_line.is_empty())
        .then(|| status_line.to_string())
        .or_else(|| app.connection_detail().map(ToString::to_string))
        .or_else(|| overlay_hint(app).map(ToString::to_string))
        .or_else(|| active_tasks_hint(app))
        .unwrap_or_else(|| "Type / for commands · /help for shortcuts".into());
    format!("{} · {}", app.connection_label(), detail)
}

fn overlay_hint(app: &TuiApp) -> Option<&'static str> {
    let slash_visible = matches!(app.overlay, OverlayState::None)
        && !super::render::slash_menu_lines(app).is_empty();
    if slash_visible {
        return Some(status_hint(KeyContext::SlashMenu, true));
    }
    let context = match app.overlay {
        OverlayState::None => return None,
        OverlayState::Agents => KeyContext::AgentsOverlay,
        OverlayState::Events { .. } => KeyContext::EventsOverlay,
        OverlayState::Transcript { .. }
        | OverlayState::AgentState { .. }
        | OverlayState::DebugPromptView { .. }
        | OverlayState::HelpView { .. } => KeyContext::ScrollOverlay,
        OverlayState::Tasks { .. } => KeyContext::TasksOverlay,
        OverlayState::ModelPicker { .. } => KeyContext::ModelPicker,
        OverlayState::ModelEffortPicker { .. } => KeyContext::ModelEffortPicker,
        OverlayState::DebugPromptInput { .. } => KeyContext::DebugPromptInput,
    };
    Some(status_hint(context, false))
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
    use super::{shorten_home_path_with_home, workspace_label};
    use crate::types::AGENT_HOME_WORKSPACE_ID;
    use std::path::Path;

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
