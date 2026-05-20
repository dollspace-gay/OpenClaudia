//! Full-screen interactive TUI application.
//!
//! Launched via `openclaudia` (default) or `openclaudia --tui`.
//! Provides a scrollable message view, text input area, status bar,
//! and streaming response display wired to the real API pipeline.

use super::events::{AppEvent, EventHandler, SpawnTarget};
use super::input::TextInput;
use super::messages::{DisplayMessage, EffortLevel, MessageKind, MessageList, Mode};
use super::{DIM, GOLD, PURPLE, SPINNER_FRAMES};
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

use crate::file_error::{self, FileError};

/// Process-wide shutdown flag for the TUI event loop.
///
/// crosslink #910: the original `run()` loop relied entirely on the
/// `should_quit` field plus the event channel — there was no way for
/// out-of-band code (a tokio signal handler, an integration test, the
/// proxy's `/shutdown` endpoint) to ask the loop to exit without
/// synthesising a keypress. This `AtomicBool` is checked at the top of
/// every tick, giving any caller a lock-free, panic-safe way to bring
/// the UI down cleanly.
///
/// Set via [`request_tui_shutdown`]. The flag is sticky for the
/// lifetime of the process — once shutdown is requested, every future
/// TUI invocation will exit immediately. That is intentional: a
/// shutdown request that survives a restart is what an embedded host
/// (or a watchdog) wants.
pub static TUI_SHUTDOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Signal the TUI event loop to exit at the next tick.
///
/// Safe to call from any thread, including signal handlers — the
/// underlying store is lock-free.
pub fn request_tui_shutdown() {
    TUI_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Chat session state — compatible with the CLI's `ChatSession` JSON format
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
    pub mode: Mode,
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
            mode: Mode::Build,
            messages: Vec::new(),
            undo_stack: Vec::new(),
        }
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }

    fn update_title(&mut self) {
        if let Some(first_user) = self
            .messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        {
            if let Some(content) = first_user.get("content").and_then(|c| c.as_str()) {
                self.title = if content.len() > 50 {
                    format!("{}...", crate::tools::safe_truncate(content, 47))
                } else {
                    content.to_string()
                };
            }
        }
    }

    const fn toggle_mode(&mut self) {
        self.mode = self.mode.toggled();
    }

    const fn mode_description(&self) -> &'static str {
        self.mode.description()
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
        self.messages
            .iter()
            .map(|m| {
                m.get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .len()
                    / 4
                    + 4
            })
            .sum()
    }
}

/// Compiled regex for `@"quoted path"` and `@bare-path` file references.
///
/// The pattern is a hard-coded literal that must compile; `expect` is the
/// correct idiom here (`unwrap` would give a less actionable panic message).
static FILE_REF_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"@"([^"]+)"|@(\S+)"#)
        .expect("FILE_REF_RE pattern is a hard-coded literal — must compile")
});

/// Expand @filename references in user input by inlining file contents.
fn expand_file_refs(input: &str) -> String {
    if !input.contains('@') {
        return input.to_string();
    }
    let mut result = input.to_string();
    let mut replacements = Vec::new();

    // Get project root for path traversal validation
    let cwd = std::env::current_dir().unwrap_or_default();

    for cap in FILE_REF_RE.captures_iter(input) {
        let full_match = match cap.get(0) {
            Some(m) => m.as_str(),
            None => continue,
        };
        let raw_path = match cap.get(1).or_else(|| cap.get(2)) {
            Some(m) => m.as_str(),
            None => continue,
        };

        // Resolve and validate path — reject traversal attempts
        let resolved = if std::path::Path::new(raw_path).is_absolute() {
            std::path::PathBuf::from(raw_path)
        } else {
            cwd.join(raw_path)
        };

        // Reject paths with .. components
        if resolved
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            replacements.push((
                full_match.to_string(),
                format!("[Path traversal blocked: {raw_path}]"),
            ));
            continue;
        }

        // #818: open-then-read on a single file descriptor.  The previous
        // canonicalize → read_to_string pair was a TOCTOU window — between
        // the two syscalls the path could be replaced with a symlink to an
        // arbitrary file.  We now open the file first (yielding an fd
        // pinned to one inode), then validate the canonical path of the
        // already-resolved name, then read from the same fd.  Any post-open
        // symlink flip is irrelevant — the kernel keeps reading the
        // originally-opened inode.
        let Ok(mut file) = std::fs::File::open(&resolved) else {
            replacements.push((
                full_match.to_string(),
                format!("[File not found: {raw_path}]"),
            ));
            continue;
        };
        // Canonicalize for the containment check.  Even if the symlink
        // chain is swapped between the open() above and this canonicalize,
        // the file we will actually read is the inode pinned by `file`.
        let Ok(canonical) = std::fs::canonicalize(&resolved) else {
            replacements.push((
                full_match.to_string(),
                format!("[File not found: {raw_path}]"),
            ));
            continue;
        };
        if !canonical.starts_with(&cwd) {
            replacements.push((
                full_match.to_string(),
                format!("[File outside project directory: {raw_path}]"),
            ));
            continue;
        }
        let mut content = String::new();
        match std::io::Read::read_to_string(&mut file, &mut content) {
            Ok(_) => {
                replacements.push((
                    full_match.to_string(),
                    format!(
                        "\n<file path=\"{}\">\n{}\n</file>\n",
                        canonical.display(),
                        content.trim()
                    ),
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

/// Format a [`SystemTime`] as an ISO-8601 date string
/// (`YYYY-MM-DD`). Used only for the log-selector "last activity"
/// column where the exact minute doesn't matter — the picker shows
/// entries newest-first so users orient by relative position, not a
/// wall-clock string. Returns `"?"` on the far-past clock drift case
/// where the timestamp predates the Unix epoch.
fn iso_of_systemtime(t: std::time::SystemTime) -> String {
    match chrono::DateTime::<chrono::Utc>::from(t)
        .format("%Y-%m-%d")
        .to_string()
    {
        s if s.is_empty() => "?".to_string(),
        s => s,
    }
}

fn save_session(session: &TuiSession) -> Result<(), FileError> {
    let dir = sessions_dir();
    file_error::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", session.id));
    match file_error::write_json_pretty(&path, session) {
        Ok(()) => Ok(()),
        Err(err) => {
            // crosslink #889: a single un-serializable message previously
            // failed the *whole* save, losing every message in the buffer.
            // Try a degraded save where messages that fail to serialize
            // are replaced with a placeholder — operator sees the loss
            // explicitly in the saved transcript instead of losing
            // everything silently.
            tracing::warn!(
                error = %err,
                "save_session: full save failed; attempting per-message recovery"
            );
            save_session_with_recovery(session, &path)
        }
    }
}

/// Best-effort recovery save: drop messages that fail individual
/// serialization, replace each with a `{"role":"system","content":"[message
/// lost: ...]"}` marker so the conversation history is reconstructable.
///
/// The path is reused (no second `create_dir_all` needed — the original
/// `save_session` already created the directory).
fn save_session_with_recovery(session: &TuiSession, path: &std::path::Path) -> Result<(), FileError> {
    let mut salvaged = session.clone();
    let mut lost = 0usize;
    for msg in &mut salvaged.messages {
        if serde_json::to_string(msg).is_err() {
            *msg = serde_json::json!({
                "role": "system",
                "content": "[message lost during persistence — original was not serializable]",
            });
            lost += 1;
        }
    }
    if lost > 0 {
        tracing::warn!(
            lost,
            session_id = %salvaged.id,
            "save_session: replaced {lost} unserializable message(s) with placeholders"
        );
    }
    file_error::write_json_pretty(path, &salvaged)
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
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
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
    pub mode: Mode,
    pub should_quit: bool,
    pub is_waiting: bool,
    spinner_frame: usize,
    /// Sender for pushing API events into the event loop's channel.
    api_event_tx: Option<std::sync::mpsc::Sender<AppEvent>>,

    // ── API pipeline fields ──
    pub client: reqwest::Client,
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub effort_level: EffortLevel,
    pub system_prompt: String,
    /// Split system prompt blocks for Anthropic cache efficiency.
    pub prompt_blocks: Option<crate::prompt::SystemPromptBlocks>,
    pub claude_code_token: Option<String>,
    /// Memory database for auto-learning from tool execution.
    pub memory_db: Option<std::sync::Arc<crate::memory::MemoryDb>>,
    /// Library-layer permission manager. When `Some`, every tool call routed
    /// through `pipeline::run_turn` consults this gate in addition to the
    /// UX-layer `PermissionResponse` flow — closes crosslink #505.
    pub permission_mgr: Option<std::sync::Arc<crate::permissions::PermissionManager>>,
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
    /// Count of `session_messages` already appended to the Claude Code
    /// JSONL transcript. Everything past this index is persisted on the
    /// next call to `persist_transcript_tail`. Rebuilt to 0 on resume
    /// because resuming re-points at an existing transcript file, so we
    /// want to skip re-appending the already-on-disk history.
    transcript_watermark: usize,
    /// Absolute cwd used for the transcript path. Captured once so
    /// later appends survive the user changing dirs within a skill.
    transcript_cwd: PathBuf,
    /// Active modal overlay (help / log picker / …). At most one at a
    /// time. `None` when the main chat UI has focus. Closing an
    /// overlay goes through its `OverlayAction` return value so the
    /// event loop stays the single owner of App-level state changes.
    overlay: Option<ActiveOverlay>,
}

/// Which overlay component is currently open. Each variant owns its
/// component state directly — the enum is the single-slot union the
/// event loop matches on to dispatch draw / key events.
pub enum ActiveOverlay {
    Help(super::components::HelpOverlay),
    LogSelector(super::components::LogSelector),
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
            mode: Mode::Build,
            should_quit: false,
            is_waiting: false,
            spinner_frame: 0,
            api_event_tx: None,
            client: reqwest::Client::new(),
            endpoint: String::new(),
            headers: Vec::new(),
            effort_level: EffortLevel::Medium,
            system_prompt: String::new(),
            prompt_blocks: None,
            claude_code_token: None,
            memory_db: None,
            permission_mgr: None,
            session_messages: Vec::new(),
            runtime_handle: None,
            chat_session: TuiSession::new(model, provider),
            pending_permission: None,
            hook_engine: None,
            rules_content: None,
            rules_injected: false,
            transcript_watermark: 0,
            transcript_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            overlay: None,
        }
    }

    /// Open the help-cheatsheet overlay. Subsequent keystrokes go to
    /// the overlay until it returns `OverlayAction::Close`.
    pub fn open_help_overlay(&mut self) {
        self.overlay = Some(ActiveOverlay::Help(super::components::HelpOverlay::new()));
    }

    /// Resume the session whose id equals or prefix-matches `id`.
    /// Shared between the log-selector overlay (exact id) and the
    /// `/load` / `/continue` text commands (prefix match). No-op
    /// with a user-visible system message when no match is found.
    fn resume_session_by_id(&mut self, id: &str) {
        let sessions = list_sessions();
        let Some(loaded) = sessions.iter().find(|s| s.id.starts_with(id)).cloned() else {
            self.messages.add(DisplayMessage::error(format!(
                "No session found with id prefix '{id}'.",
            )));
            return;
        };
        self.chat_session.clone_from(&loaded);
        self.session_messages.clone_from(&loaded.messages);
        self.model.clone_from(&loaded.model);
        self.provider.clone_from(&loaded.provider);
        self.mode = loaded.mode;
        self.tokens = self.chat_session.estimate_tokens();
        self.transcript_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.transcript_watermark = self.session_messages.len();
        // Repaint the transcript.
        self.messages = super::messages::MessageList::new();
        for msg in &loaded.messages {
            let role: super::messages::Role = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("system")
                .parse()
                .unwrap_or(super::messages::Role::System);
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if role == super::messages::Role::System {
                continue;
            }
            let kind = match role {
                super::messages::Role::User => MessageKind::User,
                super::messages::Role::Assistant => MessageKind::Assistant,
                super::messages::Role::Tool => MessageKind::ToolOk {
                    name: String::new(),
                },
                super::messages::Role::System => MessageKind::SystemInfo,
            };
            self.messages.add(DisplayMessage {
                kind,
                content: content.to_string(),
            });
        }
    }

    /// Open the log-selector (session picker) overlay seeded with
    /// every transcript for the current project's cwd. No-op when
    /// there are zero saved sessions — the caller should show a
    /// different affordance in that case (current behavior: the
    /// overlay still opens with an empty-state message, matching
    /// Claude Code's `/resume` UX).
    pub fn open_log_selector(&mut self) {
        let transcripts = crate::transcript::list_transcripts(&self.transcript_cwd);
        let rows = transcripts
            .into_iter()
            .map(|info| super::components::log_selector::SessionRow {
                session_id: info.session_id,
                first_prompt: info.first_prompt,
                message_count: info.message_count,
                modified_iso: iso_of_systemtime(info.modified),
            })
            .collect();
        self.overlay = Some(ActiveOverlay::LogSelector(
            super::components::LogSelector::new(rows),
        ));
    }

    /// Fire the `Stop` hook. Invoked when a turn reaches a terminal
    /// assistant response (no further tool-call follow-up). Best-effort
    /// — runtime/engine absence short-circuits silently.
    fn fire_stop_hook(&self) {
        if let (Some(engine), Some(handle)) =
            (self.hook_engine.as_ref(), self.runtime_handle.as_ref())
        {
            let engine = engine.clone();
            let session_id = self.chat_session.id.clone();
            handle.spawn(async move {
                let input = crate::hooks::HookInput::new(crate::hooks::HookEvent::Stop)
                    .with_session_id(session_id);
                let _ = engine.run(crate::hooks::HookEvent::Stop, &input).await;
            });
        }
    }

    /// Fire the `Notification` hook with a free-form message. Used for
    /// API errors, rate-limit warnings, etc. Best-effort as above.
    fn fire_notification_hook(&self, message: &str, level: &str) {
        if let (Some(engine), Some(handle)) =
            (self.hook_engine.as_ref(), self.runtime_handle.as_ref())
        {
            let engine = engine.clone();
            let session_id = self.chat_session.id.clone();
            let message = message.to_string();
            let level = level.to_string();
            handle.spawn(async move {
                let payload = serde_json::json!({
                    "message": message,
                    "level": level.clone(),
                    "session_id": session_id,
                });
                let _ = engine.fire_notification(&level, payload).await;
            });
        }
    }

    /// Append every `session_messages` entry past the watermark to the
    /// Claude Code-layout JSONL transcript at
    /// `$CLAUDE_CONFIG_HOME_DIR/projects/<sanitized-cwd>/<session>.jsonl`.
    /// Best-effort: transcript I/O failures are logged but never bubble
    /// up — a missing transcript must never break the live turn.
    fn persist_transcript_tail(&mut self) {
        let cwd = self.transcript_cwd.clone();
        let session_id = self.chat_session.id.clone();
        // crosslink #709: track ONLY the entries that were actually
        // persisted. The previous implementation unconditionally jumped
        // the watermark to `session_messages.len()` after an early break,
        // which silently dropped every message past the failure point
        // from the transcript permanently (the next call would skip them
        // entirely). Advance by the appended count so retried calls
        // resume exactly where the failure occurred.
        let start = self.transcript_watermark;
        let total = self.session_messages.len();
        let mut appended: usize = 0;
        for msg in &self.session_messages[start..] {
            let kind = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("system")
                .to_string();
            let entry =
                crate::transcript::envelope_for(&kind, &cwd, &session_id, Some(msg.clone()));
            match crate::transcript::append_entry(&cwd, &session_id, &entry) {
                Ok(()) => appended += 1,
                Err(err) => {
                    let remaining = total - start - appended;
                    tracing::warn!(
                        error = %err,
                        appended,
                        remaining,
                        "transcript append failed; watermark advanced only over persisted entries"
                    );
                    break;
                }
            }
        }
        self.transcript_watermark = start + appended;
    }

    /// Set the API connection details needed to make requests.
    pub fn set_api_config(
        &mut self,
        endpoint: String,
        headers: Vec<(String, String)>,
        system_prompt: String,
        prompt_blocks: Option<crate::prompt::SystemPromptBlocks>,
        claude_code_token: Option<String>,
    ) {
        self.endpoint = endpoint;
        self.headers = headers;
        self.system_prompt = system_prompt;
        self.prompt_blocks = prompt_blocks;
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
            // crosslink #910: out-of-band shutdown signal. Any process
            // (signal handler, background task, test fixture) can
            // request a clean exit by flipping TUI_SHUTDOWN — the loop
            // checks it before every tick so we exit promptly without
            // a synthetic keypress.
            if TUI_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            terminal.draw(|frame| self.draw(frame))?;
            if !self.handle_app_event(events.next()) {
                break;
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

        // Fire SessionEnd hooks. Best-effort: the app is already exiting
        // so we can't recover from a failure, and we must not spam the
        // terminal (already restored from alt-screen). The hook engine
        // owns its own error logging via tracing.
        if let (Some(engine), Some(handle)) =
            (self.hook_engine.as_ref(), self.runtime_handle.as_ref())
        {
            let session_id = self.chat_session.id.clone();
            let engine = engine.clone();
            handle.block_on(async move {
                let input = crate::hooks::HookInput::new(crate::hooks::HookEvent::SessionEnd)
                    .with_session_id(session_id);
                let _ = engine
                    .run(crate::hooks::HookEvent::SessionEnd, &input)
                    .await;
            });
        }

        Ok(())
    }

    /// Process one async event from the event loop. Returns `false` when the loop should stop.
    fn handle_app_event(&mut self, event: Result<AppEvent, std::sync::mpsc::RecvError>) -> bool {
        match event {
            Ok(AppEvent::Key(key)) => self.handle_key(key),
            Ok(AppEvent::Tick) => {
                self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
            }
            Ok(AppEvent::StreamText(text)) => {
                self.messages.finish_thinking();
                self.messages.append_streaming(&text);
                self.messages.scroll_to_bottom();
            }
            Ok(AppEvent::StreamThinking(text)) => {
                self.messages.push_thinking(&text);
                self.messages.scroll_to_bottom();
            }
            Ok(AppEvent::ToolStart { name, description }) => {
                self.messages.add(DisplayMessage {
                    kind: MessageKind::ToolStart { name },
                    content: description,
                });
            }
            Ok(AppEvent::ToolDone {
                name,
                success,
                content,
            }) => {
                let preview = if content.len() > 300 {
                    format!("{}...", crate::tools::safe_truncate(&content, 297))
                } else {
                    content
                };
                self.messages.add(DisplayMessage {
                    kind: if success {
                        MessageKind::ToolOk { name }
                    } else {
                        MessageKind::ToolErr { name }
                    },
                    content: preview,
                });
            }
            Ok(AppEvent::ResponseDone) => {
                self.messages.finish_thinking();
                self.messages.finish_streaming();
                self.is_waiting = false;
                self.chat_session.messages = self.session_messages.clone();
                self.chat_session.update_title();
                self.chat_session.touch();
                let _ = save_session(&self.chat_session);
                self.persist_transcript_tail();
                self.tokens = self.chat_session.estimate_tokens();
                self.fire_stop_hook();
            }
            Ok(AppEvent::ApiError(msg)) => {
                self.messages.finish_streaming();
                self.messages
                    .add(DisplayMessage::error(format!("Error: {msg}")));
                self.is_waiting = false;
                self.fire_notification_hook(&format!("API error: {msg}"), "error");
            }
            Ok(AppEvent::Resize(_, _)) => {}
            Ok(AppEvent::FollowUp) => {
                self.spawn_api_turn();
            }
            Ok(AppEvent::SyncMessages(messages)) => {
                self.session_messages = messages;
            }
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
            Ok(AppEvent::ShellDone {
                target,
                stdout,
                stderr,
                exit_code,
            }) => {
                self.handle_shell_done(target, &stdout, &stderr, exit_code);
            }
            Err(_) => return false,
        }
        true
    }

    /// Render the result of a backgrounded shell call dispatched via
    /// [`Self::spawn_shell`]. Closes crosslink #371: the same rendering
    /// logic that used to live inline next to a blocking `.output()` call
    /// now runs on the UI thread *after* the child has exited on the
    /// tokio runtime, so the event loop never stalls.
    fn handle_shell_done(
        &mut self,
        target: SpawnTarget,
        stdout: &str,
        stderr: &str,
        exit_code: Option<i32>,
    ) {
        match target {
            SpawnTarget::Diff => {
                let content = if exit_code.is_none() {
                    format!("Failed to run git diff: {stderr}")
                } else if stdout.is_empty() {
                    "No uncommitted changes.".to_string()
                } else {
                    format!("Uncommitted changes:\n{stdout}")
                };
                self.messages.add(DisplayMessage::system(content));
            }
            SpawnTarget::Review => {
                let content = if exit_code.is_none() {
                    format!("Failed to run git diff: {stderr}")
                } else if stdout.is_empty() {
                    "No changes to review.".to_string()
                } else {
                    let total = stdout.lines().count();
                    let lines: Vec<&str> = stdout.lines().take(100).collect();
                    if total > 100 {
                        format!("{}\n... (truncated, {total} total lines)", lines.join("\n"))
                    } else {
                        lines.join("\n")
                    }
                };
                self.messages.add(DisplayMessage::system(content));
            }
            SpawnTarget::Init => {
                let content = if exit_code.is_none() {
                    format!("Init failed: {stderr}")
                } else {
                    stdout.to_string()
                };
                self.messages.add(DisplayMessage::system(content));
            }
            SpawnTarget::Files | SpawnTarget::Doctor => {
                // Reserved for follow-up #371 migration — these branches are
                // not yet routed through spawn_shell (they don't invoke a
                // child process today), so receiving one is a logic bug
                // rather than user-visible state. Render defensively.
                let content = if exit_code.is_none() {
                    format!("Command failed: {stderr}")
                } else {
                    stdout.to_string()
                };
                self.messages.add(DisplayMessage::system(content));
            }
            SpawnTarget::ShellCommand { displayed } => {
                let success = matches!(exit_code, Some(0));
                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(stderr);
                }
                let header = format!("$ {displayed}");
                if exit_code.is_none() {
                    self.messages.add(DisplayMessage {
                        kind: MessageKind::ToolErr { name: header },
                        content: format!("Failed: {stderr}"),
                    });
                    return;
                }
                if result.is_empty() {
                    result = "(no output)".to_string();
                }
                self.messages.add(DisplayMessage {
                    kind: if success {
                        MessageKind::ToolOk { name: header }
                    } else {
                        MessageKind::ToolErr { name: header }
                    },
                    content: result,
                });
            }
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // Modal overlays consume every keystroke while open. Ordering
        // matters: overlay handling comes BEFORE the global Ctrl+C
        // quit so dismissing the overlay with Ctrl+C doesn't tear
        // down the whole app.
        if self.overlay.is_some() {
            use super::components::{Overlay as _, OverlayAction};
            let action = match self.overlay.as_mut().unwrap() {
                ActiveOverlay::Help(o) => o.handle_key(key),
                ActiveOverlay::LogSelector(o) => o.handle_key(key),
            };
            match action {
                OverlayAction::Consumed => {}
                OverlayAction::Close => {
                    self.overlay = None;
                }
                OverlayAction::ResumeSession(id) => {
                    self.overlay = None;
                    self.resume_session_by_id(&id);
                }
            }
            return;
        }

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
            self.handle_permission_key(key);
            return;
        }

        self.handle_editing_key(key);
    }

    /// Dispatch keystrokes when a permission prompt is active.
    fn handle_permission_key(&mut self, key: crossterm::event::KeyEvent) {
        use super::events::PermissionResponse;
        let response = match key.code {
            KeyCode::Char('y' | 'Y') => Some(PermissionResponse::Allow),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(PermissionResponse::Deny),
            KeyCode::Char('a' | 'A') => Some(PermissionResponse::AlwaysAllow),
            KeyCode::Char('d' | 'D') => Some(PermissionResponse::AlwaysDeny),
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
                let denied = matches!(
                    resp,
                    PermissionResponse::Deny | PermissionResponse::AlwaysDeny
                );
                let content = format!("{label}: {}", perm.tool_name);
                self.messages.add(if denied {
                    DisplayMessage::error(content)
                } else {
                    DisplayMessage::system(content)
                });
                let _ = perm.reply.send(resp);
            }
        }
    }

    /// Dispatch keystrokes for normal editing / streaming-cancel.
    fn handle_editing_key(&mut self, key: crossterm::event::KeyEvent) {
        // During streaming, Escape cancels
        if self.is_waiting {
            if key.code == KeyCode::Esc {
                self.is_waiting = false;
                self.messages.finish_streaming();
                self.messages
                    .add(DisplayMessage::system("[Response interrupted]"));
            }
            return;
        }

        match key.code {
            KeyCode::Enter if !self.input.is_empty() => {
                let text = self.input.take();
                self.handle_input(text);
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

    /// Handle user input: dispatch to slash commands, shell commands, or API.
    fn handle_input(&mut self, text: String) {
        // Shell commands: !command
        if let Some(cmd) = text.strip_prefix('!') {
            self.handle_shell_command(cmd.trim());
            return;
        }

        // Slash commands: /command
        if text.starts_with('/') || text == "?" {
            if self.handle_slash_command(&text) {
                return;
            }
            // Unknown command — fall through handled inside handle_slash_command
            return;
        }

        // Normal message → send to API
        self.send_user_message(text);
    }

    /// Handle session-management slash commands. Returns true if handled.
    fn handle_session_slash(&mut self, text: &str) -> bool {
        if text == "/sessions" || text == "/list" {
            let sessions = list_sessions();
            if sessions.is_empty() {
                self.messages
                    .add(DisplayMessage::system("No saved sessions."));
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
                self.messages.add(DisplayMessage::system(format!(
                    "Saved sessions:\n{list}\n\nUse /load <id> to resume."
                )));
            }
            return true;
        }
        if text.starts_with("/load ") || text.starts_with("/continue ") {
            let id = text.split_whitespace().nth(1).unwrap_or("");
            self.resume_session_by_id(id);
            return true;
        }
        if text == "/rewind" || text.starts_with("/rewind ") {
            self.handle_rewind(text);
            return true;
        }
        if text == "/undo" {
            if self.chat_session.undo() {
                self.session_messages = self.chat_session.messages.clone();
                if self.messages.len() >= 2 {
                    self.messages.pop_last(2);
                }
                self.messages
                    .add(DisplayMessage::system("Undone last message pair."));
                let _ = save_session(&self.chat_session);
            } else {
                self.messages
                    .add(DisplayMessage::system("Nothing to undo."));
            }
            return true;
        }
        if text == "/redo" {
            if self.chat_session.redo() {
                self.session_messages = self.chat_session.messages.clone();
                self.messages
                    .add(DisplayMessage::system("Redone last undone messages."));
                let _ = save_session(&self.chat_session);
            } else {
                self.messages
                    .add(DisplayMessage::system("Nothing to redo."));
            }
            return true;
        }
        false
    }

    /// Handle /rewind subcommand.
    fn handle_rewind(&mut self, text: &str) {
        use std::fmt::Write as _;
        let arg = text.strip_prefix("/rewind").unwrap_or("").trim();
        if arg.is_empty() {
            let mut turn_list = String::new();
            let mut turn_num = 0;
            for msg in &self.chat_session.messages {
                if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                    turn_num += 1;
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let preview = if content.len() > 60 {
                        format!("{}...", crate::tools::safe_truncate(content, 57))
                    } else {
                        content.to_string()
                    };
                    let _ = writeln!(turn_list, "  {turn_num}. {preview}");
                }
            }
            if turn_list.is_empty() {
                turn_list = "  (no conversation turns yet)\n".to_string();
            }
            self.messages.add(DisplayMessage::system(format!("Conversation has {turn_num} turn(s):\n{turn_list}\nUse /rewind N to undo the last N turns.")));
        } else if let Ok(n) = arg.parse::<usize>() {
            if n == 0 {
                self.messages
                    .add(DisplayMessage::system("Nothing to rewind (0 turns)."));
            } else {
                let mut rewound = 0;
                for _ in 0..n {
                    if self.chat_session.undo() {
                        rewound += 1;
                    } else {
                        break;
                    }
                }
                if rewound > 0 {
                    self.session_messages = self.chat_session.messages.clone();
                    let to_remove = rewound * 2;
                    if self.messages.len() >= to_remove {
                        self.messages.pop_last(to_remove);
                    }
                    self.messages.add(DisplayMessage::system(format!(
                        "Rewound {rewound} turn(s)."
                    )));
                    let _ = save_session(&self.chat_session);
                    self.persist_transcript_tail();
                } else {
                    self.messages
                        .add(DisplayMessage::system("Nothing to rewind."));
                }
            }
        } else {
            self.messages.add(DisplayMessage::system(
                "Usage: /rewind [N] — rewind N turns, or show turn list",
            ));
        }
    }

    /// Handle /export and /effort slash commands. Returns true if handled.
    fn handle_export_effort_slash(&mut self, text: &str) -> bool {
        if text == "/export" {
            use std::fmt::Write as _;
            let mut md = format!("# {}\n\n", self.chat_session.title);
            let _ = write!(
                md,
                "Model: {} · Provider: {} · {}\n\n---\n\n",
                self.model,
                self.provider,
                self.chat_session.created_at.format("%Y-%m-%d %H:%M")
            );
            for msg in &self.session_messages {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                if role == "system" {
                    continue;
                }
                let _ = write!(md, "**{role}:**\n{content}\n\n");
            }
            let export_path = format!("conversation-{}.md", &self.chat_session.id[..8]);
            match std::fs::write(&export_path, &md) {
                Ok(()) => self
                    .messages
                    .add(DisplayMessage::system(format!("Exported to {export_path}"))),
                Err(e) => self
                    .messages
                    .add(DisplayMessage::error(format!("Export failed: {e}"))),
            }
            return true;
        }
        if text.starts_with("/effort") {
            let parts: Vec<&str> = text.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let level = parts[1].trim();
                // FromStr for EffortLevel is Infallible; unknown strings map to Medium.
                let parsed: EffortLevel = level.parse().unwrap_or(EffortLevel::Medium);
                if matches!(
                    parsed,
                    EffortLevel::Low | EffortLevel::Medium | EffortLevel::High
                ) {
                    self.effort_level = parsed;
                }
            } else {
                self.effort_level = self.effort_level.cycled();
            }
            self.messages.add(DisplayMessage::system(format!(
                "Effort level: {}",
                self.effort_level
            )));
            return true;
        }
        false
    }

    /// Handle slash commands. Returns true if the command was recognized.
    fn handle_slash_command(&mut self, text: &str) -> bool {
        if text == "/quit" || text == "/exit" {
            self.should_quit = true;
            return true;
        }

        if text == "/help" || text == "?" {
            // Rich scrollable overlay — handled by the help component.
            // Falls back to a plain inline message when the overlay is
            // unavailable (e.g. during headless tests) — unreachable
            // in the interactive TUI but kept for defence-in-depth.
            self.open_help_overlay();
            return true;
        }

        if text == "/resume" || text == "/continue" {
            // No-arg form → open the picker overlay. Args form is
            // handled by the existing `/load <id>` / `/continue <id>`
            // branch below.
            self.open_log_selector();
            return true;
        }

        if text == "/clear" {
            self.messages = MessageList::new();
            // Reset session but keep system prompt
            self.session_messages
                .retain(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));
            return true;
        }

        if text == "/status" {
            self.messages.add(DisplayMessage::system(format!(
                "Model: {}\nProvider: {}\nEffort: {}\nMessages: {}\n~{} tokens",
                self.model,
                self.provider,
                self.effort_level,
                self.session_messages.len(),
                self.tokens,
            )));
            return true;
        }

        if text == "/mode" {
            self.chat_session.toggle_mode();
            self.mode = self.chat_session.mode;
            self.messages.add(DisplayMessage::system(format!(
                "Mode: {} — {}",
                self.chat_session.mode,
                self.chat_session.mode_description()
            )));
            return true;
        }

        if self.handle_session_slash(text) {
            return true;
        }

        if self.handle_export_effort_slash(text) {
            return true;
        }

        if text == "/skill" || text == "/skills" {
            let skills = crate::skills::load_skills();
            if skills.is_empty() {
                self.messages.add(DisplayMessage::system(
                    "No skills found. Add .md files to .openclaudia/skills/",
                ));
            } else {
                let list = skills
                    .iter()
                    .map(|s| format!("  /{} — {}", s.name, s.description))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.messages
                    .add(DisplayMessage::system(format!("Available skills:\n{list}")));
            }
            return true;
        }

        // Skill invocations and info/diagnostic commands starting with /
        if text.starts_with('/') {
            self.handle_info_slash(text);
            return true;
        }

        false
    }

    /// Handle skill invocations and info/diagnostic commands.
    fn handle_info_slash(&mut self, text: &str) {
        let skill_name = if text.starts_with("/skill ") {
            text.strip_prefix("/skill ").unwrap_or("").trim()
        } else {
            text.strip_prefix('/').unwrap_or("")
        };
        if let Some(skill) = crate::skills::get_skill(skill_name) {
            self.messages.add(DisplayMessage::system(format!(
                "Running skill: /{}",
                skill.name
            )));
            self.session_messages
                .push(serde_json::json!({ "role": "user", "content": skill.prompt }));
            self.is_waiting = true;
            self.spawn_api_turn();
            return;
        }
        if text.starts_with("/rename ") {
            let new_title = text.strip_prefix("/rename ").unwrap_or("").trim();
            if new_title.is_empty() {
                self.messages
                    .add(DisplayMessage::system("Usage: /rename <new title>"));
            } else {
                self.chat_session.title = new_title.to_string();
                self.chat_session.touch();
                let _ = save_session(&self.chat_session);
                self.persist_transcript_tail();
                self.messages.add(DisplayMessage::system(format!(
                    "Session renamed to: {new_title}"
                )));
            }
            return;
        }
        if self.handle_diagnostic_slash(text) {
            return;
        }
        self.messages.add(DisplayMessage::system(format!(
            "Unknown command: {text}. Type /help for commands."
        )));
    }

    /// Handle the `/cost` slash command.
    fn handle_slash_cost(&mut self) {
        let tokens = self.chat_session.estimate_tokens();
        let tokens_f64 = f64::from(u32::try_from(tokens).unwrap_or(u32::MAX));
        let cost = match self.model.as_str() {
            m if m.contains("opus") => tokens_f64.mul_add(0.000_015, tokens_f64 * 0.000_075),
            m if m.contains("sonnet") => tokens_f64.mul_add(0.000_003, tokens_f64 * 0.000_015),
            m if m.contains("haiku") => tokens_f64.mul_add(0.000_000_25, tokens_f64 * 0.000_001_25),
            _ => 0.0,
        };
        self.messages.add(DisplayMessage::system(format!(
            "Session cost estimate:\n  ~{tokens} tokens\n  ~${cost:.4}"
        )));
    }

    /// Handle the `/files [dir]` slash command.
    fn handle_slash_files(&mut self, text: &str) {
        let dir = text.strip_prefix("/files").unwrap_or("").trim();
        let dir = if dir.is_empty() { "." } else { dir };
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let mut items: Vec<String> = entries
                    .flatten()
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let suffix = if e.file_type().is_ok_and(|t| t.is_dir()) {
                            "/"
                        } else {
                            ""
                        };
                        format!("  {name}{suffix}")
                    })
                    .collect();
                items.sort();
                self.messages.add(DisplayMessage::system(format!(
                    "Files in {dir}:\n{}",
                    items.join("\n")
                )));
            }
            Err(e) => self
                .messages
                .add(DisplayMessage::error(format!("Failed to list {dir}: {e}"))),
        }
    }

    /// Handle the `/diff` slash command (shows `git diff --stat`).
    ///
    /// Dispatches to the tokio runtime via [`Self::spawn_shell`] — see
    /// crosslink #371. The rendering of the result happens on the next
    /// `AppEvent::ShellDone` tick handled in `handle_app_event`.
    fn handle_slash_diff(&self) {
        // Drop the JoinHandle explicitly: the slash-command call site is
        // fire-and-forget, the receiver lives in the mpsc channel.
        drop(self.spawn_shell(vec!["git", "diff", "--stat"], SpawnTarget::Diff));
    }

    /// Handle the `/doctor` slash command (environment diagnostics).
    fn handle_slash_doctor(&mut self) {
        let checks = [
            match crate::config::load_config() {
                Ok(_) => "✓ Config: loaded".to_string(),
                Err(e) => format!("✗ Config: {e}"),
            },
            format!("✓ Provider: {}", self.provider),
            format!("✓ Model: {}", self.model),
            format!("✓ Endpoint: {}", self.endpoint),
            format!("✓ Skills: {} loaded", crate::skills::load_skills().len()),
            if self.memory_db.is_some() {
                "✓ Memory DB: connected".to_string()
            } else {
                "✗ Memory DB: not available".to_string()
            },
        ];
        self.messages.add(DisplayMessage::system(format!(
            "Diagnostics:\n{}",
            checks.join("\n")
        )));
    }

    /// Handle the `/review` slash command (shows truncated `git diff HEAD`).
    fn handle_slash_review(&mut self) {
        let content = match std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .output()
        {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.is_empty() {
                    "No changes to review.".to_string()
                } else {
                    let lines: Vec<&str> = stdout.lines().take(100).collect();
                    if stdout.lines().count() > 100 {
                        format!(
                            "{}\n... (truncated, {} total lines)",
                            lines.join("\n"),
                            stdout.lines().count()
                        )
                    } else {
                        lines.join("\n")
                    }
                }
            }
            Err(e) => format!("Failed to run git diff: {e}"),
        };
        self.messages.add(DisplayMessage::system(content));
    }

    /// Handle the `/init` slash command (create config if absent).
    fn handle_slash_init(&mut self) {
        if crate::config::config_file_exists() {
            self.messages.add(DisplayMessage::system(
                "Config already exists. Use /doctor to check it.",
            ));
        } else {
            let content = match std::process::Command::new("openclaudia")
                .arg("init")
                .output()
            {
                Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
                Err(e) => format!("Init failed: {e}"),
            };
            self.messages.add(DisplayMessage::system(content));
        }
    }

    /// Handle diagnostic/info slash commands. Returns true if handled.
    fn handle_diagnostic_slash(&mut self, text: &str) -> bool {
        if text == "/cost" {
            self.handle_slash_cost();
            return true;
        }
        if text == "/files" || text.starts_with("/files ") {
            self.handle_slash_files(text);
            return true;
        }
        if text == "/diff" {
            self.handle_slash_diff();
            return true;
        }
        if text == "/context" {
            let msg_count = self.session_messages.len();
            let tokens = self.chat_session.estimate_tokens();
            self.messages.add(DisplayMessage::system(format!(
                "Context usage:\n  Messages: {msg_count}\n  Est. tokens: ~{tokens}\n  Model: {}\n  Provider: {}",
                self.model, self.provider
            )));
            return true;
        }
        if text == "/doctor" {
            self.handle_slash_doctor();
            return true;
        }
        if text == "/review" || text.starts_with("/review ") {
            self.handle_slash_review();
            return true;
        }
        if text == "/init" {
            self.handle_slash_init();
            return true;
        }
        false
    }

    /// Execute a shell command and display its output.
    ///
    /// Dispatches to the tokio runtime via [`Self::spawn_shell`] (crosslink
    /// #371). The previous implementation blocked the sync event loop on
    /// `std::process::Command::new("bash").output()` for the full lifetime
    /// of the child — long-running commands froze the spinner and queued
    /// keypresses. We now post the result back via
    /// [`AppEvent::ShellDone`] for the receiver in `handle_app_event` to
    /// render with the same `$ <cmd>` header as before.
    fn handle_shell_command(&self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        // Drop the JoinHandle explicitly: the shell escape is
        // fire-and-forget, results arrive via AppEvent::ShellDone.
        drop(self.spawn_shell(
            vec!["bash", "-c", cmd],
            SpawnTarget::ShellCommand {
                displayed: cmd.to_string(),
            },
        ));
    }

    /// Send a user message to the API.
    fn send_user_message(&mut self, text: String) {
        let expanded = expand_file_refs(&text);

        self.messages.add(DisplayMessage::user(text));

        self.session_messages.push(serde_json::json!({
            "role": "user",
            "content": expanded
        }));

        // Inject rules as system message on first turn
        if !self.rules_injected {
            if let Some(ref rules) = self.rules_content {
                self.session_messages.insert(
                    0,
                    serde_json::json!({
                        "role": "system",
                        "content": rules
                    }),
                );
            }
            self.rules_injected = true;
        }

        crate::guardrails::reset_turn();
        self.is_waiting = true;
        self.spawn_api_turn();
    }

    /// Spawn a subprocess on the tokio runtime and post the result back
    /// to the TUI event loop as [`AppEvent::ShellDone`].
    ///
    /// This is the seam that closes crosslink #371. Slash commands like
    /// `/diff` and the `!<cmd>` shell escape used to call
    /// `std::process::Command::new(...).output()` directly on the sync
    /// event loop thread, which blocked rendering for the full lifetime
    /// of the child. The helper instead dispatches the work to
    /// `runtime_handle.spawn(...)` using `tokio::process::Command` so
    /// the loop keeps ticking; results arrive asynchronously via the
    /// existing mpsc channel that already carries streaming API events.
    ///
    /// `cmd[0]` is the program; `cmd[1..]` are its args. The empty
    /// vector is a logic bug — we return a no-op join handle instead of
    /// panicking on `split_first` because the caller can be exercised
    /// from outside `run()` (e.g. tests).
    ///
    /// If no runtime is bound yet (`self.runtime_handle == None`) the
    /// helper still returns a `JoinHandle<()>` so the call site has a
    /// single, total signature — it just posts an error `ShellDone`
    /// (`exit_code` = None, stderr explaining the missing runtime) via
    /// `std::thread::spawn`.
    fn spawn_shell(&self, cmd: Vec<&str>, target: SpawnTarget) -> tokio::task::JoinHandle<()> {
        let tx = self.api_event_tx.clone();
        // Eagerly own the argv as Strings — the future outlives `&self`.
        let argv: Vec<String> = cmd.into_iter().map(str::to_owned).collect();

        let Some(handle) = self.runtime_handle.clone() else {
            // No runtime — surface as a failed ShellDone so the receiver
            // still gets called. We need a real JoinHandle to satisfy the
            // return type; spawn an immediately-ready future via a
            // detached single-threaded runtime would re-introduce
            // blocking, so instead we synthesize one through a
            // best-effort tokio::spawn that may itself fail. Falling
            // back to a thread keeps the contract.
            if let Some(tx) = tx {
                let _ = tx.send(AppEvent::ShellDone {
                    target,
                    stdout: String::new(),
                    stderr: "no async runtime bound — cannot spawn shell".to_string(),
                    exit_code: None,
                });
            }
            // We still owe a JoinHandle. Spawn a no-op future on a
            // throwaway runtime so the type checks. This branch is
            // only reachable before `run()` initialises the handle.
            return tokio::runtime::Builder::new_current_thread()
                .build()
                .map_or_else(
                    |_| {
                        // As a last resort, panic — there is literally
                        // no way to manufacture a JoinHandle without a
                        // runtime, and being here means the test
                        // harness is misconfigured.
                        panic!("spawn_shell called with no runtime_handle and no fallback runtime");
                    },
                    |rt| rt.spawn(async {}),
                );
        };

        handle.spawn(async move {
            let Some((exe, rest)) = argv.split_first() else {
                if let Some(tx) = tx {
                    let _ = tx.send(AppEvent::ShellDone {
                        target,
                        stdout: String::new(),
                        stderr: "spawn_shell called with empty argv".to_string(),
                        exit_code: None,
                    });
                }
                return;
            };

            let result = tokio::process::Command::new(exe).args(rest).output().await;

            let evt = match result {
                Ok(out) => AppEvent::ShellDone {
                    target,
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                    exit_code: out.status.code(),
                },
                Err(e) => AppEvent::ShellDone {
                    target,
                    stdout: String::new(),
                    stderr: format!("{e}"),
                    exit_code: None,
                },
            };

            if let Some(tx) = tx {
                let _ = tx.send(evt);
            }
        })
    }

    /// Spawn an async API turn on the tokio runtime.
    ///
    /// Sends events through the event handler's mpsc channel so the
    /// synchronous TUI event loop can display streaming output.
    fn spawn_api_turn(&mut self) {
        let Some(ref handle) = self.runtime_handle else {
            // No async runtime — show fallback message
            self.messages.add(DisplayMessage::error(
                "[No async runtime — cannot call API. Run with tokio.]",
            ));
            self.is_waiting = false;
            return;
        };

        let Some(tx) = self.event_sender() else {
            self.is_waiting = false;
            return;
        };

        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let headers = self.headers.clone();
        let provider = self.provider.clone();
        let model = self.model.clone();
        let effort_level = self.effort_level;
        let claude_code_token = self.claude_code_token.clone();
        let prompt_blocks = self.prompt_blocks.clone();
        let hook_engine = self.hook_engine.clone();
        let session_id_for_task = self.chat_session.id.clone();
        let memory_db = self.memory_db.clone();
        let permission_mgr = self.permission_mgr.clone();
        // Clone session messages so the async task can build follow-up requests
        let session_messages = self.session_messages.clone();

        handle.spawn(run_api_turn_async(ApiTurnParams {
            session_messages,
            client,
            endpoint,
            headers,
            provider,
            model,
            effort_level,
            claude_code_token,
            prompt_blocks,
            memory_db,
            permission_mgr,
            hook_engine,
            session_id: session_id_for_task,
            tx,
        }));
    }

    fn draw(&mut self, frame: &mut Frame) {
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
            .border_style(Style::default().fg(DIM));

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
            let prompt_width = 2u16;
            let cursor_pos = u16::try_from(self.input.cursor_position()).unwrap_or(u16::MAX);
            let cx = chunks[2].x + prompt_width + cursor_pos;
            let cy = chunks[2].y + 1;
            frame.set_cursor_position(Position::new(
                cx.min(chunks[2].right().saturating_sub(1)),
                cy,
            ));
        }

        // ── Status bar ──
        let left_text = "? for shortcuts";
        let effort_symbol = self.effort_level.symbol();
        let right_text = format!("{effort_symbol} {} \u{00B7} /effort", self.effort_level);

        let bar_width = chunks[3].width as usize;
        let content_len = left_text.len() + right_text.len() + 2;
        let padding = bar_width.saturating_sub(content_len);
        let status_text = format!(" {left_text}{}{right_text} ", " ".repeat(padding));

        let status = Paragraph::new(status_text).style(Style::default().fg(DIM));
        frame.render_widget(status, chunks[3]);

        // ── Permission prompt overlay ──
        self.draw_permission_overlay(frame);

        // ── Modal overlay (rendered last so it floats above everything) ──
        // Use `Clear` to blank the underlying region; both overlays paint
        // their own background via the border-block's default bg.
        if let Some(ref mut overlay) = self.overlay {
            use super::components::Overlay as _;
            let area = super::components::centered_rect(60, 60, frame.area());
            frame.render_widget(ratatui::widgets::Clear, area);
            match overlay {
                ActiveOverlay::Help(o) => o.render(frame, area),
                ActiveOverlay::LogSelector(o) => o.render(frame, area),
            }
        }
    }

    /// Render the permission-prompt dialog when one is pending.
    fn draw_permission_overlay(&self, frame: &mut Frame) {
        let Some(ref perm) = self.pending_permission else {
            return;
        };
        let area = frame.area();
        let dialog_width = area.width.min(70);
        let dialog_height = 7u16;
        let x = (area.width.saturating_sub(dialog_width)) / 2;
        let y = area.height.saturating_sub(dialog_height + 4);
        let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
        let clear = Paragraph::new("").style(Style::default().bg(Color::Black));
        frame.render_widget(clear, dialog_area);
        let args_preview = if perm.tool_args.len() > 50 {
            format!("{}...", crate::tools::safe_truncate(&perm.tool_args, 47))
        } else {
            perm.tool_args.clone()
        };
        let prompt_text = vec![
            Line::from(Span::styled(
                format!("  Tool: {}", perm.tool_name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
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
                    .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(GOLD)),
            )
            .style(Style::default().bg(Color::Black));
        frame.render_widget(dialog, dialog_area);
    }

    /// Render the welcome box — two-column bordered widget matching the old inline UI.
    fn draw_welcome_box(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::Wrap;

        // Title in the border
        let title = Line::from(vec![
            Span::styled(
                "OpenClaudia",
                Style::default().fg(PURPLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(GOLD),
            ),
        ]);

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(PURPLE));

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
        let cwd = std::env::current_dir().map_or_else(
            |_| ".".to_string(),
            |p| {
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rel) = p.strip_prefix(&home) {
                        return format!("~/{}", rel.display());
                    }
                }
                p.display().to_string()
            },
        );

        let left = Paragraph::new(vec![
            Line::from(Span::styled(
                greeting,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("Provider: {}", super::capitalize_first(&self.provider)),
                Style::default().fg(PURPLE),
            )),
            Line::from(Span::styled(
                format!("Model: {}", self.model),
                Style::default().fg(GOLD),
            )),
            Line::from(Span::styled(cwd, Style::default().fg(Color::DarkGray))),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(left, cols[0]);

        // Right column: tips and recent activity
        let tips = super::get_tips();
        let right = Paragraph::new(vec![
            Line::from(Span::styled("Tips", Style::default().fg(GOLD))),
            Line::from(Span::styled(
                tips[0].to_string(),
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled("Recent activity", Style::default().fg(GOLD))),
            Line::from(Span::styled(
                "No recent activity",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(right, cols[1]);
    }
}

/// Owned call parameters for one spawned API turn.
struct ApiTurnParams {
    session_messages: Vec<serde_json::Value>,
    client: reqwest::Client,
    endpoint: String,
    headers: Vec<(String, String)>,
    provider: String,
    model: String,
    effort_level: EffortLevel,
    claude_code_token: Option<String>,
    prompt_blocks: Option<crate::prompt::SystemPromptBlocks>,
    memory_db: Option<std::sync::Arc<crate::memory::MemoryDb>>,
    permission_mgr: Option<std::sync::Arc<crate::permissions::PermissionManager>>,
    hook_engine: Option<std::sync::Arc<crate::hooks::HookEngine>>,
    session_id: String,
    tx: std::sync::mpsc::Sender<super::events::AppEvent>,
}

/// Shared context threaded through the agentic follow-up loop.
struct AgenticCtx<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
    headers: &'a [(String, String)],
    provider: &'a str,
    model: &'a str,
    effort_level: &'a str,
    claude_code_token: Option<&'a str>,
    prompt_blocks: Option<&'a crate::prompt::SystemPromptBlocks>,
    memory_db: Option<std::sync::Arc<crate::memory::MemoryDb>>,
    permission_mgr: Option<std::sync::Arc<crate::permissions::PermissionManager>>,
    hook_engine: Option<std::sync::Arc<crate::hooks::HookEngine>>,
    session_id: &'a str,
    tx: &'a std::sync::mpsc::Sender<super::events::AppEvent>,
}

/// Run the pre-turn `UserPromptSubmit` hook. Returns `false` and sends an
/// `ApiError` event if the hook denies the request; injects any system
/// messages from hook outputs and returns `true` on success.
async fn run_preturn_hooks(
    engine: &crate::hooks::HookEngine,
    session_messages: &mut Vec<serde_json::Value>,
    tx: &std::sync::mpsc::Sender<super::events::AppEvent>,
) -> bool {
    let user_prompt = session_messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let hook_input = crate::hooks::HookInput::new(crate::hooks::HookEvent::UserPromptSubmit)
        .with_prompt(&user_prompt);
    let hook_result = engine
        .run(crate::hooks::HookEvent::UserPromptSubmit, &hook_input)
        .await;
    if !hook_result.allowed {
        let reason = hook_result.errors.first().map_or_else(
            || "Hook blocked the request".to_string(),
            std::string::ToString::to_string,
        );
        let _ = tx.send(super::events::AppEvent::ApiError(format!(
            "Blocked by hook: {reason}"
        )));
        return false;
    }
    for output in &hook_result.outputs {
        if let Some(ref sys_msg) = output.system_message {
            session_messages.push(serde_json::json!({ "role": "system", "content": sys_msg }));
        }
    }
    true
}

/// Send an event to the TUI event channel, capturing partial in-flight state
/// when the channel has been closed (e.g. user pressed Esc or the app is
/// shutting down).
///
/// Crosslink #765: previously every `tx.send(...)` site was `let _ = ...`,
/// which silently dropped both the event and any unflushed work — for
/// `SyncMessages` that meant the entire accumulated `session_messages` vector
/// vanished, leaving the next turn to retry from a stale baseline. We now
/// `tracing::warn!` with the event kind and any partial-state counts so an
/// operator running with `RUST_LOG=warn` has a forensic trail. We also
/// best-effort persist the messages to disk so a subsequent run can recover.
fn send_or_warn(
    tx: &std::sync::mpsc::Sender<super::events::AppEvent>,
    event: super::events::AppEvent,
    session_id: &str,
) {
    // Snapshot kind/sizes BEFORE moving the event into `send`, so the warn
    // path can describe what was lost without owning the value.
    let descriptor = describe_event(&event);
    let partial_messages: Option<Vec<serde_json::Value>> = match &event {
        super::events::AppEvent::SyncMessages(msgs) => Some(msgs.clone()),
        _ => None,
    };
    if tx.send(event).is_err() {
        tracing::warn!(
            event = %descriptor,
            session_id = %session_id,
            "TUI event channel closed; partial turn state being persisted to recovery file"
        );
        if let Some(msgs) = partial_messages {
            persist_orphan_messages(session_id, &msgs);
        }
    }
}

/// One-line human-readable description of an `AppEvent` for the
/// channel-closed warning. We avoid `Debug` since `AppEvent` doesn't derive
/// it and adding the derive would ripple through the rest of the file.
fn describe_event(event: &super::events::AppEvent) -> String {
    match event {
        super::events::AppEvent::SyncMessages(msgs) => {
            format!("SyncMessages(n={})", msgs.len())
        }
        super::events::AppEvent::ResponseDone => "ResponseDone".to_string(),
        super::events::AppEvent::ApiError(e) => {
            let snippet: String = e.chars().take(80).collect();
            format!("ApiError({snippet:?})")
        }
        super::events::AppEvent::StreamText(_) => "StreamText".to_string(),
        super::events::AppEvent::StreamThinking(_) => "StreamThinking".to_string(),
        super::events::AppEvent::ToolStart { name, .. } => format!("ToolStart({name})"),
        super::events::AppEvent::ToolDone { name, success, .. } => {
            format!("ToolDone({name}, ok={success})")
        }
        super::events::AppEvent::FollowUp => "FollowUp".to_string(),
        super::events::AppEvent::PermissionRequest { tool_name, .. } => {
            format!("PermissionRequest({tool_name})")
        }
        super::events::AppEvent::Key(_) => "Key".to_string(),
        super::events::AppEvent::Resize(w, h) => format!("Resize({w},{h})"),
        super::events::AppEvent::Tick => "Tick".to_string(),
        super::events::AppEvent::ShellDone { target, .. } => {
            format!("ShellDone({target:?})")
        }
    }
}

/// Best-effort persist orphaned session messages to a recovery file so the
/// next run can recover the in-flight turn instead of silently losing it.
/// Failures here are logged but not propagated — we are already on the
/// shutdown path.
fn persist_orphan_messages(session_id: &str, msgs: &[serde_json::Value]) {
    let Some(data_dir) = dirs::data_dir() else {
        tracing::warn!("no data_dir available; cannot persist orphan session state");
        return;
    };
    let dir = data_dir.join("openclaudia").join("orphan-turns");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "failed to create orphan-turn dir");
        return;
    }
    let ts = chrono::Utc::now().timestamp_millis();
    let safe_id: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe_id}-{ts}.json"));
    match serde_json::to_string_pretty(msgs) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "failed to write orphan session state"
                );
            } else {
                tracing::warn!(
                    path = %path.display(),
                    n_messages = msgs.len(),
                    "persisted orphan session state to recovery file"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to serialize orphan session state for recovery"
            );
        }
    }
}

/// Drive the agentic follow-up loop until the model stops requesting tools
/// or `MAX_ITER` iterations are exhausted.
async fn run_agentic_loop(ctx: &AgenticCtx<'_>, session_messages: &mut Vec<serde_json::Value>) {
    const MAX_ITER: u32 = 25;
    let mut iteration = 0u32;
    loop {
        iteration += 1;
        tracing::debug!(iteration, "Agentic loop iteration");
        if iteration > MAX_ITER {
            send_or_warn(
                ctx.tx,
                super::events::AppEvent::ApiError(
                    "Reached maximum tool iterations (25)".to_string(),
                ),
                ctx.session_id,
            );
            break;
        }
        let body = crate::pipeline::build_request(
            ctx.provider,
            ctx.model,
            session_messages,
            ctx.effort_level,
            ctx.claude_code_token,
            ctx.prompt_blocks,
        );
        match crate::pipeline::run_turn(crate::pipeline::RunTurnParams {
            client: ctx.client,
            endpoint: ctx.endpoint,
            headers: ctx.headers,
            request_body: &body,
            provider: ctx.provider,
            memory_db: ctx.memory_db.clone(),
            permission_mgr: ctx.permission_mgr.clone(),
            hook_engine: ctx.hook_engine.clone(),
            session_id: Some(ctx.session_id.to_string()),
            tx: ctx.tx.clone(),
        })
        .await
        {
            Ok(followup) => {
                tracing::debug!(
                    content_len = followup.content.len(),
                    tool_calls = followup.tool_calls.len(),
                    needs_followup = followup.needs_followup,
                    "Follow-up result"
                );
                if followup.needs_followup {
                    let asst = crate::pipeline::build_assistant_message_with_tools(
                        &followup.content,
                        &followup.tool_calls,
                        ctx.provider,
                    );
                    session_messages.push(asst);
                    session_messages.extend(followup.tool_results.iter().cloned());
                } else {
                    if !followup.content.is_empty() {
                        session_messages.push(
                            serde_json::json!({ "role": "assistant", "content": followup.content }),
                        );
                    }
                    break;
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Agentic follow-up failed");
                send_or_warn(ctx.tx, super::events::AppEvent::ApiError(e), ctx.session_id);
                // The caller's `SyncMessages` send after the loop will trigger
                // recovery persistence if the channel is closed — no extra
                // action needed here for partial-state capture.
                break;
            }
        }
    }
}

/// Run a complete API turn: pre-turn hooks, first `run_turn`, and an agentic
/// follow-up loop when tool calls are present.
async fn run_api_turn_async(p: ApiTurnParams) {
    let ApiTurnParams {
        mut session_messages,
        client,
        endpoint,
        headers,
        provider,
        model,
        effort_level,
        claude_code_token,
        prompt_blocks,
        memory_db,
        permission_mgr,
        hook_engine,
        session_id,
        tx,
    } = p;
    if let Some(ref engine) = hook_engine {
        if !run_preturn_hooks(engine, &mut session_messages, &tx).await {
            return;
        }
    }
    let request_body = crate::pipeline::build_request(
        &provider,
        &model,
        &session_messages,
        effort_level.as_str(),
        claude_code_token.as_deref(),
        prompt_blocks.as_ref(),
    );
    match crate::pipeline::run_turn(crate::pipeline::RunTurnParams {
        client: &client,
        endpoint: &endpoint,
        headers: &headers,
        request_body: &request_body,
        provider: &provider,
        memory_db: memory_db.clone(),
        permission_mgr: permission_mgr.clone(),
        hook_engine: hook_engine.clone(),
        session_id: Some(session_id.clone()),
        tx: tx.clone(),
    })
    .await
    {
        Ok(turn_result) => {
            handle_turn_result(
                turn_result,
                session_messages,
                TurnContext {
                    client: &client,
                    endpoint: &endpoint,
                    headers: &headers,
                    provider: &provider,
                    model: &model,
                    effort_level,
                    claude_code_token: claude_code_token.as_deref(),
                    prompt_blocks: prompt_blocks.as_ref(),
                    memory_db,
                    permission_mgr,
                    hook_engine,
                    session_id: &session_id,
                    tx: &tx,
                },
            )
            .await;
        }
        Err(e) => {
            send_or_warn(&tx, super::events::AppEvent::ApiError(e), &session_id);
        }
    }
}

/// Borrowed context bundle for [`handle_turn_result`] — purely a plumbing
/// struct to keep `run_api_turn_async` under the line-count lint while
/// preserving the per-iteration data each branch needs.
struct TurnContext<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
    headers: &'a [(String, String)],
    provider: &'a str,
    model: &'a str,
    effort_level: EffortLevel,
    claude_code_token: Option<&'a str>,
    prompt_blocks: Option<&'a crate::prompt::SystemPromptBlocks>,
    memory_db: Option<std::sync::Arc<crate::memory::MemoryDb>>,
    permission_mgr: Option<std::sync::Arc<crate::permissions::PermissionManager>>,
    hook_engine: Option<std::sync::Arc<crate::hooks::HookEngine>>,
    session_id: &'a str,
    tx: &'a std::sync::mpsc::Sender<super::events::AppEvent>,
}

/// Handle the successful `Ok(turn_result)` branch of the first `run_turn`:
/// either drive the agentic follow-up loop (when tool calls are present) or
/// push the plain assistant content. Channel-closed errors on the resulting
/// `SyncMessages` / `ResponseDone` sends go through [`send_or_warn`] so
/// partial in-flight state is persisted instead of silently dropped.
async fn handle_turn_result(
    turn_result: crate::pipeline::TurnResult,
    mut session_messages: Vec<serde_json::Value>,
    ctx: TurnContext<'_>,
) {
    tracing::debug!(
        content_len = turn_result.content.len(),
        tool_calls = turn_result.tool_calls.len(),
        needs_followup = turn_result.needs_followup,
        "Turn result"
    );
    if turn_result.needs_followup {
        let asst = crate::pipeline::build_assistant_message_with_tools(
            &turn_result.content,
            &turn_result.tool_calls,
            ctx.provider,
        );
        session_messages.push(asst);
        session_messages.extend(turn_result.tool_results.iter().cloned());
        tracing::info!(
            tool_count = turn_result.tool_calls.len(),
            result_count = turn_result.tool_results.len(),
            "Starting agentic follow-up loop"
        );
        let agentic = AgenticCtx {
            client: ctx.client,
            endpoint: ctx.endpoint,
            headers: ctx.headers,
            provider: ctx.provider,
            model: ctx.model,
            effort_level: ctx.effort_level.as_str(),
            claude_code_token: ctx.claude_code_token,
            prompt_blocks: ctx.prompt_blocks,
            memory_db: ctx.memory_db,
            permission_mgr: ctx.permission_mgr,
            hook_engine: ctx.hook_engine,
            session_id: ctx.session_id,
            tx: ctx.tx,
        };
        run_agentic_loop(&agentic, &mut session_messages).await;
        send_or_warn(
            ctx.tx,
            super::events::AppEvent::SyncMessages(session_messages),
            ctx.session_id,
        );
        send_or_warn(
            ctx.tx,
            super::events::AppEvent::ResponseDone,
            ctx.session_id,
        );
    } else if !turn_result.content.is_empty() {
        session_messages
            .push(serde_json::json!({ "role": "assistant", "content": turn_result.content }));
        send_or_warn(
            ctx.tx,
            super::events::AppEvent::SyncMessages(session_messages),
            ctx.session_id,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::expand_file_refs;
    use super::{App, AppEvent, SpawnTarget};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // =========================================================================
    // Behavior: spawn_shell — closes crosslink #371 by moving subprocess
    // execution off the sync TUI event loop and onto the tokio runtime.
    // =========================================================================

    /// Build an App wired to a tokio runtime handle and a fresh mpsc channel.
    /// Returns the receiver so the test can observe `AppEvent::ShellDone`.
    fn wire_app(app: &mut App) -> mpsc::Receiver<AppEvent> {
        app.runtime_handle = tokio::runtime::Handle::try_current().ok();
        let (tx, rx) = mpsc::channel::<AppEvent>();
        app.api_event_tx = Some(tx);
        rx
    }

    /// Block the current thread on `rx` for up to `timeout`, returning the
    /// first `ShellDone` event seen — or `None` if nothing arrives in time.
    /// Other event variants are skipped (the sync loop would handle them
    /// separately).
    fn recv_shell_done(
        rx: &mpsc::Receiver<AppEvent>,
        timeout: Duration,
    ) -> Option<(SpawnTarget, String, String, Option<i32>)> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            if let AppEvent::ShellDone {
                target,
                stdout,
                stderr,
                exit_code,
            } = rx.recv_timeout(remaining).ok()?
            {
                return Some((target, stdout, stderr, exit_code));
            }
            // Other event variants belong to the real event loop — skip
            // them and keep waiting for our ShellDone.
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_shell_returns_immediately_and_runs_in_background() {
        // The helper must not block the calling (event-loop) thread. We
        // ask it to launch `sleep 0.4` and measure that the *call itself*
        // returns in < 100ms — well below the child's lifetime — and
        // that the JoinHandle eventually completes.
        let mut app = App::new("test-model", "test-provider");
        let rx = wire_app(&mut app);

        let call_start = Instant::now();
        let join = app.spawn_shell(vec!["sleep", "0.4"], SpawnTarget::Diff);
        let call_elapsed = call_start.elapsed();

        // Pre-#371 implementation blocked for the full child lifetime.
        // 100ms is generous: spawning a tokio task is microseconds.
        assert!(
            call_elapsed < Duration::from_millis(100),
            "spawn_shell blocked the caller for {call_elapsed:?} — should return immediately"
        );

        // The handle must actually resolve once the child exits.
        join.await.expect("spawn_shell task panicked");

        // And the receiver must have observed the ShellDone event.
        let done = recv_shell_done(&rx, Duration::from_millis(500))
            .expect("expected ShellDone event after join");
        assert!(matches!(done.0, SpawnTarget::Diff));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_shell_success_delivers_stdout() {
        // `echo hello-371` writes "hello-371\n" to stdout and exits 0.
        // ShellDone must carry that stdout and an exit_code of Some(0).
        let mut app = App::new("test-model", "test-provider");
        let rx = wire_app(&mut app);

        let join = app.spawn_shell(vec!["echo", "hello-371"], SpawnTarget::Diff);
        join.await.expect("spawn_shell task panicked");

        let (target, stdout, _stderr, exit_code) = recv_shell_done(&rx, Duration::from_millis(500))
            .expect("expected ShellDone event from successful echo");
        assert!(matches!(target, SpawnTarget::Diff));
        assert_eq!(exit_code, Some(0), "echo should exit 0");
        assert!(
            stdout.contains("hello-371"),
            "expected stdout to contain 'hello-371', got {stdout:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_shell_failure_delivers_nonzero_exit() {
        // `bash -c 'exit 7'` exits with code 7. ShellDone must surface
        // exit_code = Some(7) so the renderer picks the ToolErr branch.
        let mut app = App::new("test-model", "test-provider");
        let rx = wire_app(&mut app);

        let join = app.spawn_shell(
            vec!["bash", "-c", "exit 7"],
            SpawnTarget::ShellCommand {
                displayed: "exit 7".to_string(),
            },
        );
        join.await.expect("spawn_shell task panicked");

        let (target, _stdout, _stderr, exit_code) =
            recv_shell_done(&rx, Duration::from_millis(500))
                .expect("expected ShellDone event from failing bash");
        assert!(matches!(target, SpawnTarget::ShellCommand { .. }));
        assert_eq!(
            exit_code,
            Some(7),
            "bash -c 'exit 7' should report exit_code = Some(7)"
        );
    }

    // =========================================================================
    // Behavior: expand_file_refs — panic-free regex handling (#292)
    // =========================================================================

    #[test]
    fn expand_file_refs_no_at_sign_returns_input_unchanged() {
        // Fast path: no '@' in input — function returns immediately without
        // touching the regex.  Output must equal the input exactly.
        let input = "hello world, no references here";
        assert_eq!(expand_file_refs(input), input);
    }

    #[test]
    fn expand_file_refs_double_at_does_not_panic() {
        // Regression guard for the old `.unwrap()` on cap.get(0): a bare '@@'
        // or '@ @' must not panic regardless of whether the regex matches.
        let _ = expand_file_refs("@@");
        let _ = expand_file_refs("@ @");
        let _ = expand_file_refs("email@example.com and @another");
    }

    #[test]
    fn expand_file_refs_unclosed_quote_does_not_panic() {
        // A `@"` with no closing quote must not panic — the regex simply won't
        // match group 1, and the `if let Some` guard skips it cleanly.
        let _ = expand_file_refs(r#"@"unclosed"#);
        let _ = expand_file_refs(r#"some text @"no end here and more text"#);
    }

    #[test]
    fn expand_file_refs_many_at_signs_does_not_panic() {
        // Stress: 1 000 '@' characters in a row must not panic or overflow.
        let input = "@".repeat(1_000);
        let _ = expand_file_refs(&input);
    }

    // =========================================================================
    // Behavior: persist_transcript_tail watermark — crosslink #709
    // =========================================================================

    /// Drop guard restoring `CLAUDE_CONFIG_HOME_DIR` to its previous
    /// value (or unsetting it) when the scope exits, even on panic.
    /// Holds the crate-wide [`crate::transcript::env_lock`] for the
    /// guard's lifetime so concurrent tests in other modules that
    /// mutate the same env var cannot observe a half-mutated state.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
        // Field exists to hold the lock for the EnvGuard's lifetime.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &std::path::Path) -> Self {
            let lock = crate::transcript::env_lock();
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self {
                key,
                prev,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn persist_transcript_tail_advances_watermark_to_len_on_success() {
        // Happy path: every queued message persists successfully, so the
        // watermark moves all the way to session_messages.len(). The
        // transcript writes land under `CLAUDE_CONFIG_HOME_DIR/projects/...`
        // which we redirect into a tempdir so the test can't pollute
        // the user's real `~/.claude/projects/` tree.
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("CLAUDE_CONFIG_HOME_DIR", tmp.path());

        let mut app = App::new("test-model", "test-provider");
        app.transcript_cwd = tmp.path().to_path_buf();
        app.session_messages = vec![
            serde_json::json!({"role": "user", "content": "one"}),
            serde_json::json!({"role": "assistant", "content": "two"}),
            serde_json::json!({"role": "user", "content": "three"}),
        ];
        app.transcript_watermark = 0;

        app.persist_transcript_tail();

        assert_eq!(
            app.transcript_watermark,
            3,
            "watermark advances to len when every append succeeds"
        );
    }

    #[test]
    fn persist_transcript_tail_only_advances_for_persisted_entries_on_failure() {
        // crosslink #709 regression: when `append_entry` fails, the
        // watermark must NOT jump to session_messages.len() (which would
        // permanently drop the un-persisted tail). Instead it must
        // advance only by the count actually written.
        //
        // Failure is injected by placing a regular FILE at the path
        // `create_dir_all` would otherwise create as a directory
        // (`<CLAUDE_CONFIG_HOME_DIR>/projects/`). `create_dir_all`
        // then errors with "Not a directory" on every append, so zero
        // entries persist and the watermark must stay at 0 (the bug
        // jumped it straight to 3).
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("projects"), b"not a directory")
            .expect("write blocker file");
        let _guard = EnvGuard::set("CLAUDE_CONFIG_HOME_DIR", tmp.path());

        let mut app = App::new("test-model", "test-provider");
        app.transcript_cwd = tmp.path().to_path_buf();
        app.session_messages = vec![
            serde_json::json!({"role": "user", "content": "one"}),
            serde_json::json!({"role": "assistant", "content": "two"}),
            serde_json::json!({"role": "user", "content": "three"}),
        ];
        app.transcript_watermark = 0;

        app.persist_transcript_tail();

        assert_eq!(
            app.transcript_watermark, 0,
            "watermark must NOT advance past entries that failed to persist (was: {})",
            app.transcript_watermark
        );
    }
}
