//! Session Manager - Tracks agent sessions with initializer/coding agent patterns.
//!
//! Implements two-part session architecture:
//! - Initializer Agent: First session, creates progress files and feature lists
//! - Coding Agent: Subsequent sessions, reads git logs and progress files
//!
//! Treats agents like shift workers with documented handoffs.

mod audit;
mod pricing;
mod state;
mod task;

// Re-export all public types
pub use audit::AuditLogger;
pub use pricing::{calculate_cost, get_pricing, ModelPricing};
pub use state::{
    get_session_context, is_tool_allowed_in_plan_mode, is_tool_allowed_in_plan_mode_with_policy,
    AllowedPrompt, PlanModePolicy, PlanModeState, TokenUsage, TurnMetrics, MCP_TOOL_PREFIX,
    PLAN_MODE_ALLOWED_TOOLS, PLUGIN_TOOL_PREFIX,
};
pub use task::{Task, TaskManager, TaskStatus, TaskUpdateParams, TaskUpdateStatus};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Maximum number of [`TurnMetrics`] entries retained in [`Session::turn_metrics`].
///
/// Once this cap is reached, `record_turn_estimate` evicts the oldest entry
/// (index 0) before pushing the new one, so the vector never grows beyond
/// `MAX_TURN_METRICS` elements.  The [`Session::cumulative_usage`] counter
/// continues to accumulate across all turns regardless of eviction.
///
/// Chosen to cover ~2.8 hours of turns at 10 s/turn while keeping the
/// serialised session JSON under ~500 KB for the default [`TurnMetrics`] size.
pub const MAX_TURN_METRICS: usize = 1_000;

/// Write data to a file atomically: write to a temp file, then rename.
/// If the process crashes during the write, the original file is untouched.
fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Session state indicating the agent mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// First session - creates initial context
    Initializer,
    /// Subsequent sessions - continues from handoff
    Coding,
}

/// Progress tracking for a session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionProgress {
    /// Tasks completed in this session
    pub completed_tasks: Vec<String>,
    /// Tasks in progress
    pub in_progress_tasks: Vec<String>,
    /// Tasks pending
    pub pending_tasks: Vec<String>,
    /// Key decisions made
    pub decisions: Vec<String>,
    /// Files modified
    pub files_modified: Vec<String>,
    /// Notes for next session
    pub handoff_notes: String,
    /// VDD: total findings across all VDD sessions
    #[serde(default)]
    pub vdd_total_findings: u32,
    /// VDD: total genuine findings
    #[serde(default)]
    pub vdd_total_genuine: u32,
    /// VDD: session IDs for VDD sessions in this coding session
    #[serde(default)]
    pub vdd_sessions: Vec<String>,
}

/// A single agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier
    pub id: String,
    /// Session mode (initializer or coding)
    pub mode: SessionMode,
    /// When the session started
    pub created_at: DateTime<Utc>,
    /// When the session was last updated
    pub updated_at: DateTime<Utc>,
    /// Session progress tracking
    pub progress: SessionProgress,
    /// Parent session ID if this is a continuation
    pub parent_session_id: Option<String>,
    /// Number of API requests in this session
    pub request_count: u64,
    /// Total tokens used (approximate) - kept for backward compat
    pub total_tokens: u64,
    /// Cumulative token usage across all turns
    #[serde(default)]
    pub cumulative_usage: TokenUsage,
    /// Per-turn metrics history (capped at [`MAX_TURN_METRICS`] entries)
    #[serde(default)]
    pub turn_metrics: Vec<TurnMetrics>,
    /// Monotonically increasing count of all turns ever recorded, including
    /// those evicted from the `turn_metrics` ring.  Never decremented.
    #[serde(default)]
    pub total_turns: u64,
}

impl Session {
    /// Create a new initializer session
    #[must_use]
    pub fn new_initializer() -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            mode: SessionMode::Initializer,
            created_at: now,
            updated_at: now,
            progress: SessionProgress::default(),
            parent_session_id: None,
            request_count: 0,
            total_tokens: 0,
            cumulative_usage: TokenUsage::default(),
            turn_metrics: Vec::new(),
            total_turns: 0,
        }
    }

    /// Create a new coding session continuing from a parent
    #[must_use]
    pub fn new_coding(parent_id: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            mode: SessionMode::Coding,
            created_at: now,
            updated_at: now,
            progress: SessionProgress::default(),
            parent_session_id: Some(parent_id.to_string()),
            request_count: 0,
            total_tokens: 0,
            cumulative_usage: TokenUsage::default(),
            turn_metrics: Vec::new(),
            total_turns: 0,
        }
    }

    /// Update the session timestamp
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Increment request count
    pub fn increment_requests(&mut self) {
        self.request_count += 1;
        self.touch();
    }

    /// Add tokens to the total (legacy simple counter)
    pub fn add_tokens(&mut self, tokens: u64) {
        self.total_tokens += tokens;
        self.touch();
    }

    /// Record metrics for an API turn (pre-request estimation).
    ///
    /// The in-memory ring is capped at [`MAX_TURN_METRICS`] entries: when the
    /// vector is already at capacity the oldest entry is evicted before the new
    /// one is pushed, so memory usage stays bounded regardless of session
    /// length.  [`Session::cumulative_usage`] is **not** affected by eviction —
    /// it continues to accumulate across all turns.
    pub fn record_turn_estimate(
        &mut self,
        estimated_input_tokens: usize,
        injected_context_tokens: usize,
        system_prompt_tokens: usize,
        tool_def_tokens: usize,
    ) -> u64 {
        // Evict the oldest entry when at capacity so the vec stays bounded.
        if self.turn_metrics.len() >= MAX_TURN_METRICS {
            self.turn_metrics.remove(0);
        }
        // Use the cumulative turn counter so turn_number is monotonically
        // increasing even after old entries are evicted from the ring.
        self.total_turns += 1;
        let turn_number = self.total_turns;
        self.turn_metrics.push(TurnMetrics {
            turn_number,
            estimated_input_tokens,
            actual_usage: None,
            injected_context_tokens,
            system_prompt_tokens,
            tool_def_tokens,
            timestamp: Utc::now(),
            vdd_iterations: None,
            vdd_genuine_findings: None,
            vdd_false_positives: None,
            vdd_adversary_tokens: None,
            vdd_converged: None,
        });
        self.touch();
        turn_number
    }

    /// Record actual usage from provider response for the most recent turn
    pub fn record_actual_usage(&mut self, usage: TokenUsage) {
        self.total_tokens += usage.total();
        self.cumulative_usage.accumulate(&usage);
        if let Some(last_turn) = self.turn_metrics.last_mut() {
            last_turn.actual_usage = Some(usage);
        }
        self.touch();
    }

    /// Get session stats summary
    #[must_use]
    pub fn stats_summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "Session: {}", self.id);
        let _ = writeln!(s, "Mode: {:?}", self.mode);
        let _ = writeln!(s, "Turns: {}", self.total_turns);
        let _ = writeln!(s, "Requests: {}", self.request_count);
        let _ = writeln!(
            s,
            "Input tokens:  {} (cumulative)",
            self.cumulative_usage.input_tokens
        );
        let _ = writeln!(
            s,
            "Output tokens: {} (cumulative)",
            self.cumulative_usage.output_tokens
        );
        let _ = writeln!(
            s,
            "Cache read:    {}",
            self.cumulative_usage.cache_read_tokens
        );
        let _ = writeln!(
            s,
            "Cache write:   {}",
            self.cumulative_usage.cache_write_tokens
        );
        let _ = writeln!(s, "Total tokens:  {}", self.cumulative_usage.total());

        if let Some(last) = self.turn_metrics.last() {
            let _ = write!(
                s,
                "\nLast turn #{}: estimated {} input tokens",
                last.turn_number, last.estimated_input_tokens
            );
            if let Some(actual) = &last.actual_usage {
                let _ = write!(
                    s,
                    ", actual {}in/{}out",
                    actual.input_tokens, actual.output_tokens
                );
            }
            s.push('\n');
        }

        s
    }

    /// Add a completed task
    pub fn complete_task(&mut self, task: impl Into<String>) {
        self.progress.completed_tasks.push(task.into());
        self.touch();
    }

    /// Add a file to the modified list
    pub fn add_modified_file(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !self.progress.files_modified.contains(&path) {
            self.progress.files_modified.push(path);
            self.touch();
        }
    }

    /// Set handoff notes for the next session
    pub fn set_handoff_notes(&mut self, notes: impl Into<String>) {
        self.progress.handoff_notes = notes.into();
        self.touch();
    }

    /// Generate a handoff summary for the next agent
    #[must_use]
    pub fn generate_handoff(&self) -> String {
        let mut handoff = String::new();

        handoff.push_str("## Session Handoff\n\n");
        let _ = writeln!(handoff, "Previous Session: {}", self.id);
        let _ = writeln!(handoff, "Mode: {:?}", self.mode);
        let _ = write!(
            handoff,
            "Duration: {} to {}\n\n",
            self.created_at.format("%Y-%m-%d %H:%M UTC"),
            self.updated_at.format("%Y-%m-%d %H:%M UTC")
        );

        if !self.progress.completed_tasks.is_empty() {
            handoff.push_str("### Completed Tasks\n");
            for task in &self.progress.completed_tasks {
                let _ = writeln!(handoff, "- [x] {task}");
            }
            handoff.push('\n');
        }

        if !self.progress.in_progress_tasks.is_empty() {
            handoff.push_str("### In Progress\n");
            for task in &self.progress.in_progress_tasks {
                let _ = writeln!(handoff, "- [ ] {task}");
            }
            handoff.push('\n');
        }

        if !self.progress.pending_tasks.is_empty() {
            handoff.push_str("### Pending Tasks\n");
            for task in &self.progress.pending_tasks {
                let _ = writeln!(handoff, "- [ ] {task}");
            }
            handoff.push('\n');
        }

        if !self.progress.decisions.is_empty() {
            handoff.push_str("### Key Decisions\n");
            for decision in &self.progress.decisions {
                let _ = writeln!(handoff, "- {decision}");
            }
            handoff.push('\n');
        }

        if !self.progress.files_modified.is_empty() {
            handoff.push_str("### Files Modified\n");
            for file in &self.progress.files_modified {
                let _ = writeln!(handoff, "- {file}");
            }
            handoff.push('\n');
        }

        if !self.progress.handoff_notes.is_empty() {
            handoff.push_str("### Notes for Next Session\n");
            handoff.push_str(&self.progress.handoff_notes);
            handoff.push('\n');
        }

        // Include token usage stats
        if self.cumulative_usage.total() > 0 {
            handoff.push_str("\n### Token Usage\n");
            let _ = writeln!(
                handoff,
                "- Input: {} tokens",
                self.cumulative_usage.input_tokens
            );
            let _ = writeln!(
                handoff,
                "- Output: {} tokens",
                self.cumulative_usage.output_tokens
            );
            let _ = writeln!(
                handoff,
                "- Cache read: {} tokens",
                self.cumulative_usage.cache_read_tokens
            );
            let _ = writeln!(handoff, "- Turns: {}", self.total_turns);
        }

        handoff
    }
}

/// Manages session lifecycle and persistence
#[derive(Debug, Clone)]
pub struct SessionManager {
    /// Directory for session persistence
    persist_dir: PathBuf,
    /// Current active session
    current_session: Option<Session>,
    /// VDD advisory context to inject into the next turn
    vdd_pending_context: Option<String>,
    /// Structured task manager for `task_create`/`update`/`get`/`list` tools
    pub task_manager: TaskManager,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(persist_dir: impl Into<PathBuf>) -> Self {
        let persist_dir = persist_dir.into();

        // Ensure the directory exists
        if let Err(e) = fs::create_dir_all(&persist_dir) {
            warn!(error = %e, path = ?persist_dir, "Failed to create session directory");
        }

        Self {
            persist_dir,
            current_session: None,
            vdd_pending_context: None,
            task_manager: TaskManager::new(),
        }
    }

    /// Get the current session, creating one if none exists
    ///
    /// # Panics
    ///
    /// Panics if session creation succeeds but the internal option is still `None`
    /// (should be unreachable).
    pub fn get_or_create_session(&mut self) -> &Session {
        if self.current_session.is_none() {
            self.current_session = Some(self.create_session());
        }
        self.current_session
            .as_ref()
            .expect("session must exist after get_or_create")
    }

    /// Get the current session mutably
    pub const fn get_session_mut(&mut self) -> Option<&mut Session> {
        self.current_session.as_mut()
    }

    /// Get the current session immutably
    #[must_use]
    pub const fn get_session(&self) -> Option<&Session> {
        self.current_session.as_ref()
    }

    /// Store VDD advisory context to inject into the next turn
    pub fn store_vdd_context(&mut self, context: String) {
        self.vdd_pending_context = Some(context);
    }

    /// Take (consume) the pending VDD context for injection
    pub const fn take_vdd_context(&mut self) -> Option<String> {
        self.vdd_pending_context.take()
    }

    /// Create a new session (initializer or coding based on history)
    fn create_session(&self) -> Session {
        // Check if there's a previous session to continue from
        if let Some(last_session) = self.load_latest_session() {
            info!(
                parent_id = %last_session.id,
                "Creating coding session continuing from previous"
            );
            Session::new_coding(&last_session.id)
        } else {
            info!("Creating new initializer session");
            Session::new_initializer()
        }
    }

    /// Start a fresh initializer session
    ///
    /// # Panics
    ///
    /// Panics if session assignment succeeds but the internal option is still `None`
    /// (should be unreachable).
    pub fn start_initializer(&mut self) -> &Session {
        let session = Session::new_initializer();
        info!(session_id = %session.id, "Started initializer session");
        self.current_session = Some(session);
        self.current_session
            .as_ref()
            .expect("session must exist after assignment")
    }

    /// Start a coding session from a parent
    ///
    /// # Panics
    ///
    /// Panics if session assignment succeeds but the internal option is still `None`
    /// (should be unreachable).
    pub fn start_coding(&mut self, parent_id: &str) -> &Session {
        let session = Session::new_coding(parent_id);
        info!(
            session_id = %session.id,
            parent_id = %parent_id,
            "Started coding session"
        );
        self.current_session = Some(session);
        self.current_session
            .as_ref()
            .expect("session must exist after assignment")
    }

    /// End the current session and persist it
    pub fn end_session(&mut self, handoff_notes: Option<&str>) -> Option<Session> {
        if let Some(mut session) = self.current_session.take() {
            if let Some(notes) = handoff_notes {
                session.set_handoff_notes(notes);
            }

            // Persist the session
            if let Err(e) = self.persist_session(&session) {
                warn!(error = %e, "Failed to persist session");
            }

            info!(
                session_id = %session.id,
                requests = session.request_count,
                "Ended session"
            );

            Some(session)
        } else {
            None
        }
    }

    /// Persist a session to disk using atomic write-to-temp-then-rename.
    ///
    /// Each file is written to a `.tmp` file first, then atomically renamed.
    /// If the process crashes mid-write, the original file remains intact.
    fn persist_session(&self, session: &Session) -> anyhow::Result<()> {
        let filename = format!("{}.json", session.id);
        let path = self.persist_dir.join(&filename);

        let json = serde_json::to_string_pretty(session)?;
        atomic_write(&path, json.as_bytes())?;

        debug!(path = ?path, "Persisted session");

        // Also update the "latest" symlink/file
        let latest_path = self.persist_dir.join("latest.json");
        atomic_write(
            &latest_path,
            serde_json::to_string_pretty(session)?.as_bytes(),
        )?;

        // Generate and save handoff document
        let handoff_path = self.persist_dir.join("handoff.md");
        atomic_write(&handoff_path, session.generate_handoff().as_bytes())?;

        Ok(())
    }

    /// Load a session by ID
    #[must_use]
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        let path = self.persist_dir.join(format!("{session_id}.json"));
        self.load_session_from_path(&path)
    }

    /// Load the most recent session
    #[must_use]
    pub fn load_latest_session(&self) -> Option<Session> {
        let path = self.persist_dir.join("latest.json");
        self.load_session_from_path(&path)
    }

    /// Load a session from a file path
    #[allow(clippy::unused_self)]
    fn load_session_from_path(&self, path: &Path) -> Option<Session> {
        if !path.exists() {
            return None;
        }

        match fs::read_to_string(path) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(session) => Some(session),
                Err(e) => {
                    warn!(error = %e, path = ?path, "Failed to parse session file");
                    None
                }
            },
            Err(e) => {
                warn!(error = %e, path = ?path, "Failed to read session file");
                None
            }
        }
    }

    /// Get the handoff context from the last session
    #[must_use]
    pub fn get_handoff_context(&self) -> Option<String> {
        let handoff_path = self.persist_dir.join("handoff.md");
        fs::read_to_string(&handoff_path).ok()
    }

    /// List all persisted sessions
    #[must_use]
    pub fn list_sessions(&self) -> Vec<Session> {
        let mut sessions = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.persist_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    // Skip latest.json as it's a copy
                    if path.file_stem().is_some_and(|s| s == "latest") {
                        continue;
                    }
                    if let Some(session) = self.load_session_from_path(&path) {
                        sessions.push(session);
                    }
                }
            }
        }

        // Sort by created_at descending
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        sessions
    }

    /// Clean up old sessions, keeping only the most recent N
    pub fn cleanup_old_sessions(&self, keep_count: usize) {
        let sessions = self.list_sessions();

        if sessions.len() <= keep_count {
            return;
        }

        for session in sessions.iter().skip(keep_count) {
            let path = self.persist_dir.join(format!("{}.json", session.id));
            if let Err(e) = fs::remove_file(&path) {
                warn!(error = %e, path = ?path, "Failed to remove old session");
            } else {
                debug!(session_id = %session.id, "Removed old session");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_initializer_session() {
        let session = Session::new_initializer();
        assert_eq!(session.mode, SessionMode::Initializer);
        assert!(session.parent_session_id.is_none());
        assert_eq!(session.request_count, 0);
    }

    #[test]
    fn test_new_coding_session() {
        let session = Session::new_coding("parent-123");
        assert_eq!(session.mode, SessionMode::Coding);
        assert_eq!(session.parent_session_id, Some("parent-123".to_string()));
    }

    #[test]
    fn test_session_progress() {
        let mut session = Session::new_initializer();
        session.complete_task("Task 1");
        session.add_modified_file("src/main.rs");
        session.set_handoff_notes("Continue with task 2");

        assert_eq!(session.progress.completed_tasks.len(), 1);
        assert_eq!(session.progress.files_modified.len(), 1);
        assert!(!session.progress.handoff_notes.is_empty());
    }

    #[test]
    fn test_generate_handoff() {
        let mut session = Session::new_initializer();
        session.complete_task("Implemented feature X");
        session
            .progress
            .pending_tasks
            .push("Test feature X".to_string());
        session.set_handoff_notes("Feature X works but needs tests");

        let handoff = session.generate_handoff();
        assert!(handoff.contains("Implemented feature X"));
        assert!(handoff.contains("Test feature X"));
        assert!(handoff.contains("needs tests"));
    }

    #[test]
    fn test_session_manager_persistence() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        // Create and end a session
        let session = manager.get_or_create_session().clone();
        assert_eq!(session.mode, SessionMode::Initializer);

        manager.end_session(Some("Test handoff notes"));

        // Load it back
        let loaded = manager.load_session(&session.id);
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, session.id);
    }

    #[test]
    fn test_session_manager_coding_continuation() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        // First session
        let first = manager.get_or_create_session().clone();
        manager.end_session(None);

        // Second session should be coding mode
        let second = manager.get_or_create_session().clone();
        assert_eq!(second.mode, SessionMode::Coding);
        assert_eq!(second.parent_session_id, Some(first.id));
    }

    #[test]
    fn test_session_manager_has_task_manager() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));
        // Verify we can access and use the task manager
        manager
            .task_manager
            .create_task("Test".to_string(), "Test task".to_string(), None);
        assert_eq!(manager.task_manager.list_tasks().len(), 1);
    }

    // ── Phase 2 spec-pinning tests (#552 / spec #537 B-session) ──────────────

    /// Spec — `TokenUsage::total()` is input + output (not cache tokens).
    #[test]
    fn token_usage_total_excludes_cache() {
        let u = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 200,
            cache_write_tokens: 300,
        };
        assert_eq!(u.total(), 150, "total() must be input + output only");
    }

    /// Spec — `TokenUsage::accumulate` sums all four fields independently.
    #[test]
    fn token_usage_accumulate_sums_all_fields() {
        let mut acc = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 20,
            cache_write_tokens: 30,
        };
        let delta = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 200,
            cache_write_tokens: 300,
        };
        acc.accumulate(&delta);
        assert_eq!(acc.input_tokens, 110);
        assert_eq!(acc.output_tokens, 55);
        assert_eq!(acc.cache_read_tokens, 220);
        assert_eq!(acc.cache_write_tokens, 330);
    }

    /// Spec — `record_actual_usage` feeds into `cumulative_usage` and `total_tokens`.
    #[test]
    fn session_record_actual_usage_accumulates() {
        let mut session = Session::new_initializer();

        // Simulate a turn estimate first (required before actual usage)
        session.record_turn_estimate(1000, 100, 80, 20);

        let usage = TokenUsage {
            input_tokens: 500,
            output_tokens: 250,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        session.record_actual_usage(usage);

        assert_eq!(session.cumulative_usage.input_tokens, 500);
        assert_eq!(session.cumulative_usage.output_tokens, 250);
        // total_tokens legacy field also updated
        assert_eq!(
            session.total_tokens, 750,
            "legacy total_tokens must equal input+output"
        );

        // Second turn
        session.record_turn_estimate(800, 90, 70, 20);
        session.record_actual_usage(TokenUsage {
            input_tokens: 300,
            output_tokens: 100,
            cache_read_tokens: 50,
            cache_write_tokens: 0,
        });
        assert_eq!(session.cumulative_usage.input_tokens, 800);
        assert_eq!(session.cumulative_usage.output_tokens, 350);
        assert_eq!(session.cumulative_usage.cache_read_tokens, 50);
        assert_eq!(session.total_tokens, 1150);
    }

    /// Spec — `record_turn_estimate` assigns monotonically increasing turn numbers.
    #[test]
    fn session_turn_numbers_monotonically_increasing() {
        let mut session = Session::new_initializer();
        let t1 = session.record_turn_estimate(100, 10, 8, 2);
        let t2 = session.record_turn_estimate(200, 20, 16, 4);
        let t3 = session.record_turn_estimate(300, 30, 24, 6);
        assert_eq!(t1, 1);
        assert_eq!(t2, 2);
        assert_eq!(t3, 3);
        assert_eq!(session.turn_metrics.len(), 3);
    }

    /// Spec — `get_session_context` returns initializer context for first session.
    #[test]
    fn session_context_initializer_mode() {
        let session = Session::new_initializer();
        let ctx = get_session_context(&session);
        assert!(
            ctx.contains("Initializer"),
            "initializer mode context must identify the agent role"
        );
    }

    /// Spec — `get_session_context` returns coding context including parent ID.
    #[test]
    fn session_context_coding_mode_includes_parent() {
        let session = Session::new_coding("parent-abc");
        let ctx = get_session_context(&session);
        assert!(
            ctx.contains("Coding") || ctx.contains("continuing"),
            "coding mode context must identify continuation"
        );
        assert!(
            ctx.contains("parent-abc"),
            "coding mode context must include parent session ID"
        );
    }

    /// Spec — `add_modified_file` is idempotent (no duplicates).
    #[test]
    fn add_modified_file_is_idempotent() {
        let mut session = Session::new_initializer();
        session.add_modified_file("src/main.rs");
        session.add_modified_file("src/main.rs");
        session.add_modified_file("src/main.rs");
        assert_eq!(
            session.progress.files_modified.len(),
            1,
            "duplicate file paths must not be added"
        );
    }

    /// Spec — `SessionManager::start_initializer` always creates an initializer
    /// session regardless of any persisted history.
    #[test]
    fn start_initializer_overrides_history() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        // Create and persist a session so there IS history
        manager.get_or_create_session();
        manager.end_session(None);

        // Force an initializer even though history exists
        let session = manager.start_initializer().clone();
        assert_eq!(
            session.mode,
            SessionMode::Initializer,
            "start_initializer must override history detection"
        );
        assert!(
            session.parent_session_id.is_none(),
            "forced initializer must have no parent"
        );
    }

    /// Spec — VDD context round-trips through store/take.
    #[test]
    fn vdd_context_store_and_take() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        assert!(
            manager.take_vdd_context().is_none(),
            "initially no VDD context"
        );

        manager.store_vdd_context("advisory: review tool calls".to_string());
        let ctx = manager.take_vdd_context();
        assert_eq!(ctx.as_deref(), Some("advisory: review tool calls"));

        // take is destructive — second take returns None
        assert!(
            manager.take_vdd_context().is_none(),
            "take_vdd_context must be destructive"
        );
    }

    // -----------------------------------------------------------------------
    // #285 — Session.turn_metrics grows unbounded — memory leak in long sessions
    //
    // Forensic evidence: every API turn called `session.record_turn_estimate()`
    // which pushed a full `TurnMetrics` struct (11 fields, including
    // `Option<TokenUsage>`) onto `Session.turn_metrics: Vec<TurnMetrics>` with
    // no truncation.  For a 24-hour agent loop at 10 s/turn that is 8 640
    // entries.  `Session` derives `Clone` and is cloned at multiple call sites,
    // each clone duplicating the full history.  On `end_session` the entire vec
    // is serialised to JSON and fsynced, growing proportionally.
    //
    // Fix applied: `MAX_TURN_METRICS = 1_000` cap.  When the vec is at capacity
    // `record_turn_estimate` evicts the oldest entry (`remove(0)`) before
    // pushing the new one.  A separate `total_turns: u64` field tracks the true
    // cumulative turn count so `turn_number` stays monotonically increasing and
    // `cumulative_usage` is unaffected by eviction.
    // -----------------------------------------------------------------------

    /// #285: push 10 000 turns — `turn_metrics` vec must never exceed
    /// `MAX_TURN_METRICS` and `total_turns` must equal exactly 10 000.
    #[test]
    fn issue_285_turn_metrics_stays_bounded_under_push_pressure() {
        let mut session = Session::new_initializer();

        let pushes = MAX_TURN_METRICS * 10; // 10 000 turns
        for _ in 0..pushes {
            session.record_turn_estimate(1_000, 100, 80, 20);
        }

        assert_eq!(
            session.turn_metrics.len(),
            MAX_TURN_METRICS,
            "turn_metrics.len() must be capped at MAX_TURN_METRICS after {pushes} pushes",
        );
        assert_eq!(
            session.total_turns, pushes as u64,
            "total_turns must equal the total number of record_turn_estimate calls"
        );
    }

    /// #285: after eviction, `turn_number` in the remaining entries is still
    /// monotonically increasing (oldest entries were evicted, not shuffled).
    #[test]
    fn issue_285_evicted_turn_numbers_remain_monotonic() {
        let mut session = Session::new_initializer();

        // Push cap + 5 entries so eviction has definitely happened.
        let pushes = MAX_TURN_METRICS + 5;
        for _ in 0..pushes {
            session.record_turn_estimate(500, 50, 40, 10);
        }

        // The ring holds the most-recent MAX_TURN_METRICS entries.
        // Their turn_numbers must be strictly increasing.
        let nums: Vec<u64> = session.turn_metrics.iter().map(|t| t.turn_number).collect();
        for window in nums.windows(2) {
            assert!(
                window[0] < window[1],
                "turn_numbers must be strictly increasing after eviction: {:?}",
                &window
            );
        }

        // The oldest retained entry's turn_number must be > the evicted count.
        let evicted = (pushes - MAX_TURN_METRICS) as u64;
        assert!(
            nums[0] > evicted,
            "first retained turn_number ({}) must be > evicted count ({})",
            nums[0],
            evicted
        );
    }

    /// #285: `cumulative_usage` accumulates across all turns including evicted ones —
    /// it must not be reset when the ring wraps.
    #[test]
    fn issue_285_cumulative_usage_unaffected_by_eviction() {
        let mut session = Session::new_initializer();

        // Push enough turns to trigger eviction, recording actual usage each time.
        let turns = MAX_TURN_METRICS + 50;
        for _ in 0..turns {
            session.record_turn_estimate(100, 10, 8, 2);
            session.record_actual_usage(crate::session::TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
        }

        assert_eq!(
            session.cumulative_usage.input_tokens, turns as u64,
            "cumulative input_tokens must count all turns, not just retained ones"
        );
        assert_eq!(
            session.cumulative_usage.output_tokens, turns as u64,
            "cumulative output_tokens must count all turns, not just retained ones"
        );
    }
}
