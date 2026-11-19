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

#[test]
fn move_to_start_moves_cursor_to_beginning() {
    let mut composer = ComposerState::from("some text");
    composer.move_to_start();
    assert_eq!(composer.cursor(), 0);
}

#[test]
fn move_to_end_moves_cursor_to_end() {
    let mut composer = ComposerState::from("some text");
    composer.move_to_start();
    composer.move_to_end();
    assert_eq!(composer.cursor(), 9);
}

#[test]
fn delete_to_end_deletes_from_cursor_to_line_end() {
    let mut composer = ComposerState::from("first line\nsecond line");
    // Move to start of "first line"
    composer.move_to_start();
    // Move to position after "first"
    for _ in 0..5 {
        composer.move_right();
    }
    composer.delete_to_end();
    assert_eq!(composer.as_str(), "first line");
    assert_eq!(composer.cursor(), 5);
}

#[test]
fn delete_to_start_deletes_from_line_start_to_cursor() {
    let mut composer = ComposerState::from("first line\nsecond line");
    // Move to start of "first line"
    composer.move_to_start();
    // Move to position after "first "
    for _ in 0..6 {
        composer.move_right();
    }
    composer.delete_to_start();
    assert_eq!(composer.as_str(), "line\nsecond line");
    assert_eq!(composer.cursor(), 0);
}

#[test]
fn delete_to_end_on_last_line_deletes_to_end_of_buffer() {
    let mut composer = ComposerState::from("first\nsecond");
    // Move to start of "second"
    composer.move_to_start();
    for _ in 0..7 {
        composer.move_right();
    }
    composer.delete_to_end();
    assert_eq!(composer.as_str(), "first\nsecond");
    // Cursor should be at end since there's no newline after
    assert_eq!(composer.cursor(), 7);
}

#[test]
fn delete_word_deletes_previous_word() {
    let mut composer = ComposerState::from("hello world test");
    composer.delete_word();
    assert_eq!(composer.as_str(), "hello world ");
    assert_eq!(composer.cursor(), 12);
}

#[test]
fn delete_word_handles_multiple_spaces() {
    let mut composer = ComposerState::from("hello   world");
    composer.delete_word();
    assert_eq!(composer.as_str(), "hello   ");
    assert_eq!(composer.cursor(), 8);
}

#[test]
fn delete_word_handles_tabs_and_newlines() {
    let mut composer = ComposerState::from("hello\tworld\nthere");
    composer.delete_word();
    // Should delete "there" and stop at whitespace
    assert_eq!(composer.as_str(), "hello\tworld\n");
    assert_eq!(composer.cursor(), 12);
}

#[test]
fn vim_word_motions_respect_utf8_boundaries() {
    let mut composer = ComposerState::from("你 好 world");
    composer.move_to_start();

    composer.move_word_forward();
    assert_eq!(composer.cursor(), "你 ".len());

    composer.move_word_forward();
    assert_eq!(composer.cursor(), "你 好 ".len());

    composer.move_word_end();
    assert_eq!(composer.cursor(), "你 好 worl".len());

    composer.move_word_backward();
    assert_eq!(composer.cursor(), "你 好 ".len());
}

#[test]
fn vim_line_and_word_edits_stay_inside_current_line() {
    let mut composer = ComposerState::from("alpha beta\ngamma");
    composer.move_to_start();
    for _ in 0..6 {
        composer.move_right();
    }

    composer.delete_word_forward();
    assert_eq!(composer.as_str(), "alpha \ngamma");

    composer.insert_str("beta");
    composer.move_home();
    composer.delete_to_line_end();
    assert_eq!(composer.as_str(), "\ngamma");

    composer.delete_current_line();
    assert_eq!(composer.as_str(), "gamma");
}

#[test]
fn vim_open_line_above_and_below_place_cursor_on_new_line() {
    let mut composer = ComposerState::from("alpha\nbeta");
    composer.move_to_start();
    composer.open_line_above();
    assert_eq!(composer.as_str(), "\nalpha\nbeta");
    assert_eq!(composer.cursor(), 0);

    composer.move_to_end();
    composer.open_line_below();
    assert_eq!(composer.as_str(), "\nalpha\nbeta\n");
    assert_eq!(composer.cursor(), "\nalpha\nbeta\n".len());
}
