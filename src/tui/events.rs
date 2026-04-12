//! Event system — merges terminal key events with async API streaming events.

use crossterm::event::{self, Event as CEvent, KeyEvent, KeyEventKind};
use std::sync::mpsc;
use std::time::Duration;

/// User's response to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResponse {
    /// Allow this one time
    Allow,
    /// Deny this one time
    Deny,
    /// Always allow this tool in this session
    AlwaysAllow,
    /// Always deny this tool in this session
    AlwaysDeny,
}

/// Application events from multiple sources.
pub enum AppEvent {
    /// Terminal key event
    Key(KeyEvent),
    /// Terminal resize
    Resize(u16, u16),
    /// Animation tick
    Tick,
    /// Streaming text delta from API
    StreamText(String),
    /// Streaming thinking text
    StreamThinking(String),
    /// Tool execution started
    ToolStart { name: String, description: String },
    /// Tool execution completed
    ToolDone {
        name: String,
        success: bool,
        content: String,
    },
    /// API response completed
    ResponseDone,
    /// API error
    ApiError(String),
    /// Tool results require a follow-up API call
    FollowUp,
    /// Sync updated session messages back to the App after an agentic loop.
    SyncMessages(Vec<serde_json::Value>),
    /// Pipeline requesting permission to run a tool.
    /// Includes a oneshot sender to reply with the user's decision.
    PermissionRequest {
        tool_name: String,
        tool_args: String,
        reply: std::sync::mpsc::Sender<PermissionResponse>,
    },
}

/// Handles terminal events in a background thread, merges with async events.
pub struct EventHandler {
    rx: mpsc::Receiver<AppEvent>,
    tx: mpsc::Sender<AppEvent>,
}

impl EventHandler {
    #[must_use]
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let event_tx = tx.clone();

        std::thread::spawn(move || loop {
            if event::poll(tick_rate).unwrap_or(false) {
                if let Ok(evt) = event::read() {
                    if let Some(app_evt) = translate_terminal_event(evt) {
                        if event_tx.send(app_evt).is_err() {
                            break;
                        }
                    }
                }
            } else if event_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        });

        Self { rx, tx }
    }

    /// Get a sender for pushing async events (streaming, tool results) into the loop.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
    }

    /// Block until next event.
    ///
    /// # Errors
    ///
    /// Returns an error if the event channel is disconnected.
    pub fn next(&self) -> Result<AppEvent, mpsc::RecvError> {
        self.rx.recv()
    }
}

/// Translate a raw crossterm terminal event into an [`AppEvent`], filtering
/// events we do not want to propagate to the UI thread.
///
/// On Windows, crossterm fires both [`KeyEventKind::Press`] and
/// [`KeyEventKind::Release`] for every keystroke. Forwarding both would cause
/// each key to be processed twice (e.g. `"a"` → `"aa"`, `"A"` → `"Aa"`, `"?"`
/// → `"?/"` because the shifted and unshifted forms of the same physical key
/// are delivered on press vs. release). We therefore accept only `Press` and
/// `Repeat`, which matches Unix behavior and keeps key-hold working on
/// terminals that support the kitty keyboard protocol.
///
/// See: <https://github.com/ratatui/ratatui/issues/347>
fn translate_terminal_event(evt: CEvent) -> Option<AppEvent> {
    match evt {
        CEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            Some(AppEvent::Key(key))
        }
        CEvent::Key(_) => None,
        CEvent::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};

    fn key(code: KeyCode, kind: KeyEventKind) -> CEvent {
        CEvent::Key(KeyEvent::new_with_kind_and_state(
            code,
            KeyModifiers::NONE,
            kind,
            KeyEventState::NONE,
        ))
    }

    #[test]
    fn press_events_are_forwarded() {
        let evt = key(KeyCode::Char('a'), KeyEventKind::Press);
        match translate_terminal_event(evt) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Char('a')),
            _ => panic!("expected AppEvent::Key for Press"),
        }
    }

    #[test]
    fn repeat_events_are_forwarded() {
        // Repeat should be honored so holding a key still works under the
        // kitty keyboard protocol.
        let evt = key(KeyCode::Char('x'), KeyEventKind::Repeat);
        match translate_terminal_event(evt) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Char('x')),
            _ => panic!("expected AppEvent::Key for Repeat"),
        }
    }

    #[test]
    fn release_events_are_dropped() {
        // Regression test for https://github.com/dollspace-gay/OpenClaudia/issues/13
        // On Windows, crossterm fires a Release alongside every Press. If we
        // forwarded these, every key would be entered twice.
        let evt = key(KeyCode::Char('a'), KeyEventKind::Release);
        assert!(translate_terminal_event(evt).is_none());
    }

    #[test]
    fn shifted_release_is_dropped() {
        // The original bug report noted that entering "A" produced "Aa" and
        // "?" produced "?/". That happens because Windows delivers the
        // shifted form on Press and the unshifted form on Release of the
        // same physical key. Verify that the Release is discarded regardless
        // of which glyph it carries.
        let evt = key(KeyCode::Char('/'), KeyEventKind::Release);
        assert!(translate_terminal_event(evt).is_none());
    }

    #[test]
    fn resize_events_are_forwarded() {
        match translate_terminal_event(CEvent::Resize(120, 40)) {
            Some(AppEvent::Resize(w, h)) => {
                assert_eq!(w, 120);
                assert_eq!(h, 40);
            }
            _ => panic!("expected AppEvent::Resize"),
        }
    }
}
