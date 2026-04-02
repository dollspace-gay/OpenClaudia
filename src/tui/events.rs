//! Event system — merges terminal key events with async API streaming events.

use crossterm::event::{self, Event as CEvent, KeyEvent};
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
                    let should_break = match evt {
                        CEvent::Key(key) => event_tx.send(AppEvent::Key(key)).is_err(),
                        CEvent::Resize(w, h) => event_tx.send(AppEvent::Resize(w, h)).is_err(),
                        _ => false,
                    };
                    if should_break {
                        break;
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
