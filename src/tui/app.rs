//! Full-screen interactive TUI application.
//!
//! Launched via `openclaudia` (default) or `openclaudia --tui`.
//! Provides a scrollable message view, text input area, status bar,
//! and streaming response display wired to the real API pipeline.

use super::events::{AppEvent, EventHandler};
use super::input::TextInput;
use super::messages::{DisplayMessage, MessageList};
use crossterm::{
    event::{KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Chat session state — compatible with the CLI's ChatSession JSON format
/// so sessions saved by one can be loaded by the other.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TuiSession {
    pub id: String,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub mode: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    undo_stack: Vec<(serde_json::Value, serde_json::Value)>,
}

impl TuiSession {
    fn new(model: &str, provider: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New conversation".to_string(),
            created_at: now,
            updated_at: now,
            model: model.to_string(),
            provider: provider.to_string(),
            mode: "Build".to_string(),
            messages: Vec::new(),
            undo_stack: Vec::new(),
        }
    }

    fn touch(&mut self) { self.updated_at = chrono::Utc::now(); }

    fn update_title(&mut self) {
        if let Some(first_user) = self.messages.iter().find(|m|
            m.get("role").and_then(|r| r.as_str()) == Some("user")
        ) {
            if let Some(content) = first_user.get("content").and_then(|c| c.as_str()) {
                self.title = if content.len() > 50 {
                    format!("{}...", crate::tools::safe_truncate(&content, 47))
                } else {
                    content.to_string()
                };
            }
        }
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode.as_str() {
            "Build" => "Plan".to_string(),
            _ => "Build".to_string(),
        };
    }

    fn mode_description(&self) -> &str {
        match self.mode.as_str() {
            "Plan" => "Read-only — suggestions only",
            _ => "Full access — can make changes",
        }
    }

    fn undo(&mut self) -> bool {
        if self.messages.len() >= 2 {
            if let (Some(assistant), Some(user)) = (self.messages.pop(), self.messages.pop()) {
                self.undo_stack.push((user, assistant));
                self.touch();
                return true;
            }
        }
        false
    }

    fn redo(&mut self) -> bool {
        if let Some((user, assistant)) = self.undo_stack.pop() {
            self.messages.push(user);
            self.messages.push(assistant);
            self.touch();
            true
        } else {
            false
        }
    }

    fn estimate_tokens(&self) -> usize {
        self.messages.iter().map(|m| {
            m.get("content").and_then(|c| c.as_str()).unwrap_or("").len() / 4 + 4
        }).sum()
    }
}

/// Expand @filename references in user input by inlining file contents.
fn expand_file_refs(input: &str) -> String {
    if !input.contains('@') {
        return input.to_string();
    }
    let re = regex::Regex::new(r#"@"([^"]+)"|@(\S+)"#).unwrap();
    let mut result = input.to_string();
    let mut replacements = Vec::new();

    // Get project root for path traversal validation
    let cwd = std::env::current_dir().unwrap_or_default();

    for cap in re.captures_iter(input) {
        let full_match = cap.get(0).unwrap().as_str();
        let raw_path = cap.get(1).or(cap.get(2)).unwrap().as_str();

        // Resolve and validate path — reject traversal attempts
        let resolved = if std::path::Path::new(raw_path).is_absolute() {
            std::path::PathBuf::from(raw_path)
        } else {
            cwd.join(raw_path)
        };

        // Reject paths with .. components
        if resolved.components().any(|c| c == std::path::Component::ParentDir) {
            replacements.push((
                full_match.to_string(),
                format!("[Path traversal blocked: {raw_path}]"),
            ));
            continue;
        }

        // Canonicalize and verify it's within the project root
        match std::fs::canonicalize(&resolved) {
            Ok(canonical) => {
                if !canonical.starts_with(&cwd) {
                    replacements.push((
                        full_match.to_string(),
                        format!("[File outside project directory: {raw_path}]"),
                    ));
                    continue;
                }
                match std::fs::read_to_string(&canonical) {
                    Ok(content) => {
                        replacements.push((
                            full_match.to_string(),
                            format!("\n<file path=\"{}\">\n{}\n</file>\n", canonical.display(), content.trim()),
                        ));
                    }
                    Err(e) => {
                        replacements.push((
                            full_match.to_string(),
                            format!("[Cannot read {raw_path}: {e}]"),
                        ));
                    }
                }
            }
            Err(_) => {
                replacements.push((
                    full_match.to_string(),
                    format!("[File not found: {raw_path}]"),
                ));
            }
        }
    }
    for (from, to) in replacements {
        result = result.replace(&from, &to);
    }
    result
}

fn sessions_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("openclaudia")
        .join("chat_sessions")
}

fn save_session(session: &TuiSession) -> Result<(), String> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", session.id));
    let json = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

fn list_sessions() -> Vec<TuiSession> {
    let dir = sessions_dir();
    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<TuiSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

/// A pending permission prompt waiting for user input.
struct PendingPermission {
    tool_name: String,
    tool_args: String,
    reply: std::sync::mpsc::Sender<super::events::PermissionResponse>,
}

/// Main TUI application state.
pub struct App {
    pub messages: MessageList,
    pub input: TextInput,
    pub model: String,
    pub provider: String,
    pub tokens: usize,
    pub mode: String,
    pub should_quit: bool,
    pub is_waiting: bool,
    spinner_frame: usize,
    /// Sender for pushing API events into the event loop's channel.
    api_event_tx: Option<std::sync::mpsc::Sender<AppEvent>>,

    // ── API pipeline fields ──
    pub client: reqwest::Client,
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub effort_level: String,
    pub system_prompt: String,
    pub claude_code_token: Option<String>,
    /// Memory database for auto-learning from tool execution.
    pub memory_db: Option<std::sync::Arc<crate::memory::MemoryDb>>,
    /// Conversation messages in the provider's wire format.
    pub session_messages: Vec<serde_json::Value>,
    /// Async runtime handle for spawning API tasks from the sync event loop.
    runtime_handle: Option<tokio::runtime::Handle>,
    /// Persistent chat session (for save/load/resume)
    pub chat_session: TuiSession,
    /// Active permission prompt (if any). Tool execution blocks until resolved.
    pending_permission: Option<PendingPermission>,
    /// Hook engine for running lifecycle hooks.
    pub hook_engine: Option<std::sync::Arc<crate::hooks::HookEngine>>,
    /// Rules content injected as system message (loaded once at startup).
    pub rules_content: Option<String>,
    /// Whether rules have been injected into session messages.
    rules_injected: bool,
}

impl App {
    #[must_use]
    pub fn new(model: &str, provider: &str) -> Self {
        Self {
            messages: MessageList::new(),
            input: TextInput::new(),
            model: model.to_string(),
            provider: provider.to_string(),
            tokens: 0,
            mode: "Build".to_string(),
            should_quit: false,
            is_waiting: false,
            spinner_frame: 0,
            api_event_tx: None,
            client: reqwest::Client::new(),
            endpoint: String::new(),
            headers: Vec::new(),
            effort_level: "medium".to_string(),
            system_prompt: String::new(),
            claude_code_token: None,
            memory_db: None,
            session_messages: Vec::new(),
            runtime_handle: None,
            chat_session: TuiSession::new(model, provider),
            pending_permission: None,
            hook_engine: None,
            rules_content: None,
            rules_injected: false,
        }
    }

    /// Set the API connection details needed to make requests.
    pub fn set_api_config(
        &mut self,
        endpoint: String,
        headers: Vec<(String, String)>,
        system_prompt: String,
        claude_code_token: Option<String>,
    ) {
        self.endpoint = endpoint;
        self.headers = headers;
        self.system_prompt = system_prompt;
        self.claude_code_token = claude_code_token;
    }

    /// Get an event sender for pushing async API events into the TUI loop.
    #[must_use]
    pub fn event_sender(&self) -> Option<std::sync::mpsc::Sender<AppEvent>> {
        self.api_event_tx.clone()
    }

    /// Run the interactive TUI event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal initialization or rendering fails.
    pub fn run(&mut self) -> io::Result<()> {
        // Capture the tokio runtime handle (must be called inside an async context).
        self.runtime_handle = tokio::runtime::Handle::try_current().ok();

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Single event handler — MUST NOT create two, or they steal each other's keypresses
        let events = EventHandler::new(Duration::from_millis(100));
        // Store a sender clone so spawn_api_turn can push events into the same channel
        self.api_event_tx = Some(events.sender());

        // Inject system prompt as the first message
        if !self.system_prompt.is_empty() {
            self.session_messages.push(serde_json::json!({
                "role": "system",
                "content": self.system_prompt
            }));
        }

        // No welcome message added to the message list — the welcome
        // box is rendered directly in draw() as a ratatui widget.

        loop {
            terminal.draw(|frame| self.draw(frame))?;

            match events.next() {
                Ok(AppEvent::Key(key)) => self.handle_key(key),
                Ok(AppEvent::Tick) => {
                    self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
                }
                Ok(AppEvent::StreamText(text)) => {
                    self.messages.append_streaming(&text);
                    self.messages.scroll_to_bottom();
                }
                Ok(AppEvent::StreamThinking(text)) => {
                    self.messages
                        .append_streaming(&format!("[thinking] {text}"));
                }
                Ok(AppEvent::ToolStart { name, description }) => {
                    self.messages.add(DisplayMessage {
                        role: "tool".to_string(),
                        content: description,
                        tool_name: Some(name),
                        is_error: false,
                        is_thinking: false,
                    });
                }
                Ok(AppEvent::ToolDone {
                    name,
                    success,
                    content,
                }) => {
                    let preview = if content.len() > 300 {
                        format!("{}...", &content[..297])
                    } else {
                        content
                    };
                    self.messages.add(DisplayMessage {
                        role: "tool".to_string(),
                        content: preview,
                        tool_name: Some(name),
                        is_error: !success,
                        is_thinking: false,
                    });
                }
                Ok(AppEvent::ResponseDone) => {
                    self.messages.finish_streaming();
                    self.is_waiting = false;
                    // Sync session messages and auto-save
                    self.chat_session.messages = self.session_messages.clone();
                    self.chat_session.update_title();
                    self.chat_session.touch();
                    let _ = save_session(&self.chat_session);
                    // Update token estimate
                    self.tokens = self.chat_session.estimate_tokens();
                }
                Ok(AppEvent::ApiError(msg)) => {
                    self.messages.finish_streaming();
                    self.messages.add(DisplayMessage {
                        role: "system".to_string(),
                        content: format!("Error: {msg}"),
                        tool_name: None,
                        is_error: true,
                        is_thinking: false,
                    });
                    self.is_waiting = false;
                }
                Ok(AppEvent::Resize(_, _)) => {} // terminal.draw handles it
                // Pipeline follow-up: tool results need another API call
                Ok(AppEvent::FollowUp) => {
                    self.spawn_api_turn();
                }
                // Sync updated session messages after agentic loop
                Ok(AppEvent::SyncMessages(messages)) => {
                    self.session_messages = messages;
                }
                // Pipeline asking permission for a write/destructive tool
                Ok(AppEvent::PermissionRequest {
                    tool_name,
                    tool_args,
                    reply,
                }) => {
                    self.pending_permission = Some(PendingPermission {
                        tool_name,
                        tool_args,
                        reply,
                    });
                }
                Err(_) => break,
            }

            if self.should_quit {
                break;
            }
        }

        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;

        // Save session on exit
        self.chat_session.messages = self.session_messages.clone();
        self.chat_session.touch();
        let _ = save_session(&self.chat_session);

        Ok(())
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // Ctrl+C always quits
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            // If permission prompt is active, deny and dismiss
            if let Some(perm) = self.pending_permission.take() {
                let _ = perm.reply.send(super::events::PermissionResponse::Deny);
                return;
            }
            self.should_quit = true;
            return;
        }

        // Handle permission prompt keystrokes
        if self.pending_permission.is_some() {
            use super::events::PermissionResponse;
            let response = match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(PermissionResponse::Allow),
                KeyCode::Char('n') | KeyCode::Char('N') => Some(PermissionResponse::Deny),
                KeyCode::Char('a') | KeyCode::Char('A') => Some(PermissionResponse::AlwaysAllow),
                KeyCode::Char('d') | KeyCode::Char('D') => Some(PermissionResponse::AlwaysDeny),
                KeyCode::Esc => Some(PermissionResponse::Deny),
                _ => None,
            };
            if let Some(resp) = response {
                if let Some(perm) = self.pending_permission.take() {
                    let label = match resp {
                        PermissionResponse::Allow => "Allowed",
                        PermissionResponse::AlwaysAllow => "Always allowed",
                        PermissionResponse::Deny => "Denied",
                        PermissionResponse::AlwaysDeny => "Always denied",
                    };
                    self.messages.add(DisplayMessage {
                        role: "system".to_string(),
                        content: format!("{label}: {}", perm.tool_name),
                        tool_name: None,
                        is_error: matches!(resp, PermissionResponse::Deny | PermissionResponse::AlwaysDeny),
                        is_thinking: false,
                    });
                    let _ = perm.reply.send(resp);
                }
            }
            return;
        }

        // During streaming, Escape cancels
        if self.is_waiting {
            if key.code == KeyCode::Esc {
                self.is_waiting = false;
                self.messages.finish_streaming();
                self.messages.add(DisplayMessage {
                    role: "system".to_string(),
                    content: "[Response interrupted]".to_string(),
                    tool_name: None,
                    is_error: false,
                    is_thinking: false,
                });
            }
            return;
        }

        match key.code {
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    let text = self.input.take();

                    // ── Slash commands ──
                    if text == "/quit" || text == "/exit" {
                        self.should_quit = true;
                        return;
                    }

                    if text == "/help" || text == "?" {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: "Commands:\n\
                                      /help          Show this help\n\
                                      /mode          Toggle Build/Plan mode\n\
                                      /effort [lvl]  Set effort (low/medium/high) or cycle\n\
                                      /status        Show session info\n\
                                      /sessions      List saved sessions\n\
                                      /load <id>     Load a saved session\n\
                                      /undo          Undo last message pair\n\
                                      /redo          Redo undone messages\n\
                                      /clear         Clear conversation\n\
                                      /skill [name]  List or run skills\n\
                                      /export        Export conversation to markdown\n\
                                      !<cmd>         Run shell command\n\
                                      /quit          Exit\n\n\
                                      Scroll: Up/Down/PageUp/PageDown · Cancel: Esc · Quit: Ctrl+C"
                                .to_string(),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text == "/clear" {
                        self.messages = MessageList::new();
                        // Reset session but keep system prompt
                        self.session_messages.retain(|m| {
                            m.get("role").and_then(|r| r.as_str()) == Some("system")
                        });
                        return;
                    }

                    if text == "/status" {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!(
                                "Model: {}\nProvider: {}\nEffort: {}\nMessages: {}\n~{} tokens",
                                self.model,
                                self.provider,
                                self.effort_level,
                                self.session_messages.len(),
                                self.tokens,
                            ),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text == "/mode" {
                        self.chat_session.toggle_mode();
                        self.mode = self.chat_session.mode.clone();
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!(
                                "Mode: {} — {}",
                                self.chat_session.mode,
                                self.chat_session.mode_description()
                            ),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text == "/sessions" || text == "/list" {
                        let sessions = list_sessions();
                        if sessions.is_empty() {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "No saved sessions.".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        } else {
                            let list = sessions
                                .iter()
                                .take(10)
                                .map(|s| {
                                    format!(
                                        "  {} — {} ({})",
                                        &s.id[..8],
                                        s.title,
                                        s.updated_at.format("%Y-%m-%d %H:%M")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: format!("Saved sessions:\n{list}\n\nUse /load <id> to resume."),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        }
                        return;
                    }

                    if text.starts_with("/load ") || text.starts_with("/continue ") {
                        let id = text.split_whitespace().nth(1).unwrap_or("");
                        let sessions = list_sessions();
                        if let Some(loaded) = sessions.iter().find(|s| s.id.starts_with(id)) {
                            self.chat_session = loaded.clone();
                            self.session_messages = loaded.messages.clone();
                            self.model = loaded.model.clone();
                            self.provider = loaded.provider.clone();
                            self.mode = loaded.mode.clone();
                            self.tokens = self.chat_session.estimate_tokens();
                            // Show loaded messages in the display
                            self.messages = super::messages::MessageList::new();
                            for msg in &loaded.messages {
                                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("system");
                                let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                                if role == "system" { continue; }
                                self.messages.add(DisplayMessage {
                                    role: role.to_string(),
                                    content: content.to_string(),
                                    tool_name: None,
                                    is_error: false,
                                    is_thinking: false,
                                });
                            }
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: format!("Resumed session: {} ({})", loaded.title, &loaded.id[..8]),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        } else {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: format!("Session not found: {id}"),
                                tool_name: None,
                                is_error: true,
                                is_thinking: false,
                            });
                        }
                        return;
                    }

                    if text == "/undo" {
                        if self.chat_session.undo() {
                            self.session_messages = self.chat_session.messages.clone();
                            // Remove last two display messages (user + assistant)
                            if self.messages.messages.len() >= 2 {
                                self.messages.messages.pop();
                                self.messages.messages.pop();
                            }
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "Undone last message pair.".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                            let _ = save_session(&self.chat_session);
                        } else {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "Nothing to undo.".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        }
                        return;
                    }

                    if text == "/redo" {
                        if self.chat_session.redo() {
                            self.session_messages = self.chat_session.messages.clone();
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "Redone last undone messages.".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                            let _ = save_session(&self.chat_session);
                        } else {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "Nothing to redo.".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        }
                        return;
                    }

                    if text == "/export" {
                        let mut md = format!("# {}\n\n", self.chat_session.title);
                        md.push_str(&format!(
                            "Model: {} · Provider: {} · {}\n\n---\n\n",
                            self.model, self.provider, self.chat_session.created_at.format("%Y-%m-%d %H:%M")
                        ));
                        for msg in &self.session_messages {
                            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                            if role == "system" { continue; }
                            md.push_str(&format!("**{role}:**\n{content}\n\n"));
                        }
                        let export_path = format!("conversation-{}.md", &self.chat_session.id[..8]);
                        match std::fs::write(&export_path, &md) {
                            Ok(()) => {
                                self.messages.add(DisplayMessage {
                                    role: "system".to_string(),
                                    content: format!("Exported to {export_path}"),
                                    tool_name: None,
                                    is_error: false,
                                    is_thinking: false,
                                });
                            }
                            Err(e) => {
                                self.messages.add(DisplayMessage {
                                    role: "system".to_string(),
                                    content: format!("Export failed: {e}"),
                                    tool_name: None,
                                    is_error: true,
                                    is_thinking: false,
                                });
                            }
                        }
                        return;
                    }

                    // Shell commands: !command
                    if let Some(cmd) = text.strip_prefix('!') {
                        let cmd = cmd.trim();
                        if !cmd.is_empty() {
                            let output = std::process::Command::new("bash")
                                .arg("-c")
                                .arg(cmd)
                                .output();
                            match output {
                                Ok(out) => {
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    let mut result = String::new();
                                    if !stdout.is_empty() {
                                        result.push_str(&stdout);
                                    }
                                    if !stderr.is_empty() {
                                        if !result.is_empty() { result.push('\n'); }
                                        result.push_str(&stderr);
                                    }
                                    if result.is_empty() {
                                        result = "(no output)".to_string();
                                    }
                                    self.messages.add(DisplayMessage {
                                        role: "tool".to_string(),
                                        content: result,
                                        tool_name: Some(format!("$ {cmd}")),
                                        is_error: !out.status.success(),
                                        is_thinking: false,
                                    });
                                }
                                Err(e) => {
                                    self.messages.add(DisplayMessage {
                                        role: "tool".to_string(),
                                        content: format!("Failed: {e}"),
                                        tool_name: Some(format!("$ {cmd}")),
                                        is_error: true,
                                        is_thinking: false,
                                    });
                                }
                            }
                        }
                        return;
                    }

                    if text.starts_with("/effort") {
                        let parts: Vec<&str> = text.splitn(2, ' ').collect();
                        if parts.len() == 2 {
                            let level = parts[1].trim();
                            if matches!(level, "low" | "medium" | "high") {
                                self.effort_level = level.to_string();
                            }
                        } else {
                            // Cycle: low -> medium -> high -> low
                            self.effort_level = match self.effort_level.as_str() {
                                "low" => "medium".to_string(),
                                "medium" => "high".to_string(),
                                _ => "low".to_string(),
                            };
                        }
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!("Effort level: {}", self.effort_level),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text == "/skill" || text == "/skills" {
                        let skills = crate::skills::load_skills();
                        if skills.is_empty() {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: "No skills found. Add .md files to .openclaudia/skills/".to_string(),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        } else {
                            let list = skills
                                .iter()
                                .map(|s| format!("  /{} — {}", s.name, s.description))
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: format!("Available skills:\n{list}"),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });
                        }
                        return;
                    }

                    // Check if it's a skill invocation: /skillname or /skill skillname
                    if text.starts_with('/') {
                        let skill_name = if text.starts_with("/skill ") {
                            text.strip_prefix("/skill ").unwrap_or("").trim()
                        } else {
                            text.strip_prefix('/').unwrap_or("")
                        };

                        if let Some(skill) = crate::skills::get_skill(skill_name) {
                            self.messages.add(DisplayMessage {
                                role: "system".to_string(),
                                content: format!("Running skill: /{}", skill.name),
                                tool_name: None,
                                is_error: false,
                                is_thinking: false,
                            });

                            // Inject skill prompt as user message and send to API
                            self.session_messages.push(serde_json::json!({
                                "role": "user",
                                "content": skill.prompt
                            }));
                            self.is_waiting = true;
                            self.spawn_api_turn();
                            return;
                        }

                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!("Unknown command: {text}. Type /help for commands."),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    // ── Normal message: send to API ──

                    // Expand @file references (inline file contents)
                    let expanded = expand_file_refs(&text);

                    self.messages.add(DisplayMessage {
                        role: "user".to_string(),
                        content: text.clone(), // Show original to user
                        tool_name: None,
                        is_error: false,
                        is_thinking: false,
                    });

                    // Add expanded version to session history (model sees file contents)
                    self.session_messages.push(serde_json::json!({
                        "role": "user",
                        "content": expanded
                    }));

                    // Inject rules as system message on first turn
                    if !self.rules_injected {
                        if let Some(ref rules) = self.rules_content {
                            self.session_messages.insert(0, serde_json::json!({
                                "role": "system",
                                "content": rules
                            }));
                        }
                        self.rules_injected = true;
                    }

                    // Reset guardrails for this turn
                    crate::guardrails::reset_turn();

                    self.is_waiting = true;
                    self.spawn_api_turn();
                }
            }
            KeyCode::Char(c) => self.input.insert(c),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => self.messages.scroll_up(3),
            KeyCode::Down => self.messages.scroll_down(3),
            KeyCode::PageUp => self.messages.scroll_up(15),
            KeyCode::PageDown => self.messages.scroll_down(15),
            _ => {}
        }
    }

    /// Spawn an async API turn on the tokio runtime.
    ///
    /// Sends events through the event handler's mpsc channel so the
    /// synchronous TUI event loop can display streaming output.
    fn spawn_api_turn(&mut self) {
        let Some(ref handle) = self.runtime_handle else {
            // No async runtime — show fallback message
            self.messages.add(DisplayMessage {
                role: "system".to_string(),
                content: "[No async runtime — cannot call API. Run with tokio.]".to_string(),
                tool_name: None,
                is_error: true,
                is_thinking: false,
            });
            self.is_waiting = false;
            return;
        };

        let Some(tx) = self.event_sender() else {
            self.is_waiting = false;
            return;
        };

        // Build request
        let request_body = crate::pipeline::build_request(
            &self.provider,
            &self.model,
            &self.session_messages,
            &self.effort_level,
            self.claude_code_token.as_deref(),
        );

        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let headers = self.headers.clone();
        let provider = self.provider.clone();
        let model = self.model.clone();
        let effort_level = self.effort_level.clone();
        let claude_code_token = self.claude_code_token.clone();
        let hook_engine = self.hook_engine.clone();
        let memory_db = self.memory_db.clone();
        // Clone session messages so the async task can build follow-up requests
        let mut session_messages = self.session_messages.clone();

        handle.spawn(async move {
            // Run UserPromptSubmit hooks before the API call
            if let Some(ref engine) = hook_engine {
                // Get the last user message as the prompt
                let user_prompt = session_messages.last()
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();

                let hook_input = crate::hooks::HookInput::new(crate::hooks::HookEvent::UserPromptSubmit)
                    .with_prompt(&user_prompt);
                let hook_result = engine.run(crate::hooks::HookEvent::UserPromptSubmit, &hook_input).await;

                if !hook_result.allowed {
                    let reason = hook_result.errors.first()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "Hook blocked the request".to_string());
                    let _ = tx.send(AppEvent::ApiError(format!("Blocked by hook: {reason}")));
                    return;
                }

                // Inject any system messages from hooks
                for output in &hook_result.outputs {
                    if let Some(ref sys_msg) = output.system_message {
                        session_messages.push(serde_json::json!({
                            "role": "system",
                            "content": sys_msg
                        }));
                    }
                }
            }

            // Run the turn (may include tool execution)
            match crate::pipeline::run_turn(
                &client, &endpoint, &headers, &request_body, &provider, memory_db.clone(), tx.clone(),
            )
            .await
            {
                Ok(turn_result) => {
                    // If the model returned tool calls, we need to append the
                    // assistant message + tool results and send a follow-up.
                    if turn_result.needs_followup {
                        // Build assistant message with tool calls
                        let assistant_msg =
                            crate::pipeline::build_assistant_message_with_tools(
                                &turn_result.content,
                                &turn_result.tool_calls,
                                &provider,
                            );
                        session_messages.push(assistant_msg);
                        // Append tool results
                        session_messages.extend(turn_result.tool_results.iter().cloned());

                        // Agentic loop: keep calling until no more tool calls
                        let max_iterations = 25u32;
                        let mut iteration = 0u32;
                        let mut current_messages = session_messages;
                        loop {
                            iteration += 1;
                            if iteration > max_iterations {
                                let _ = tx.send(AppEvent::ApiError(
                                    "Reached maximum tool iterations (25)".to_string(),
                                ));
                                break;
                            }

                            let followup_body = crate::pipeline::build_request(
                                &provider,
                                &model,
                                &current_messages,
                                &effort_level,
                                claude_code_token.as_deref(),
                            );

                            match crate::pipeline::run_turn(
                                &client,
                                &endpoint,
                                &headers,
                                &followup_body,
                                &provider,
                                memory_db.clone(),
                                tx.clone(),
                            )
                            .await
                            {
                                Ok(followup) => {
                                    if followup.needs_followup {
                                        let asst_msg =
                                            crate::pipeline::build_assistant_message_with_tools(
                                                &followup.content,
                                                &followup.tool_calls,
                                                &provider,
                                            );
                                        current_messages.push(asst_msg);
                                        current_messages.extend(
                                            followup.tool_results.iter().cloned(),
                                        );
                                        // continue loop
                                    } else {
                                        // Done — add final assistant message
                                        if !followup.content.is_empty() {
                                            current_messages.push(serde_json::json!({
                                                "role": "assistant",
                                                "content": followup.content
                                            }));
                                        }
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AppEvent::ApiError(e));
                                    break;
                                }
                            }
                        }
                        // Sync updated messages back to the App
                        let _ = tx.send(AppEvent::SyncMessages(current_messages));
                        let _ = tx.send(AppEvent::ResponseDone);
                    } else {
                        // No tool calls — add assistant text to session
                        if !turn_result.content.is_empty() {
                            session_messages.push(serde_json::json!({
                                "role": "assistant",
                                "content": turn_result.content
                            }));
                            let _ = tx.send(AppEvent::SyncMessages(session_messages));
                        }
                        // ResponseDone already sent by run_turn
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ApiError(e));
                }
            }
        });
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8), // Welcome box
                Constraint::Min(3),    // Messages
                Constraint::Length(3), // Input
                Constraint::Length(1), // Status
            ])
            .split(frame.area());

        // ── Welcome box (two-column, bordered) ──
        self.draw_welcome_box(frame, chunks[0]);

        // ── Messages ──
        self.messages.render(frame, chunks[1]);

        // ── Input area ──
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Rgb(128, 128, 128)));

        let prompt_text = if self.is_waiting {
            format!("{} ", SPINNER_FRAMES[self.spinner_frame])
        } else {
            "\u{203A} ".to_string()
        };
        let display_text = format!("{prompt_text}{}", self.input.content);

        let input_para = Paragraph::new(display_text)
            .block(input_block)
            .style(Style::default().fg(Color::White));
        frame.render_widget(input_para, chunks[2]);

        // Cursor
        if !self.is_waiting {
            #[allow(clippy::cast_possible_truncation)]
            let prompt_width = 2u16;
            let cx = chunks[2].x + prompt_width + self.input.cursor_pos as u16;
            let cy = chunks[2].y + 1;
            frame.set_cursor_position(Position::new(
                cx.min(chunks[2].right().saturating_sub(1)),
                cy,
            ));
        }

        // ── Status bar ──
        let left_text = "? for shortcuts";
        let effort_symbol = match self.effort_level.as_str() {
            "low" => "\u{25CB}",
            "high" => "\u{25CF}",
            _ => "\u{25D0}",
        };
        let right_text = format!("{effort_symbol} {} \u{00B7} /effort", self.effort_level);

        let bar_width = chunks[3].width as usize;
        let content_len = left_text.len() + right_text.len() + 2;
        let padding = bar_width.saturating_sub(content_len);
        let status_text = format!(
            " {left_text}{}{right_text} ",
            " ".repeat(padding)
        );

        let status = Paragraph::new(status_text)
            .style(Style::default().fg(Color::Rgb(128, 128, 128)));
        frame.render_widget(status, chunks[3]);

        // ── Permission prompt overlay ──
        if let Some(ref perm) = self.pending_permission {
            let area = frame.area();
            // Center a dialog box
            let dialog_width = area.width.min(70);
            let dialog_height = 7u16;
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = area.height.saturating_sub(dialog_height + 4);
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

            // Clear the area behind the dialog
            let clear = Paragraph::new("")
                .style(Style::default().bg(Color::Black));
            frame.render_widget(clear, dialog_area);

            let args_preview = if perm.tool_args.len() > 50 {
                format!("{}...", crate::tools::safe_truncate(&perm.tool_args, 47))
            } else {
                perm.tool_args.clone()
            };

            let prompt_text = vec![
                Line::from(Span::styled(
                    format!("  Tool: {}", perm.tool_name),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("  Args: {args_preview}"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  [y] ", Style::default().fg(Color::Green)),
                    Span::raw("Allow  "),
                    Span::styled("[n] ", Style::default().fg(Color::Red)),
                    Span::raw("Deny  "),
                    Span::styled("[a] ", Style::default().fg(Color::Cyan)),
                    Span::raw("Always  "),
                    Span::styled("[d] ", Style::default().fg(Color::Yellow)),
                    Span::raw("Never"),
                ]),
            ];

            let dialog = Paragraph::new(prompt_text)
                .block(
                    Block::default()
                        .title(" Permission Required ")
                        .title_style(Style::default().fg(Color::Rgb(218, 165, 32)).add_modifier(Modifier::BOLD))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Rgb(218, 165, 32))),
                )
                .style(Style::default().bg(Color::Black));
            frame.render_widget(dialog, dialog_area);
        }
    }

    /// Render the welcome box — two-column bordered widget matching the old inline UI.
    fn draw_welcome_box(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::Wrap;

        // Title in the border
        let title = Line::from(vec![
            Span::styled(
                "OpenClaudia",
                Style::default()
                    .fg(Color::Rgb(147, 112, 219))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::Rgb(218, 165, 32)),
            ),
        ]);

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(147, 112, 219)));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Two-column layout
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner);

        // Left column: greeting, provider, model, cwd
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let greeting = if username.is_empty() {
            "Welcome to OpenClaudia!".to_string()
        } else {
            format!("Welcome back, {username}!")
        };
        let cwd = std::env::current_dir()
            .map(|p| {
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rel) = p.strip_prefix(&home) {
                        return format!("~/{}", rel.display());
                    }
                }
                p.display().to_string()
            })
            .unwrap_or_else(|_| ".".to_string());

        let left = Paragraph::new(vec![
            Line::from(Span::styled(
                greeting,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("Provider: {}", super::capitalize_first(&self.provider)),
                Style::default().fg(Color::Rgb(147, 112, 219)),
            )),
            Line::from(Span::styled(
                format!("Model: {}", self.model),
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(cwd, Style::default().fg(Color::DarkGray))),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(left, cols[0]);

        // Right column: tips and recent activity
        let tips = super::get_tips();
        let right = Paragraph::new(vec![
            Line::from(Span::styled(
                "Tips",
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(
                tips[0].to_string(),
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Recent activity",
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(
                "No recent activity",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(right, cols[1]);
    }
}
