pub mod command_registry;
pub mod input;
pub mod keybindings;
pub mod models;
pub mod permissions;
pub mod plan_mode;
pub mod review;
pub mod session_io;
pub mod slash;
pub mod vim;

use openclaudia::tools::safe_truncate;
use std::fs;
use std::path::PathBuf;

/// Get the data directory for `OpenClaudia`
pub fn get_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("openclaudia")
}

/// Get the history file path for rustyline
pub fn get_history_path() -> PathBuf {
    get_data_dir().join("history.txt")
}

/// Get the chat sessions directory
pub fn get_sessions_dir() -> PathBuf {
    get_data_dir().join("chat_sessions")
}

/// Agent operating mode.
///
/// `Build` is the default full-access mode. `Plan` is the read-only review
/// mode entered via `enter_plan_mode`. `Extend` and `Refactor` mirror the
/// `modes::Preset` scope axis so the REPL can record (and restore) a
/// non-Plan working mode across plan-mode entry — see crosslink #618 for
/// the `previous_mode` snapshot that `ExitPlanMode` consults instead of
/// unconditionally falling back to `Build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Full access mode - can make changes
    #[default]
    Build,
    /// Read-only mode - only suggestions
    Plan,
    /// Extending an existing implementation (write-allowed, scope-locked).
    Extend,
    /// Refactor mode — write-allowed, structural changes expected.
    Refactor,
}

impl AgentMode {
    /// Toggle Build <-> Plan. Non-Build/Plan modes flip into Plan so the
    /// keybinding still has a sensible effect; the previous mode is
    /// remembered by [`PlanModeState::previous_mode`] (crosslink #618).
    pub const fn toggle(self) -> Self {
        match self {
            Self::Build | Self::Extend | Self::Refactor => Self::Plan,
            Self::Plan => Self::Build,
        }
    }

    pub const fn display(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Plan => "Plan",
            Self::Extend => "Extend",
            Self::Refactor => "Refactor",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Build => "Full access - can make changes",
            Self::Plan => "Read-only - suggestions only",
            Self::Extend => "Write-allowed; scope locked to an existing surface",
            Self::Refactor => "Write-allowed; structural changes expected",
        }
    }

    /// Stable lowercase token used when persisting the mode into
    /// [`PlanModeState::previous_mode`] (which is a `String` so the
    /// session module doesn't depend on the binary-side enum).
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Plan => "plan",
            Self::Extend => "extend",
            Self::Refactor => "refactor",
        }
    }

    /// Parse the token produced by [`Self::as_token`]. Unknown tokens fall
    /// back to `Build` so a future variant added on a newer binary doesn't
    /// crash an older one reading the saved session — the worst case is a
    /// degraded restore to Build, which matches the pre-#618 behaviour.
    pub fn from_token(s: &str) -> Self {
        match s {
            "plan" => Self::Plan,
            "extend" => Self::Extend,
            "refactor" => Self::Refactor,
            _ => Self::Build,
        }
    }
}

/// A saved chat session with messages
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatSession {
    /// Session ID
    pub id: String,
    /// Session title (first user message or default)
    pub title: String,
    /// When the session was created
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the session was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// The model used
    pub model: String,
    /// The provider used
    pub provider: String,
    /// Agent mode (Build or Plan)
    #[serde(default)]
    pub mode: AgentMode,
    /// Behavioral mode (agency/quality/scope axes + modifiers)
    #[serde(default)]
    pub behavior_mode: openclaudia::modes::BehaviorMode,
    /// Conversation messages
    pub messages: Vec<serde_json::Value>,
    /// Undo stack for undone message pairs (user + assistant)
    #[serde(default)]
    pub undo_stack: Vec<(serde_json::Value, serde_json::Value)>,
    /// Plan mode state (None when not in plan mode)
    #[serde(default)]
    pub plan_mode: Option<openclaudia::session::PlanModeState>,
    /// Approved plan content injected as system context
    #[serde(default)]
    pub approved_plan: Option<String>,
    /// Additional working directories added via `/add-dir`
    #[serde(default)]
    pub working_dirs: Vec<std::path::PathBuf>,
}

impl ChatSession {
    pub fn new(
        model: &str,
        provider: &str,
        behavior_mode: openclaudia::modes::BehaviorMode,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New conversation".to_string(),
            created_at: now,
            updated_at: now,
            model: model.to_string(),
            provider: provider.to_string(),
            mode: AgentMode::default(),
            behavior_mode,
            messages: Vec::new(),
            undo_stack: Vec::new(),
            plan_mode: None,
            approved_plan: None,
            working_dirs: Vec::new(),
        }
    }

    /// Undo the last user+assistant message pair
    pub fn undo(&mut self) -> bool {
        if self.messages.len() >= 2 {
            if let (Some(assistant), Some(user)) = (self.messages.pop(), self.messages.pop()) {
                self.undo_stack.push((user, assistant));
                self.touch();
                return true;
            }
        }
        false
    }

    /// Redo the last undone message pair
    pub fn redo(&mut self) -> bool {
        if let Some((user, assistant)) = self.undo_stack.pop() {
            self.messages.push(user);
            self.messages.push(assistant);
            self.touch();
            true
        } else {
            false
        }
    }

    /// Clear undo stack (call when new messages are added)
    pub fn clear_undo_stack(&mut self) {
        self.undo_stack.clear();
    }

    /// Add a working directory to the session scope (deduplicates by canonical path).
    ///
    /// Returns `true` if the directory was added, `false` if it was already present.
    pub fn add_working_dir(&mut self, path: std::path::PathBuf) -> bool {
        if self.working_dirs.contains(&path) {
            return false;
        }
        self.working_dirs.push(path);
        self.touch();
        true
    }

    pub fn update_title(&mut self) {
        if let Some(first_user) = self
            .messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        {
            if let Some(content) = first_user.get("content").and_then(|c| c.as_str()) {
                let title = if content.len() > 50 {
                    format!("{}...", safe_truncate(content, 47))
                } else {
                    content.to_string()
                };
                self.title = title;
            }
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }
}

/// Save a chat session to disk
pub fn save_chat_session(session: &ChatSession) -> anyhow::Result<()> {
    let dir = get_sessions_dir();
    fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.json", session.id));
    let json = serde_json::to_string_pretty(session)?;
    fs::write(path, json)?;
    Ok(())
}

/// Load a chat session by ID
pub fn load_chat_session(id: &str) -> Option<ChatSession> {
    let path = get_sessions_dir().join(format!("{id}.json"));
    if path.exists() {
        let json = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    } else {
        None
    }
}

/// List all chat sessions, sorted by most recent
pub fn list_chat_sessions() -> Vec<ChatSession> {
    let dir = get_sessions_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<ChatSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }
    }

    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    sessions
}
