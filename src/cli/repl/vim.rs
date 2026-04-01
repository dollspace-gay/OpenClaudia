//! Vim mode state machine for terminal input.
//!
//! Implements normal and insert modes with basic motions, operators,
//! and text objects following the pattern from Claude Code's design doc.

/// Current vim mode
#[derive(Debug, Clone, PartialEq)]
pub enum VimMode {
    /// Insert mode — characters go directly into the buffer
    Insert,
    /// Normal mode — keys are commands
    Normal,
}

/// State for normal mode command parsing
#[derive(Debug, Clone, PartialEq)]
pub enum CommandState {
    /// Waiting for first key
    Idle,
    /// Accumulating count prefix (e.g., "3" in "3w")
    Count { digits: String },
    /// Waiting for motion after operator (e.g., "d" waiting for "w")
    Operator { op: Operator, count: u32 },
    /// Waiting for char after f/F/t/T
    Find { find_type: FindType, count: u32 },
    /// Waiting for replacement char after r
    Replace { count: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operator {
    Delete, // d
    Change, // c
    Yank,   // y
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindType {
    Forward,      // f
    ForwardTill,  // t
    Backward,     // F
    BackwardTill, // T
}

/// Action to perform on the text buffer
#[derive(Debug, Clone, PartialEq)]
pub enum VimAction {
    /// No action (key consumed but no effect)
    None,
    /// Switch to insert mode
    EnterInsert,
    /// Switch to insert mode at end of line
    EnterInsertAppend,
    /// Switch to insert mode at start of line
    EnterInsertLineStart,
    /// Switch to insert mode on new line below
    OpenLineBelow,
    /// Switch to insert mode on new line above
    OpenLineAbove,
    /// Switch to normal mode
    EnterNormal,
    /// Move cursor
    MoveCursor(CursorMove),
    /// Delete text range
    Delete(TextRange),
    /// Change text range (delete + enter insert)
    Change(TextRange),
    /// Yank text range
    Yank(TextRange),
    /// Delete char under cursor
    DeleteChar,
    /// Delete to end of line
    DeleteToEnd,
    /// Change to end of line
    ChangeToEnd,
    /// Yank entire line
    YankLine,
    /// Paste after cursor
    PasteAfter,
    /// Paste before cursor
    PasteBefore,
    /// Undo
    Undo,
    /// Submit the line (like Enter in insert mode)
    Submit,
}

/// How to move the cursor
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorMove {
    Left(u32),
    Right(u32),
    WordForward(u32),
    WordBackward(u32),
    WordEnd(u32),
    LineStart,
    LineEnd,
    FirstNonBlank,
}

/// A range of text to operate on
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextRange {
    Motion(CursorMove),
    Line,
    InnerWord,
    AWord,
}

/// The vim state machine
pub struct VimState {
    pub mode: VimMode,
    pub command: CommandState,
    pub yank_buffer: String,
    pub last_find: Option<(FindType, char)>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            mode: VimMode::Insert, // Start in insert mode like most editors
            command: CommandState::Idle,
            yank_buffer: String::new(),
            last_find: None,
        }
    }
}

impl VimState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a key input and return the action to perform.
    /// Returns None if the key should be passed through (insert mode regular chars).
    pub fn process_key(&mut self, key: &str) -> Option<VimAction> {
        match &self.mode {
            VimMode::Insert => self.process_insert(key),
            VimMode::Normal => Some(self.process_normal(key)),
        }
    }

    fn process_insert(&mut self, key: &str) -> Option<VimAction> {
        match key {
            "Escape" => {
                self.mode = VimMode::Normal;
                self.command = CommandState::Idle;
                Some(VimAction::EnterNormal)
            }
            _ => None, // Pass through to regular input
        }
    }

    fn process_normal(&mut self, key: &str) -> VimAction {
        match &self.command {
            CommandState::Idle => self.process_idle(key),
            CommandState::Count { digits } => {
                let digits = digits.clone();
                self.process_count(key, &digits)
            }
            CommandState::Operator { op, count } => {
                let op = *op;
                let count = *count;
                self.process_operator(key, op, count)
            }
            CommandState::Find { find_type, count } => {
                let ft = *find_type;
                let count = *count;
                self.process_find(key, ft, count)
            }
            CommandState::Replace { count } => {
                let count = *count;
                self.process_replace(key, count)
            }
        }
    }

    fn process_idle(&mut self, key: &str) -> VimAction {
        match key {
            // Mode switching
            "i" => {
                self.mode = VimMode::Insert;
                VimAction::EnterInsert
            }
            "a" => {
                self.mode = VimMode::Insert;
                VimAction::EnterInsertAppend
            }
            "I" => {
                self.mode = VimMode::Insert;
                VimAction::EnterInsertLineStart
            }
            "A" => {
                self.mode = VimMode::Insert;
                VimAction::EnterInsertAppend
            }
            "o" => {
                self.mode = VimMode::Insert;
                VimAction::OpenLineBelow
            }
            "O" => {
                self.mode = VimMode::Insert;
                VimAction::OpenLineAbove
            }

            // Basic motions
            "h" => VimAction::MoveCursor(CursorMove::Left(1)),
            "l" => VimAction::MoveCursor(CursorMove::Right(1)),
            "w" => VimAction::MoveCursor(CursorMove::WordForward(1)),
            "b" => VimAction::MoveCursor(CursorMove::WordBackward(1)),
            "e" => VimAction::MoveCursor(CursorMove::WordEnd(1)),
            "0" => VimAction::MoveCursor(CursorMove::LineStart),
            "$" => VimAction::MoveCursor(CursorMove::LineEnd),
            "^" => VimAction::MoveCursor(CursorMove::FirstNonBlank),

            // Operators (wait for motion)
            "d" => {
                self.command = CommandState::Operator {
                    op: Operator::Delete,
                    count: 1,
                };
                VimAction::None
            }
            "c" => {
                self.command = CommandState::Operator {
                    op: Operator::Change,
                    count: 1,
                };
                VimAction::None
            }
            "y" => {
                self.command = CommandState::Operator {
                    op: Operator::Yank,
                    count: 1,
                };
                VimAction::None
            }

            // Shorthand commands
            "x" => VimAction::DeleteChar,
            "D" => VimAction::DeleteToEnd,
            "C" => {
                self.mode = VimMode::Insert;
                VimAction::ChangeToEnd
            }
            "Y" => VimAction::YankLine,
            "p" => VimAction::PasteAfter,
            "P" => VimAction::PasteBefore,
            "u" => VimAction::Undo,

            // Find
            "f" => {
                self.command = CommandState::Find {
                    find_type: FindType::Forward,
                    count: 1,
                };
                VimAction::None
            }
            "F" => {
                self.command = CommandState::Find {
                    find_type: FindType::Backward,
                    count: 1,
                };
                VimAction::None
            }
            "t" => {
                self.command = CommandState::Find {
                    find_type: FindType::ForwardTill,
                    count: 1,
                };
                VimAction::None
            }
            "T" => {
                self.command = CommandState::Find {
                    find_type: FindType::BackwardTill,
                    count: 1,
                };
                VimAction::None
            }

            // Replace single char
            "r" => {
                self.command = CommandState::Replace { count: 1 };
                VimAction::None
            }

            // Count prefix
            k if k.len() == 1
                && k.chars()
                    .next()
                    .map_or(false, |c| c.is_ascii_digit() && c != '0') =>
            {
                self.command = CommandState::Count {
                    digits: k.to_string(),
                };
                VimAction::None
            }

            // Enter submits
            "Enter" => VimAction::Submit,

            _ => VimAction::None,
        }
    }

    fn process_count(&mut self, key: &str, digits: &str) -> VimAction {
        if key.len() == 1 && key.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            let mut new_digits = digits.to_string();
            new_digits.push_str(key);
            self.command = CommandState::Count {
                digits: new_digits,
            };
            return VimAction::None;
        }

        let count = digits.parse::<u32>().unwrap_or(1);
        self.command = CommandState::Idle;

        match key {
            "h" => VimAction::MoveCursor(CursorMove::Left(count)),
            "l" => VimAction::MoveCursor(CursorMove::Right(count)),
            "w" => VimAction::MoveCursor(CursorMove::WordForward(count)),
            "b" => VimAction::MoveCursor(CursorMove::WordBackward(count)),
            "e" => VimAction::MoveCursor(CursorMove::WordEnd(count)),
            "x" => VimAction::DeleteChar, // Could repeat
            "d" => {
                self.command = CommandState::Operator {
                    op: Operator::Delete,
                    count,
                };
                VimAction::None
            }
            "c" => {
                self.command = CommandState::Operator {
                    op: Operator::Change,
                    count,
                };
                VimAction::None
            }
            "y" => {
                self.command = CommandState::Operator {
                    op: Operator::Yank,
                    count,
                };
                VimAction::None
            }
            "f" => {
                self.command = CommandState::Find {
                    find_type: FindType::Forward,
                    count,
                };
                VimAction::None
            }
            "F" => {
                self.command = CommandState::Find {
                    find_type: FindType::Backward,
                    count,
                };
                VimAction::None
            }
            "t" => {
                self.command = CommandState::Find {
                    find_type: FindType::ForwardTill,
                    count,
                };
                VimAction::None
            }
            "T" => {
                self.command = CommandState::Find {
                    find_type: FindType::BackwardTill,
                    count,
                };
                VimAction::None
            }
            _ => VimAction::None,
        }
    }

    fn process_operator(&mut self, key: &str, op: Operator, count: u32) -> VimAction {
        self.command = CommandState::Idle;

        let motion = match key {
            "w" => Some(TextRange::Motion(CursorMove::WordForward(count))),
            "b" => Some(TextRange::Motion(CursorMove::WordBackward(count))),
            "e" => Some(TextRange::Motion(CursorMove::WordEnd(count))),
            "$" => Some(TextRange::Motion(CursorMove::LineEnd)),
            "0" => Some(TextRange::Motion(CursorMove::LineStart)),
            "^" => Some(TextRange::Motion(CursorMove::FirstNonBlank)),
            // dd, cc, yy — operate on whole line
            "d" if op == Operator::Delete => Some(TextRange::Line),
            "c" if op == Operator::Change => Some(TextRange::Line),
            "y" if op == Operator::Yank => Some(TextRange::Line),
            // iw, aw — text objects
            "i" => Some(TextRange::InnerWord),
            "a" => Some(TextRange::AWord),
            _ => None,
        };

        match (op, motion) {
            (Operator::Delete, Some(range)) => VimAction::Delete(range),
            (Operator::Change, Some(range)) => {
                self.mode = VimMode::Insert;
                VimAction::Change(range)
            }
            (Operator::Yank, Some(range)) => VimAction::Yank(range),
            _ => VimAction::None,
        }
    }

    fn process_find(&mut self, key: &str, find_type: FindType, _count: u32) -> VimAction {
        self.command = CommandState::Idle;
        if let Some(ch) = key.chars().next() {
            self.last_find = Some((find_type, ch));
            // The actual cursor movement would be handled by the editor
            // For now, return a motion action
            VimAction::None // Actual find motion TBD when wired to buffer
        } else {
            VimAction::None
        }
    }

    fn process_replace(&mut self, _key: &str, _count: u32) -> VimAction {
        self.command = CommandState::Idle;
        VimAction::None // Replacement TBD when wired to buffer
    }

    /// Whether we're currently in the middle of a multi-key command
    pub fn is_pending(&self) -> bool {
        !matches!(self.command, CommandState::Idle)
    }

    /// Get a display string for the current pending command
    pub fn pending_display(&self) -> &str {
        match &self.command {
            CommandState::Idle => "",
            CommandState::Count { .. } => "\u{2026}",
            CommandState::Operator {
                op: Operator::Delete,
                ..
            } => "d\u{2026}",
            CommandState::Operator {
                op: Operator::Change,
                ..
            } => "c\u{2026}",
            CommandState::Operator {
                op: Operator::Yank, ..
            } => "y\u{2026}",
            CommandState::Find { .. } => "f\u{2026}",
            CommandState::Replace { .. } => "r\u{2026}",
        }
    }
}

/// Get a description of the current vim state for the prompt.
pub fn status_description(state: &VimState) -> String {
    let mode_str = match &state.mode {
        VimMode::Insert => "INSERT",
        VimMode::Normal => "NORMAL",
    };
    let pending = match &state.command {
        CommandState::Idle => String::new(),
        CommandState::Count { digits } => format!(" {}", digits),
        CommandState::Operator { op, count } => {
            let c = match op { Operator::Delete => 'd', Operator::Change => 'c', Operator::Yank => 'y' };
            format!(" {}{}", count, c)
        }
        CommandState::Find { find_type, count } => {
            let c = match find_type { FindType::Forward => 'f', FindType::ForwardTill => 't', FindType::Backward => 'F', FindType::BackwardTill => 'T' };
            format!(" {}{}", count, c)
        }
        CommandState::Replace { count } => format!(" {}r", count),
    };
    format!("-- {} --{}", mode_str, pending)
}

/// Describe a VimAction for display purposes.
pub fn describe_action(action: &VimAction) -> &'static str {
    match action {
        VimAction::None => "none",
        VimAction::EnterInsert => "enter insert",
        VimAction::EnterInsertAppend => "append",
        VimAction::EnterInsertLineStart => "insert at start",
        VimAction::OpenLineBelow => "open below",
        VimAction::OpenLineAbove => "open above",
        VimAction::EnterNormal => "enter normal",
        VimAction::MoveCursor(m) => match m {
            CursorMove::Left(_) => "left", CursorMove::Right(_) => "right",
            CursorMove::WordForward(_) => "word forward", CursorMove::WordBackward(_) => "word backward",
            CursorMove::WordEnd(_) => "word end", CursorMove::LineStart => "line start",
            CursorMove::LineEnd => "line end", CursorMove::FirstNonBlank => "first non-blank",
        },
        VimAction::Delete(r) | VimAction::Change(r) | VimAction::Yank(r) => match r {
            TextRange::Motion(_) => "motion", TextRange::Line => "line",
            TextRange::InnerWord => "inner word", TextRange::AWord => "a word",
        },
        VimAction::DeleteChar => "delete char",
        VimAction::DeleteToEnd => "delete to end",
        VimAction::ChangeToEnd => "change to end",
        VimAction::YankLine => "yank line",
        VimAction::PasteAfter => "paste after",
        VimAction::PasteBefore => "paste before",
        VimAction::Undo => "undo",
        VimAction::Submit => "submit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_starts_in_insert() {
        let vim = VimState::new();
        assert_eq!(vim.mode, VimMode::Insert);
    }

    #[test]
    fn test_escape_enters_normal() {
        let mut vim = VimState::new();
        let action = vim.process_key("Escape");
        assert_eq!(action, Some(VimAction::EnterNormal));
        assert_eq!(vim.mode, VimMode::Normal);
    }

    #[test]
    fn test_i_enters_insert() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        vim.command = CommandState::Idle;
        let action = vim.process_key("i");
        assert_eq!(action, Some(VimAction::EnterInsert));
        assert_eq!(vim.mode, VimMode::Insert);
    }

    #[test]
    fn test_basic_motions() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        assert_eq!(
            vim.process_key("h"),
            Some(VimAction::MoveCursor(CursorMove::Left(1)))
        );
        assert_eq!(
            vim.process_key("l"),
            Some(VimAction::MoveCursor(CursorMove::Right(1)))
        );
        assert_eq!(
            vim.process_key("w"),
            Some(VimAction::MoveCursor(CursorMove::WordForward(1)))
        );
        assert_eq!(
            vim.process_key("b"),
            Some(VimAction::MoveCursor(CursorMove::WordBackward(1)))
        );
        assert_eq!(
            vim.process_key("$"),
            Some(VimAction::MoveCursor(CursorMove::LineEnd))
        );
        assert_eq!(
            vim.process_key("0"),
            Some(VimAction::MoveCursor(CursorMove::LineStart))
        );
    }

    #[test]
    fn test_dd_deletes_line() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        vim.process_key("d"); // Enter operator mode
        assert!(vim.is_pending());
        let action = vim.process_key("d");
        assert_eq!(action, Some(VimAction::Delete(TextRange::Line)));
    }

    #[test]
    fn test_count_motion() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        vim.process_key("3");
        let action = vim.process_key("w");
        assert_eq!(
            action,
            Some(VimAction::MoveCursor(CursorMove::WordForward(3)))
        );
    }

    #[test]
    fn test_dw_delete_word() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        vim.process_key("d");
        let action = vim.process_key("w");
        assert_eq!(
            action,
            Some(VimAction::Delete(TextRange::Motion(
                CursorMove::WordForward(1)
            )))
        );
    }

    #[test]
    fn test_insert_passthrough() {
        let mut vim = VimState::new();
        // In insert mode, regular keys return None (pass through)
        assert_eq!(vim.process_key("a"), None);
        assert_eq!(vim.process_key("x"), None);
    }

    #[test]
    fn test_pending_display() {
        let mut vim = VimState::new();
        vim.mode = VimMode::Normal;
        assert_eq!(vim.pending_display(), "");
        vim.process_key("d");
        assert_eq!(vim.pending_display(), "d\u{2026}");
    }
}
