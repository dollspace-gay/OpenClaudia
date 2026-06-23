//! Text input widget for the TUI.

/// Text input with cursor tracking.
pub struct TextInput {
    pub content: String,
    cursor_pos: usize,
}

impl TextInput {
    /// Current cursor position (byte offset into content).
    #[must_use]
    pub const fn cursor_position(&self) -> usize {
        self.cursor_pos
    }
}

impl TextInput {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            content: String::new(),
            cursor_pos: 0,
        }
    }

    pub fn insert(&mut self, ch: char) {
        self.content.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    pub fn insert_str(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\r' {
                if matches!(chars.peek(), Some('\n')) {
                    let _ = chars.next();
                }
                self.insert_newline();
            } else {
                self.insert(ch);
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.content[..self.cursor_pos]
                .chars()
                .last()
                .map_or(1, char::len_utf8);
            self.cursor_pos -= prev;
            self.content.remove(self.cursor_pos);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor_pos < self.content.len() {
            self.content.remove(self.cursor_pos);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.content[..self.cursor_pos]
                .chars()
                .last()
                .map_or(1, char::len_utf8);
            self.cursor_pos -= prev;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.content.len() {
            let next = self.content[self.cursor_pos..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
            self.cursor_pos += next;
        }
    }

    pub const fn home(&mut self) {
        self.cursor_pos = 0;
    }

    pub const fn end(&mut self) {
        self.cursor_pos = self.content.len();
    }

    #[must_use]
    pub fn visual_line_count(&self, content_width: u16) -> u16 {
        let width = usize::from(content_width.max(1));
        let rows = self
            .content
            .split('\n')
            .map(|line| wrapped_rows(line.chars().count(), width))
            .sum::<usize>();
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    #[must_use]
    pub fn visual_cursor_position(&self, content_width: u16) -> (u16, u16) {
        let width = usize::from(content_width.max(1));
        let before_cursor = &self.content[..self.cursor_pos];
        let mut row = 0usize;
        let mut lines = before_cursor.split('\n').peekable();

        while let Some(line) = lines.next() {
            let col = line.chars().count();
            if lines.peek().is_some() {
                row = row.saturating_add(wrapped_rows(col, width));
            } else {
                row = row.saturating_add(col / width);
                let col = col % width;
                return (
                    u16::try_from(row).unwrap_or(u16::MAX),
                    u16::try_from(col).unwrap_or(u16::MAX),
                );
            }
        }

        (0, 0)
    }

    /// Take the content and reset.
    pub fn take(&mut self) -> String {
        let s = std::mem::take(&mut self.content);
        self.cursor_pos = 0;
        s
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.content.is_empty()
    }
}

const fn wrapped_rows(char_count: usize, width: usize) -> usize {
    if char_count == 0 {
        1
    } else {
        char_count.saturating_add(width - 1) / width
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_take() {
        let mut input = TextInput::new();
        input.insert('h');
        input.insert('i');
        assert_eq!(input.content, "hi");
        assert_eq!(input.cursor_pos, 2);
        let taken = input.take();
        assert_eq!(taken, "hi");
        assert!(input.is_empty());
    }

    #[test]
    fn test_backspace() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.backspace();
        assert_eq!(input.content, "a");
        assert_eq!(input.cursor_pos, 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.home();
        assert_eq!(input.cursor_pos, 0);
        input.end();
        assert_eq!(input.cursor_pos, 3);
        input.move_left();
        assert_eq!(input.cursor_pos, 2);
        input.move_right();
        assert_eq!(input.cursor_pos, 3);
    }

    #[test]
    fn test_delete() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.home();
        input.delete();
        assert_eq!(input.content, "b");
    }

    #[test]
    fn test_insert_multiline_text_normalizes_crlf() {
        let mut input = TextInput::new();
        input.insert_str("a\r\nb\rc");
        assert_eq!(input.content, "a\nb\nc");
        assert_eq!(input.cursor_pos, input.content.len());
    }

    #[test]
    fn test_visual_cursor_position_tracks_newlines() {
        let mut input = TextInput::new();
        input.insert_str("abc\nde");

        assert_eq!(input.visual_line_count(10), 2);
        assert_eq!(input.visual_cursor_position(10), (1, 2));
    }

    #[test]
    fn test_visual_line_count_accounts_for_wrapping() {
        let mut input = TextInput::new();
        input.insert_str("abcd\nef");

        assert_eq!(input.visual_line_count(3), 3);
        assert_eq!(input.visual_cursor_position(3), (2, 2));
    }
}
