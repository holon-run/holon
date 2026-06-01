use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::render::TaskOverlayAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyContext {
    Global,
    Main,
    Composer,
    SlashMenu,
    AgentsOverlay,
    EventsOverlay,
    ScrollOverlay,
    TasksOverlay,
    ModelPicker,
    ModelEffortPicker,
    DebugPromptInput,
}

impl KeyContext {
    pub(super) const fn label(self) -> &'static str {
        match self {
            KeyContext::Global => "global",
            KeyContext::Main => "main",
            KeyContext::Composer => "composer",
            KeyContext::SlashMenu => "slash menu",
            KeyContext::AgentsOverlay => "agents overlay",
            KeyContext::EventsOverlay => "events overlay",
            KeyContext::ScrollOverlay => "scroll overlays",
            KeyContext::TasksOverlay => "tasks overlay",
            KeyContext::ModelPicker => "model picker",
            KeyContext::ModelEffortPicker => "reasoning effort picker",
            KeyContext::DebugPromptInput => "debug prompt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TuiKeyAction {
    Quit,
    OpenHelp,
    HistoryPrevious,
    HistoryNext,
    ChatScroll(ScrollAction),
    Composer(ComposerAction),
    SlashMenu(SlashMenuAction),
    OverlayClose,
    OverlayAccept,
    OverlayMoveUp,
    OverlayMoveDown,
    OverlayScroll(ScrollAction),
    Task(TaskOverlayAction),
    ModelFilterBackspace,
    InsertChar(char),
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerAction {
    Submit,
    Cancel,
    InsertNewline,
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveHome,
    MoveEnd,
    InsertTab,
    MoveToStart,
    MoveToEnd,
    DeleteToEnd,
    DeleteToStart,
    DeleteWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashMenuAction {
    Dismiss,
    Previous,
    Next,
    Complete,
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DefaultBindingHint {
    pub(super) context: KeyContext,
    pub(super) action: &'static str,
    pub(super) keys: &'static str,
}

pub(super) const DEFAULT_BINDING_HINTS: &[DefaultBindingHint] = &[
    DefaultBindingHint {
        context: KeyContext::Global,
        action: "quit",
        keys: "Ctrl+C",
    },
    DefaultBindingHint {
        context: KeyContext::Main,
        action: "help when composer is empty",
        keys: "?",
    },
    DefaultBindingHint {
        context: KeyContext::Main,
        action: "history when composer is empty",
        keys: "Up/Down",
    },
    DefaultBindingHint {
        context: KeyContext::Main,
        action: "move cursor when composer has content",
        keys: "Up/Down",
    },
    DefaultBindingHint {
        context: KeyContext::Main,
        action: "scroll chat",
        keys: "PgUp/PgDn",
    },
    DefaultBindingHint {
        context: KeyContext::Composer,
        action: "submit",
        keys: "Enter",
    },
    DefaultBindingHint {
        context: KeyContext::Composer,
        action: "newline",
        keys: "Shift+Enter",
    },
    DefaultBindingHint {
        context: KeyContext::Composer,
        action: "readline edit",
        keys: "Ctrl+A/E/B/F/K/U/W/H/D",
    },
    DefaultBindingHint {
        context: KeyContext::SlashMenu,
        action: "select/complete/run/close",
        keys: "Up/Down Tab Enter Esc",
    },
    DefaultBindingHint {
        context: KeyContext::AgentsOverlay,
        action: "select/close",
        keys: "Up/Down Enter Esc",
    },
    DefaultBindingHint {
        context: KeyContext::TasksOverlay,
        action: "select/actions/close",
        keys: "Up/Down f/l/x/i Esc",
    },
    DefaultBindingHint {
        context: KeyContext::ScrollOverlay,
        action: "scroll/close",
        keys: "Up/Down PgUp/PgDn Home/End Esc",
    },
];

pub(super) fn resolve_key(context: KeyContext, key: KeyEvent) -> TuiKeyAction {
    match context {
        KeyContext::Global => resolve_global_key(key),
        KeyContext::Main => resolve_main_key(key),
        KeyContext::Composer | KeyContext::DebugPromptInput => resolve_composer_key(key),
        KeyContext::SlashMenu => resolve_slash_menu_key(key),
        KeyContext::AgentsOverlay => resolve_list_overlay_key(key),
        KeyContext::EventsOverlay => resolve_events_overlay_key(key),
        KeyContext::ScrollOverlay => resolve_scroll_overlay_key(key),
        KeyContext::TasksOverlay => resolve_tasks_overlay_key(key),
        KeyContext::ModelPicker => resolve_model_picker_key(key),
        KeyContext::ModelEffortPicker => resolve_list_overlay_key(key),
    }
}

pub(super) fn status_hint(context: KeyContext, slash_menu_visible: bool) -> &'static str {
    if slash_menu_visible {
        return "Slash: Up/Down select  Tab complete  Enter run  Esc close";
    }
    match context {
        KeyContext::Global | KeyContext::Main | KeyContext::Composer => {
            "/help commands  /state agent state  /transcript  PgUp/PgDn scroll  Ctrl+A/E edit  Ctrl+C quit"
        }
        KeyContext::SlashMenu => "Slash: Up/Down select  Tab complete  Enter run  Esc close",
        KeyContext::AgentsOverlay => "Agents: Up/Down, Enter select, Esc",
        KeyContext::EventsOverlay => "Events: Up/Down, PgUp/PgDn, Home/End, Esc",
        KeyContext::ScrollOverlay => "Up/Down, PgUp/PgDn, Home/End, Esc",
        KeyContext::TasksOverlay => "Tasks: Up/Down, PgUp/PgDn, Home/End, f/l/x/i actions, Esc",
        KeyContext::ModelPicker => {
            "Model: type filter, Backspace edit, Up/Down move, Enter select, Esc cancel"
        }
        KeyContext::ModelEffortPicker => "Reasoning effort: Up/Down move, Enter select, Esc back",
        KeyContext::DebugPromptInput => "Debug prompt: Enter confirm, Esc cancel",
    }
}

fn resolve_global_key(key: KeyEvent) -> TuiKeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        TuiKeyAction::Quit
    } else {
        TuiKeyAction::Ignore
    }
}

fn resolve_main_key(key: KeyEvent) -> TuiKeyAction {
    match key.code {
        KeyCode::Char('?') => TuiKeyAction::OpenHelp,
        KeyCode::Up => TuiKeyAction::HistoryPrevious,
        KeyCode::Down => TuiKeyAction::HistoryNext,
        KeyCode::PageUp => TuiKeyAction::ChatScroll(ScrollAction::PageUp),
        KeyCode::PageDown => TuiKeyAction::ChatScroll(ScrollAction::PageDown),
        _ => resolve_composer_key(key),
    }
}

fn resolve_composer_key(key: KeyEvent) -> TuiKeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('a') => TuiKeyAction::Composer(ComposerAction::MoveToStart),
            KeyCode::Char('e') => TuiKeyAction::Composer(ComposerAction::MoveToEnd),
            KeyCode::Char('b') => TuiKeyAction::Composer(ComposerAction::MoveLeft),
            KeyCode::Char('f') => TuiKeyAction::Composer(ComposerAction::MoveRight),
            KeyCode::Char('k') => TuiKeyAction::Composer(ComposerAction::DeleteToEnd),
            KeyCode::Char('u') => TuiKeyAction::Composer(ComposerAction::DeleteToStart),
            KeyCode::Char('w') => TuiKeyAction::Composer(ComposerAction::DeleteWord),
            KeyCode::Char('h') => TuiKeyAction::Composer(ComposerAction::Backspace),
            KeyCode::Char('d') => TuiKeyAction::Composer(ComposerAction::Delete),
            _ => TuiKeyAction::Ignore,
        };
    }

    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            TuiKeyAction::Composer(ComposerAction::InsertNewline)
        }
        KeyCode::Enter => TuiKeyAction::Composer(ComposerAction::Submit),
        KeyCode::Esc => TuiKeyAction::Composer(ComposerAction::Cancel),
        KeyCode::Backspace => TuiKeyAction::Composer(ComposerAction::Backspace),
        KeyCode::Delete => TuiKeyAction::Composer(ComposerAction::Delete),
        KeyCode::Left => TuiKeyAction::Composer(ComposerAction::MoveLeft),
        KeyCode::Right => TuiKeyAction::Composer(ComposerAction::MoveRight),
        KeyCode::Up => TuiKeyAction::Composer(ComposerAction::MoveUp),
        KeyCode::Down => TuiKeyAction::Composer(ComposerAction::MoveDown),
        KeyCode::Home => TuiKeyAction::Composer(ComposerAction::MoveHome),
        KeyCode::End => TuiKeyAction::Composer(ComposerAction::MoveEnd),
        KeyCode::Tab => TuiKeyAction::Composer(ComposerAction::InsertTab),
        KeyCode::Char(ch) => TuiKeyAction::InsertChar(ch),
        _ => TuiKeyAction::Ignore,
    }
}

fn resolve_slash_menu_key(key: KeyEvent) -> TuiKeyAction {
    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
        return TuiKeyAction::Ignore;
    }
    match key.code {
        KeyCode::Esc => TuiKeyAction::SlashMenu(SlashMenuAction::Dismiss),
        KeyCode::Up => TuiKeyAction::SlashMenu(SlashMenuAction::Previous),
        KeyCode::Down => TuiKeyAction::SlashMenu(SlashMenuAction::Next),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            TuiKeyAction::SlashMenu(SlashMenuAction::Previous)
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            TuiKeyAction::SlashMenu(SlashMenuAction::Next)
        }
        KeyCode::Tab => TuiKeyAction::SlashMenu(SlashMenuAction::Complete),
        KeyCode::Enter => TuiKeyAction::SlashMenu(SlashMenuAction::Submit),
        _ => TuiKeyAction::Ignore,
    }
}

fn resolve_list_overlay_key(key: KeyEvent) -> TuiKeyAction {
    match key.code {
        KeyCode::Esc => TuiKeyAction::OverlayClose,
        KeyCode::Enter => TuiKeyAction::OverlayAccept,
        KeyCode::Up | KeyCode::Char('k') => TuiKeyAction::OverlayMoveUp,
        KeyCode::Down | KeyCode::Char('j') => TuiKeyAction::OverlayMoveDown,
        _ => TuiKeyAction::Ignore,
    }
}

fn resolve_events_overlay_key(key: KeyEvent) -> TuiKeyAction {
    match key.code {
        KeyCode::Esc => TuiKeyAction::OverlayClose,
        KeyCode::Up => TuiKeyAction::OverlayMoveUp,
        KeyCode::Down => TuiKeyAction::OverlayMoveDown,
        _ => scroll_action_for_key(key.code)
            .map(TuiKeyAction::OverlayScroll)
            .unwrap_or(TuiKeyAction::Ignore),
    }
}

fn resolve_scroll_overlay_key(key: KeyEvent) -> TuiKeyAction {
    if key.code == KeyCode::Esc {
        return TuiKeyAction::OverlayClose;
    }
    scroll_action_for_key(key.code)
        .map(TuiKeyAction::OverlayScroll)
        .unwrap_or(TuiKeyAction::Ignore)
}

fn resolve_tasks_overlay_key(key: KeyEvent) -> TuiKeyAction {
    match key.code {
        KeyCode::Esc => TuiKeyAction::OverlayClose,
        KeyCode::Up => TuiKeyAction::OverlayMoveUp,
        KeyCode::Down => TuiKeyAction::OverlayMoveDown,
        KeyCode::Char('f') | KeyCode::Char('F') => {
            TuiKeyAction::Task(TaskOverlayAction::FullOutput)
        }
        KeyCode::Char('l') | KeyCode::Char('L') => {
            TuiKeyAction::Task(TaskOverlayAction::FollowOutput)
        }
        KeyCode::Char('x') | KeyCode::Char('X') => TuiKeyAction::Task(TaskOverlayAction::Stop),
        KeyCode::Char('i') | KeyCode::Char('I') => TuiKeyAction::Task(TaskOverlayAction::Input),
        _ => scroll_action_for_key(key.code)
            .map(TuiKeyAction::OverlayScroll)
            .unwrap_or(TuiKeyAction::Ignore),
    }
}

fn resolve_model_picker_key(key: KeyEvent) -> TuiKeyAction {
    match key.code {
        KeyCode::Esc => TuiKeyAction::OverlayClose,
        KeyCode::Enter => TuiKeyAction::OverlayAccept,
        KeyCode::Up | KeyCode::Char('k') => TuiKeyAction::OverlayMoveUp,
        KeyCode::Down | KeyCode::Char('j') => TuiKeyAction::OverlayMoveDown,
        KeyCode::Backspace => TuiKeyAction::ModelFilterBackspace,
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            TuiKeyAction::InsertChar(ch)
        }
        _ => TuiKeyAction::Ignore,
    }
}

pub(super) fn scroll_action_for_key(key: KeyCode) -> Option<ScrollAction> {
    match key {
        KeyCode::Up => Some(ScrollAction::Up),
        KeyCode::Down => Some(ScrollAction::Down),
        KeyCode::PageUp => Some(ScrollAction::PageUp),
        KeyCode::PageDown => Some(ScrollAction::PageDown),
        KeyCode::Home => Some(ScrollAction::Home),
        KeyCode::End => Some(ScrollAction::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn composer_readline_bindings_resolve_to_actions() {
        assert_eq!(
            resolve_key(KeyContext::Composer, ctrl('a')),
            TuiKeyAction::Composer(ComposerAction::MoveToStart)
        );
        assert_eq!(
            resolve_key(KeyContext::Composer, ctrl('k')),
            TuiKeyAction::Composer(ComposerAction::DeleteToEnd)
        );
        assert_eq!(
            resolve_key(KeyContext::Composer, key(KeyCode::Enter)),
            TuiKeyAction::Composer(ComposerAction::Submit)
        );
        assert_eq!(
            resolve_key(
                KeyContext::Composer,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
            ),
            TuiKeyAction::Composer(ComposerAction::InsertNewline)
        );
        assert_eq!(
            resolve_key(KeyContext::Composer, key(KeyCode::Up)),
            TuiKeyAction::Composer(ComposerAction::MoveUp)
        );
        assert_eq!(
            resolve_key(KeyContext::Composer, key(KeyCode::Down)),
            TuiKeyAction::Composer(ComposerAction::MoveDown)
        );
    }

    #[test]
    fn slash_menu_conflicts_resolve_inside_slash_context() {
        assert_eq!(
            resolve_key(KeyContext::SlashMenu, key(KeyCode::Enter)),
            TuiKeyAction::SlashMenu(SlashMenuAction::Submit)
        );
        assert_eq!(
            resolve_key(
                KeyContext::SlashMenu,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
            ),
            TuiKeyAction::Ignore
        );
        assert_eq!(
            resolve_key(KeyContext::SlashMenu, ctrl('n')),
            TuiKeyAction::SlashMenu(SlashMenuAction::Next)
        );
    }

    #[test]
    fn task_overlay_keys_resolve_to_selection_scroll_and_task_actions() {
        assert_eq!(
            resolve_key(KeyContext::TasksOverlay, key(KeyCode::Up)),
            TuiKeyAction::OverlayMoveUp
        );
        assert_eq!(
            resolve_key(KeyContext::TasksOverlay, key(KeyCode::PageDown)),
            TuiKeyAction::OverlayScroll(ScrollAction::PageDown)
        );
        assert_eq!(
            resolve_key(KeyContext::TasksOverlay, key(KeyCode::Char('x'))),
            TuiKeyAction::Task(TaskOverlayAction::Stop)
        );
    }

    #[test]
    fn default_binding_hints_cover_required_contexts() {
        assert!(DEFAULT_BINDING_HINTS
            .iter()
            .any(|hint| hint.context == KeyContext::Composer));
        assert!(DEFAULT_BINDING_HINTS
            .iter()
            .any(|hint| hint.context == KeyContext::SlashMenu));
        assert!(DEFAULT_BINDING_HINTS
            .iter()
            .any(|hint| hint.context == KeyContext::TasksOverlay));
    }
}
