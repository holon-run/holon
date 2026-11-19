use super::ComposerState;

#[test]
fn inserts_and_moves_inside_existing_text() {
    let mut composer = ComposerState::from("hlo");
    composer.move_left();
    composer.move_left();
    composer.insert_char('e');
    assert_eq!(composer.as_str(), "helo");
    assert_eq!(composer.cursor(), 2);
}

#[test]
fn inserts_pasted_text_at_cursor() {
    let mut composer = ComposerState::from("ab");
    composer.move_left();
    composer.insert_str("x\ny");
    assert_eq!(composer.as_str(), "ax\nyb");
    assert_eq!(composer.cursor(), "ax\ny".len());
}

#[test]
fn backspace_and_delete_respect_utf8_boundaries() {
    let mut composer = ComposerState::from("你好吗");
    composer.move_left();
    composer.backspace();
    assert_eq!(composer.as_str(), "你吗");
    composer.move_home();
    composer.delete();
    assert_eq!(composer.as_str(), "吗");
}

#[test]
fn home_and_end_stay_within_current_line() {
    let mut composer = ComposerState::from("first\nsecond\nthird");
    composer.move_home();
    assert_eq!(composer.cursor(), "first\nsecond\n".len());
    composer.move_left();
    composer.move_left();
    composer.move_home();
    assert_eq!(composer.cursor(), "first\n".len());
    composer.move_end();
    assert_eq!(composer.cursor(), "first\nsecond".len());
}

#[test]
fn up_and_down_move_between_lines_at_matching_columns() {
    let mut composer = ComposerState::from("alpha\nbeta\ncharlie");
    composer.move_to_start();
    for _ in 0..8 {
        composer.move_right();
    }

    composer.move_up();
    assert_eq!(composer.cursor(), 2);
    composer.move_down();
    assert_eq!(composer.cursor(), "alpha\nbe".len());
    composer.move_down();
    assert_eq!(composer.cursor(), "alpha\nbeta\nch".len());
}

#[test]
fn up_and_down_clamp_to_shorter_lines_and_noop_at_edges() {
    let mut composer = ComposerState::from("long\nx\nwide");
    composer.move_to_start();
    for _ in 0..3 {
        composer.move_right();
    }
    composer.move_up();
    assert_eq!(composer.cursor(), 3);

    composer.move_down();
    assert_eq!(composer.cursor(), "long\nx".len());
    composer.move_down();
    assert_eq!(composer.cursor(), "long\nx\nw".len());
    composer.move_down();
    assert_eq!(composer.cursor(), "long\nx\nw".len());
}

#[test]
fn up_and_down_respect_utf8_columns() {
    let mut composer = ComposerState::from("你好\n世界");
    composer.move_to_start();
    composer.move_right();
    composer.move_down();
    assert_eq!(composer.cursor(), "你好\n世".len());
    composer.move_up();
    assert_eq!(composer.cursor(), "你".len());
}
