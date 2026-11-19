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
fn overlay_shortcut_keys_resolve_to_explicit_targets() {
    assert_eq!(
        resolve_key(KeyContext::Main, ctrl('o')),
        TuiKeyAction::BeginOverlayShortcut
    );
    assert_eq!(
        resolve_overlay_shortcut_key(key(KeyCode::Char('a'))),
        TuiKeyAction::OpenOverlay(OverlayShortcutTarget::Agents)
    );
    assert_eq!(
        resolve_overlay_shortcut_key(key(KeyCode::Char('K'))),
        TuiKeyAction::OpenOverlay(OverlayShortcutTarget::SelectedAgentSkills)
    );
    assert_eq!(
        resolve_overlay_shortcut_key(key(KeyCode::Esc)),
        TuiKeyAction::OverlayClose
    );
    assert_eq!(
        resolve_overlay_shortcut_key(ctrl('a')),
        TuiKeyAction::Ignore
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
