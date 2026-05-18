//! Session-picker overlay for `--resume` / `/resume`.
//!
//! Port of Claude Code's `LogSelector` / session-picker UI. Shows every
//! saved session for the current project (newest-first) with a
//! preview of the first user prompt. Arrow keys move the highlight;
//! Enter resumes the selected session.
//!
//! The caller hands in an already-sorted `Vec<SessionRow>` so this
//! component stays testable without hitting the filesystem — transcript
//! enumeration lives in `crate::transcript::list_transcripts` and the
//! main app maps its output into `SessionRow` before constructing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use super::{Overlay, OverlayAction};

/// One displayable row in the picker — the fields actually rendered.
/// Deliberately narrower than `TranscriptInfo` so tests can stub
/// transcripts without touching disk.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    /// First user prompt (truncated to fit on one terminal line). None
    /// when the transcript had no user messages yet.
    pub first_prompt: Option<String>,
    pub message_count: usize,
    /// ISO-8601-ish timestamp for the "last activity" column. Callers
    /// format this — the overlay treats it as opaque text.
    pub modified_iso: String,
}

/// Result handed back to the app when the user picks a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSession {
    pub session_id: String,
}

/// Modal picker state.
pub struct LogSelector {
    rows: Vec<SessionRow>,
    state: ListState,
}

impl LogSelector {
    #[must_use]
    pub fn new(rows: Vec<SessionRow>) -> Self {
        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(0));
        }
        Self { rows, state }
    }

    /// True when the caller gave us zero sessions — the overlay shows
    /// an "empty" placeholder instead of a list in that case. Exposed
    /// so the event loop can decline to open an empty picker.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn select_delta(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let current = self.state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.state.select(Some(next as usize));
    }

    fn selected_id(&self) -> Option<String> {
        self.state
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.session_id.clone())
    }
}

impl Overlay for LogSelector {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(147, 112, 219)))
            .title(" resume session ");

        if self.rows.is_empty() {
            let empty = ratatui::widgets::Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No saved sessions for this project.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Esc to close.",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(block);
            frame.render_widget(empty, area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let id_short = row
                    .session_id
                    .get(..8)
                    .unwrap_or(&row.session_id)
                    .to_string();
                let prompt = row.first_prompt.as_deref().unwrap_or("(no prompt yet)");
                let prompt = truncate_display(prompt, 60);
                let count = row.message_count;
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{id_short}  "),
                        Style::default()
                            .fg(Color::Rgb(100, 180, 255))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{prompt}  "), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("({count} msgs, {})", row.modified_iso),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(60, 60, 90))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("\u{203A} ");

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn handle_key(&mut self, key: KeyEvent) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                OverlayAction::Close
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_delta(-1);
                OverlayAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_delta(1);
                OverlayAction::Consumed
            }
            KeyCode::Home => {
                if !self.rows.is_empty() {
                    self.state.select(Some(0));
                }
                OverlayAction::Consumed
            }
            KeyCode::End => {
                if !self.rows.is_empty() {
                    self.state.select(Some(self.rows.len() - 1));
                }
                OverlayAction::Consumed
            }
            KeyCode::Enter => match self.selected_id() {
                Some(id) => OverlayAction::ResumeSession(id),
                None => OverlayAction::Close,
            },
            _ => OverlayAction::Consumed,
        }
    }
}

/// Truncate `s` to at most `max_chars` character (not byte) width,
/// appending an ellipsis when a cut happens. Prevents mid-UTF8
/// slicing that would panic in ratatui's layout pass.
fn truncate_display(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let mut out: String = chars.iter().take(max_chars.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};

    fn row(id: &str, prompt: Option<&str>, count: usize) -> SessionRow {
        SessionRow {
            session_id: id.to_string(),
            first_prompt: prompt.map(str::to_string),
            message_count: count,
            modified_iso: "2026-04-20".to_string(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn empty_selector_ignores_enter() {
        let mut sel = LogSelector::new(vec![]);
        assert!(sel.is_empty());
        // Enter with no rows just closes rather than resuming.
        assert_eq!(sel.handle_key(key(KeyCode::Enter)), OverlayAction::Close);
    }

    #[test]
    fn arrows_wrap_around_list() {
        let mut sel = LogSelector::new(vec![
            row("aaa", Some("first"), 3),
            row("bbb", Some("second"), 5),
            row("ccc", Some("third"), 1),
        ]);
        assert_eq!(sel.state.selected(), Some(0));
        sel.handle_key(key(KeyCode::Down));
        assert_eq!(sel.state.selected(), Some(1));
        sel.handle_key(key(KeyCode::Down));
        assert_eq!(sel.state.selected(), Some(2));
        // Wrap forward.
        sel.handle_key(key(KeyCode::Down));
        assert_eq!(sel.state.selected(), Some(0));
        // Wrap backward.
        sel.handle_key(key(KeyCode::Up));
        assert_eq!(sel.state.selected(), Some(2));
    }

    #[test]
    fn enter_returns_selected_session_id() {
        let mut sel = LogSelector::new(vec![
            row("first-id", Some("q1"), 2),
            row("second-id", Some("q2"), 4),
        ]);
        sel.handle_key(key(KeyCode::Down));
        match sel.handle_key(key(KeyCode::Enter)) {
            OverlayAction::ResumeSession(id) => assert_eq!(id, "second-id"),
            other => panic!("expected ResumeSession, got {other:?}"),
        }
    }

    #[test]
    fn vim_keys_work() {
        let mut sel = LogSelector::new(vec![row("a", Some("x"), 1), row("b", Some("y"), 1)]);
        sel.handle_key(key(KeyCode::Char('j')));
        assert_eq!(sel.state.selected(), Some(1));
        sel.handle_key(key(KeyCode::Char('k')));
        assert_eq!(sel.state.selected(), Some(0));
    }

    #[test]
    fn home_end_jump_to_ends() {
        let rows: Vec<_> = (0..5).map(|i| row(&format!("{i}"), None, 0)).collect();
        let mut sel = LogSelector::new(rows);
        sel.handle_key(key(KeyCode::End));
        assert_eq!(sel.state.selected(), Some(4));
        sel.handle_key(key(KeyCode::Home));
        assert_eq!(sel.state.selected(), Some(0));
    }

    #[test]
    fn truncate_display_caps_and_ellipsizes() {
        assert_eq!(truncate_display("short", 10), "short");
        let long = "a".repeat(100);
        let cut = truncate_display(&long, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_display_handles_multibyte_without_panic() {
        // 10 multi-byte chars → 30 bytes. max_chars=5 would panic on a
        // byte-index-based truncation.
        let s = "日本語の文字列テスト";
        let cut = truncate_display(s, 5);
        assert_eq!(cut.chars().count(), 5);
    }
}
