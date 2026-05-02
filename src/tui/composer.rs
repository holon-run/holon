#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ComposerState {
    text: String,
    cursor: usize,
}

impl ComposerState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(super) fn move_left(&mut self) {
        if let Some(index) = self.previous_boundary(self.cursor) {
            self.cursor = index;
        }
    }

    pub(super) fn move_right(&mut self) {
        if let Some(index) = self.next_boundary(self.cursor) {
            self.cursor = index;
        }
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.current_line_start();
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.current_line_end();
    }

    pub(super) fn backspace(&mut self) {
        if let Some(start) = self.previous_boundary(self.cursor) {
            self.text.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    pub(super) fn delete(&mut self) {
        if let Some(end) = self.next_boundary(self.cursor) {
            self.text.drain(self.cursor..end);
        }
    }
    pub(super) fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn move_to_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub(super) fn delete_to_end(&mut self) {
        self.text.truncate(self.cursor);
    }

    pub(super) fn delete_to_start(&mut self) {
        self.text.drain(0..self.cursor);
        self.cursor = 0;
    }

    pub(super) fn delete_word(&mut self) {
        let start = self.find_word_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    fn find_word_start(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed_end = before.trim_end().len();
        let word_end = if trimmed_end == 0 {
            before.len()
        } else {
            trimmed_end
        };
        before[..word_end].rfind(' ').map(|i| i + 1).unwrap_or(0)
    }

    fn previous_boundary(&self, from: usize) -> Option<usize> {
        self.text[..from]
            .char_indices()
            .last()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self, from: usize) -> Option<usize> {
        if from >= self.text.len() {
            return None;
        }
        self.text[from..]
            .chars()
            .next()
            .map(|ch| from + ch.len_utf8())
    }

    fn current_line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn current_line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.text.len())
    }
}

impl From<&str> for ComposerState {
    fn from(value: &str) -> Self {
        Self {
            text: value.to_string(),
            cursor: value.len(),
        }
    }
}

impl From<String> for ComposerState {
    fn from(value: String) -> Self {
        let cursor = value.len();
        Self {
            text: value,
            cursor,
        }
    }
}

#[cfg(test)]
mod tests {
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
}
