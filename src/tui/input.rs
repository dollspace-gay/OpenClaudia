//! Text input widget for the TUI.

/// Single-line text input with cursor tracking.
pub struct TextInput {
    pub content: String,
    pub cursor_pos: usize,
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
}
