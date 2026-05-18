//! Keybindings cheat-sheet overlay.
//!
//! Shows a scrollable list of every keyboard shortcut the TUI
//! supports. Port of Claude Code's `/help` overlay — a single
//! discoverable surface for the bindings that would otherwise be
//! buried in the main keybindings file.
//!
//! Input:
//! - `Up` / `Down` / `PageUp` / `PageDown` / `Home` / `End` scroll.
//! - `Esc` / `q` / `?` close.
//! - Anything else is consumed silently (prevents stray key events
//!   from leaking to the underlying input field while the overlay is
//!   open).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::{Overlay, OverlayAction};

/// One line in the cheatsheet — a shortcut and what it does.
struct Shortcut {
    keys: &'static str,
    description: &'static str,
}

/// Full cheatsheet text. Grouped by section; keep groups short so the
/// overlay fits on typical 24-row terminals without scrolling.
const SECTIONS: &[(&str, &[Shortcut])] = &[
    (
        "Input",
        &[
            Shortcut {
                keys: "Enter",
                description: "send message",
            },
            Shortcut {
                keys: "Shift+Enter",
                description: "insert newline",
            },
            Shortcut {
                keys: "Ctrl+L",
                description: "clear the screen",
            },
            Shortcut {
                keys: "Ctrl+C",
                description: "cancel current turn / exit when idle",
            },
        ],
    ),
    (
        "Navigation",
        &[
            Shortcut {
                keys: "PageUp / PageDown",
                description: "scroll the transcript",
            },
            Shortcut {
                keys: "Up / Down",
                description: "previous / next input in history",
            },
            Shortcut {
                keys: "Esc",
                description: "close overlays, dismiss prompts",
            },
        ],
    ),
    (
        "Slash commands",
        &[
            Shortcut {
                keys: "/help",
                description: "show this overlay",
            },
            Shortcut {
                keys: "/agents",
                description: "list subagent types",
            },
            Shortcut {
                keys: "/sessions",
                description: "list saved sessions",
            },
            Shortcut {
                keys: "/resume",
                description: "resume a session by id or index",
            },
            Shortcut {
                keys: "/compact",
                description: "summarize old messages to free context",
            },
            Shortcut {
                keys: "/export",
                description: "export the conversation to markdown",
            },
            Shortcut {
                keys: "/effort",
                description: "switch effort level (low / medium / high / max)",
            },
            Shortcut {
                keys: "/model",
                description: "switch the model in use",
            },
            Shortcut {
                keys: "/mode",
                description: "switch behavioral mode",
            },
            Shortcut {
                keys: "/plan",
                description: "toggle plan mode (read-only)",
            },
            Shortcut {
                keys: "/undo",
                description: "drop the last user + assistant pair",
            },
            Shortcut {
                keys: "/redo",
                description: "restore the last undone pair",
            },
            Shortcut {
                keys: "/quit",
                description: "exit OpenClaudia",
            },
        ],
    ),
    (
        "Thinking",
        &[
            Shortcut {
                keys: "\"ultrathink\"",
                description: "mention in any user message → max thinking budget (31 999)",
            },
            Shortcut {
                keys: "env MAX_THINKING_TOKENS=N",
                description: "force a specific Anthropic thinking budget",
            },
            Shortcut {
                keys: "env CLAUDE_CODE_EFFORT_LEVEL=…",
                description: "low/medium/high/max/unset — overrides the in-session effort",
            },
        ],
    ),
];

/// Keybindings overlay state. Small — just the scroll offset.
#[derive(Debug, Default)]
pub struct HelpOverlay {
    /// Lines scrolled off the top. Clamped against the rendered line
    /// count inside `handle_key` so overflow is a no-op.
    scroll: u16,
    /// Most recent viewport height (rows). Set on each `render` so
    /// key handling can clamp scroll against the actual visible area.
    last_height: u16,
}

impl HelpOverlay {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scroll: 0,
            last_height: 0,
        }
    }

    /// Flatten the section tree into one `Vec<Line>` for the paragraph
    /// widget. Allocation-per-render is fine — the cheatsheet is <50
    /// lines and this runs once per frame, at human-input cadence.
    fn build_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            "Keyboard shortcuts",
            Style::default()
                .fg(Color::Rgb(147, 112, 219))
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        for (title, shortcuts) in SECTIONS {
            lines.push(Line::from(Span::styled(
                *title,
                Style::default()
                    .fg(Color::Rgb(218, 165, 32))
                    .add_modifier(Modifier::BOLD),
            )));
            for sc in *shortcuts {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        sc.keys,
                        Style::default()
                            .fg(Color::Rgb(100, 180, 255))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(sc.description, Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            "Esc / q / ? to close",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        lines
    }

    /// Max scroll offset given the current lines and viewport.
    fn max_scroll(&self, lines: u16, viewport: u16) -> u16 {
        lines.saturating_sub(viewport)
    }
}

impl Overlay for HelpOverlay {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.last_height = area.height.saturating_sub(2); // borders
        let lines = self.build_lines();
        #[allow(clippy::cast_possible_truncation)] // list is tiny
        let total_lines = lines.len() as u16;
        // Clamp scroll to the current viewport so resize doesn't leave
        // us scrolled past the bottom.
        let max = self.max_scroll(total_lines, self.last_height);
        if self.scroll > max {
            self.scroll = max;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(147, 112, 219)))
            .title(" help ");
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(paragraph, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => OverlayAction::Close,
            // Ctrl+C closes too (matches CC dismiss semantics).
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                OverlayAction::Close
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                OverlayAction::Consumed
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                OverlayAction::Consumed
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(self.last_height.max(1));
                OverlayAction::Consumed
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(self.last_height.max(1));
                OverlayAction::Consumed
            }
            KeyCode::Home => {
                self.scroll = 0;
                OverlayAction::Consumed
            }
            KeyCode::End => {
                // Actual max is clamped on the next render.
                self.scroll = u16::MAX;
                OverlayAction::Consumed
            }
            _ => OverlayAction::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn esc_q_question_mark_close_overlay() {
        let mut overlay = HelpOverlay::new();
        assert_eq!(overlay.handle_key(key(KeyCode::Esc)), OverlayAction::Close);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('q'))),
            OverlayAction::Close
        );
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('?'))),
            OverlayAction::Close
        );
        assert_eq!(
            overlay.handle_key(ctrl(KeyCode::Char('c'))),
            OverlayAction::Close
        );
    }

    #[test]
    fn arrows_scroll_and_saturate_at_zero() {
        let mut overlay = HelpOverlay::new();
        assert_eq!(overlay.scroll, 0);
        // Up at zero shouldn't underflow.
        overlay.handle_key(key(KeyCode::Up));
        assert_eq!(overlay.scroll, 0);
        overlay.handle_key(key(KeyCode::Down));
        overlay.handle_key(key(KeyCode::Down));
        assert_eq!(overlay.scroll, 2);
        overlay.handle_key(key(KeyCode::Up));
        assert_eq!(overlay.scroll, 1);
    }

    #[test]
    fn home_resets_end_maxes_out() {
        let mut overlay = HelpOverlay::new();
        overlay.handle_key(key(KeyCode::End));
        assert_eq!(overlay.scroll, u16::MAX);
        overlay.handle_key(key(KeyCode::Home));
        assert_eq!(overlay.scroll, 0);
    }

    #[test]
    fn sections_non_empty() {
        // Guard: if someone deletes the cheatsheet by accident, this
        // test fails rather than shipping an empty overlay.
        assert!(!SECTIONS.is_empty());
        for (title, shortcuts) in SECTIONS {
            assert!(!title.is_empty());
            assert!(!shortcuts.is_empty(), "section {title} has no shortcuts");
        }
    }
}
