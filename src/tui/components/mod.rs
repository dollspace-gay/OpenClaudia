//! Rich TUI components (overlays, pickers, dialogs).
//!
//! Each component lives in its own submodule with three pieces:
//! - a state struct (what the component remembers between frames)
//! - a `render(frame, area)` method that draws it
//! - a `handle_key(key) -> OverlayAction` method that processes input
//!
//! The [`ActiveOverlay`] enum in [`crate::tui::app`] drives which
//! component — if any — gets the event-loop's attention on a given
//! frame. At most one overlay is active at a time; opening a new one
//! closes the current one.
//!
//! Port of Claude Code's `components/` layer (dialogs, pickers,
//! overlays). OC's set is intentionally small — high-impact components
//! only — with the plumbing in place to add more without churning the
//! event loop.

pub mod help;
pub mod log_selector;

pub use help::HelpOverlay;
pub use log_selector::{LogSelector, SelectedSession};

use ratatui::layout::Rect;
use ratatui::Frame;

/// What the event loop should do after handing a key event to an
/// overlay. Returned by `handle_key` so the loop can close the overlay
/// (no change / selection made / canceled) without the component
/// reaching into app state itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    /// Overlay consumed the key and stays open.
    Consumed,
    /// Overlay wants to close without a result (Esc, outside click, etc.).
    Close,
    /// Overlay wants to close and hand a session id to the app for resume.
    ResumeSession(String),
}

/// Every overlay component implements this so the event loop can
/// render + drive it uniformly regardless of type. Kept minimal on
/// purpose — components that need config (e.g. a list of models to
/// pick from) accept it at construction.
pub trait Overlay {
    /// Render the overlay into `area` on top of the main UI. Callers
    /// typically center the area with `centered_rect`.
    fn render(&mut self, frame: &mut Frame, area: Rect);

    /// Handle one key event. Returns what the event loop should do
    /// next — see [`OverlayAction`].
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OverlayAction;
}

/// Compute a centered sub-rectangle of `area` with `percent_x`% width
/// and `percent_y`% height. Used to position overlays so they float
/// on top of the main UI with a comfortable border.
#[must_use]
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
            ratatui::layout::Constraint::Percentage(percent_y),
            ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
            ratatui::layout::Constraint::Percentage(percent_x),
            ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_shrinks_proportionally() {
        let full = Rect::new(0, 0, 100, 100);
        let centered = centered_rect(50, 50, full);
        assert_eq!(centered.width, 50);
        assert_eq!(centered.height, 50);
        // Centered position (rough — percentage math truncates).
        assert!(centered.x >= 24 && centered.x <= 26);
        assert!(centered.y >= 24 && centered.y <= 26);
    }

    #[test]
    fn centered_rect_full_size() {
        let full = Rect::new(0, 0, 40, 20);
        let centered = centered_rect(100, 100, full);
        // 100% means the inner layout takes the entire area.
        assert_eq!(centered.width, 40);
        assert_eq!(centered.height, 20);
    }
}
