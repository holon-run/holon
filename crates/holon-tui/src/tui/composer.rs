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

    pub(super) fn is_cursor_at_end(&self) -> bool {
        self.cursor >= self.text.len()
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub(super) fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
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

    pub(super) fn move_up(&mut self) {
        let line_start = self.current_line_start();
        if line_start == 0 {
            return;
        }

        let previous_line_end = line_start - 1;
        let previous_line_start = self.text[..previous_line_end]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.cursor = self.line_index_at_column(
            previous_line_start,
            previous_line_end,
            self.current_line_column(),
        );
    }

    pub(super) fn move_down(&mut self) {
        let line_end = self.current_line_end();
        if line_end == self.text.len() {
            return;
        }

        let next_line_start = line_end + 1;
        let next_line_end = self.text[next_line_start..]
            .find('\n')
            .map(|index| next_line_start + index)
            .unwrap_or(self.text.len());
        self.cursor =
            self.line_index_at_column(next_line_start, next_line_end, self.current_line_column());
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

    pub(super) fn clamp_cursor_to_normal_position(&mut self) {
        if self.text.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.text.len() {
            self.cursor = self
                .previous_boundary(self.text.len())
                .unwrap_or(self.text.len());
        }
    }

    pub(super) fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn move_to_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub(super) fn delete_to_end(&mut self) {
        let end = self.current_line_end();
        self.text.truncate(end);
        if self.cursor > end {
            self.cursor = end;
        }
    }

    pub(super) fn delete_to_line_end(&mut self) {
        let end = self.current_line_end();
        self.text.drain(self.cursor..end);
    }

    pub(super) fn delete_to_start(&mut self) {
        let start = self.current_line_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub(super) fn delete_word(&mut self) {
        let start = self.find_word_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub(super) fn move_word_forward(&mut self) {
        let chars = self.char_ranges();
        let Some(mut index) = chars
            .iter()
            .position(|(start, end, _)| self.cursor >= *start && self.cursor < *end)
            .or_else(|| chars.iter().position(|(start, _, _)| *start >= self.cursor))
        else {
            self.cursor = self.text.len();
            return;
        };

        if !chars[index].2.is_whitespace() {
            while index < chars.len() && !chars[index].2.is_whitespace() {
                index += 1;
            }
        }
        while index < chars.len() && chars[index].2.is_whitespace() {
            index += 1;
        }

        self.cursor = chars
            .get(index)
            .map(|(start, _, _)| *start)
            .unwrap_or(self.text.len());
        self.clamp_cursor_to_normal_position();
    }

    pub(super) fn move_word_backward(&mut self) {
        let chars = self.char_ranges();
        let Some(mut index) = chars.iter().rposition(|(start, _, _)| *start < self.cursor) else {
            self.cursor = 0;
            return;
        };

        while index > 0 && chars[index].2.is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].2.is_whitespace() {
            index -= 1;
        }
        self.cursor = chars[index].0;
    }

    pub(super) fn move_word_end(&mut self) {
        let chars = self.char_ranges();
        let Some(mut index) = chars
            .iter()
            .position(|(start, end, _)| self.cursor >= *start && self.cursor < *end)
            .or_else(|| chars.iter().position(|(start, _, _)| *start >= self.cursor))
        else {
            self.clamp_cursor_to_normal_position();
            return;
        };

        while index < chars.len() && chars[index].2.is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            self.clamp_cursor_to_normal_position();
            return;
        }
        while index + 1 < chars.len() && !chars[index + 1].2.is_whitespace() {
            index += 1;
        }
        self.cursor = chars[index].0;
    }

    pub(super) fn delete_current_line(&mut self) {
        if self.text.is_empty() {
            return;
        }

        let start = self.current_line_start();
        let line_end = self.current_line_end();
        if line_end < self.text.len() {
            self.text.drain(start..line_end + 1);
            self.cursor = start.min(self.text.len());
        } else if start > 0 {
            self.text.drain(start - 1..line_end);
            self.cursor = (start - 1).min(self.text.len());
        } else {
            self.clear();
        }
        self.clamp_cursor_to_normal_position();
    }

    pub(super) fn open_line_below(&mut self) {
        self.cursor = self.current_line_end();
        self.insert_char('\n');
    }

    pub(super) fn open_line_above(&mut self) {
        self.cursor = self.current_line_start();
        self.insert_char('\n');
        self.move_left();
    }

    pub(super) fn delete_word_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }

        let end = self
            .text
            .char_indices()
            .skip_while(|(index, _)| *index < self.cursor)
            .skip(1)
            .find_map(|(index, ch)| {
                let current = self.text[self.cursor..].chars().next().unwrap_or(ch);
                (current.is_whitespace() != ch.is_whitespace()).then_some(index)
            })
            .unwrap_or(self.text.len());
        self.text.drain(self.cursor..end);
        self.clamp_cursor_to_normal_position();
    }

    fn find_word_start(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed_end = before.trim_end().len();
        let word_end = if trimmed_end == 0 {
            before.len()
        } else {
            trimmed_end
        };
        before[..word_end]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| {
                // Find the byte after the whitespace character
                i + before[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1)
            })
            .unwrap_or(0)
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

    pub(super) fn current_line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    pub(super) fn current_line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.text.len())
    }

    fn current_line_column(&self) -> usize {
        self.text[self.current_line_start()..self.cursor]
            .chars()
            .count()
    }

    fn line_index_at_column(&self, line_start: usize, line_end: usize, column: usize) -> usize {
        let mut index = line_start;
        for (count, (offset, ch)) in self.text[line_start..line_end].char_indices().enumerate() {
            if count == column {
                return line_start + offset;
            }
            index = line_start + offset + ch.len_utf8();
        }
        index
    }

    fn char_ranges(&self) -> Vec<(usize, usize, char)> {
        self.text
            .char_indices()
            .map(|(start, ch)| (start, start + ch.len_utf8(), ch))
            .collect()
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
mod tests;
