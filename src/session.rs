//! Session Manager - Tracks agent sessions with initializer/coding agent patterns.
//!
//! Implements two-part session architecture:
//! - Initializer Agent: First session, creates progress files and feature lists
//! - Coding Agent: Subsequent sessions, reads git logs and progress files
//!
//! Treats agents like shift workers with documented handoffs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use uuid::Uuid;

// =========================================================================
// Structured Task Management
// =========================================================================

/// Status of a managed task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
        }
    }
}

/// A structured task with dependency tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier (auto-incrementing, e.g. "task-1")
    pub id: String,
    /// Brief title in imperative form (e.g. "Add permission system")
    pub subject: String,
    /// Detailed description of the task
    pub description: String,
    /// Present continuous form for spinner display (e.g. "Adding permission system")
    pub active_form: Option<String>,
    /// Current task status
    pub status: TaskStatus,
    /// IDs of tasks that this task blocks (downstream dependencies)
    pub blocks: Vec<String>,
    /// IDs of tasks that block this task (upstream dependencies)
    pub blocked_by: Vec<String>,
    /// When the task was created
    pub created_at: DateTime<Utc>,
}

/// Manages structured tasks with dependency tracking.
///
/// Enforces the invariant that only one task can be `InProgress` at a time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskManager {
    tasks: Vec<Task>,
    next_id: u64,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    /// Create a new empty TaskManager.
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }

    /// Create a new task. Returns the created task.
    pub fn create_task(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
    ) -> &Task {
        let id = format!("task-{}", self.next_id);
        self.next_id += 1;

        let task = Task {
            id,
            subject,
            description,
            active_form,
            status: TaskStatus::Pending,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            created_at: Utc::now(),
        };
        self.tasks.push(task);
        self.tasks.last().unwrap()
    }

    /// Get a task by ID.
    pub fn get_task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == task_id)
    }

    /// Get a mutable reference to a task by ID.
    fn get_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
    }

    /// Update a task's fields. Returns an error message if validation fails.
    ///
    /// Enforces that only one task can be `InProgress` at a time. When a task
    /// is set to `InProgress`, any currently in-progress task is moved back to
    /// `Pending`.
    pub fn update_task(
        &mut self,
        task_id: &str,
        status: Option<&str>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        add_blocks: Option<Vec<String>>,
        add_blocked_by: Option<Vec<String>>,
    ) -> Result<&Task, String> {
        // Validate the task exists
        if self.get_task(task_id).is_none() {
            return Err(format!("Task '{}' not found", task_id));
        }

        // Parse and validate the new status
        let new_status = if let Some(s) = status {
            match s {
                "pending" => Some(TaskStatus::Pending),
                "in_progress" => Some(TaskStatus::InProgress),
                "completed" => Some(TaskStatus::Completed),
                "deleted" => {
                    // Remove the task entirely
                    self.tasks.retain(|t| t.id != task_id);
                    // Return a synthetic "deleted" result -- but we can't return a ref
                    // to a deleted task. Instead, just return an error-like message.
                    return Err(format!("Task '{}' deleted", task_id));
                }
                other => return Err(format!("Invalid status '{}'. Must be: pending, in_progress, completed, deleted", other)),
            }
        } else {
            None
        };

        // If setting to InProgress, demote any currently in-progress task
        if new_status == Some(TaskStatus::InProgress) {
            for task in &mut self.tasks {
                if task.status == TaskStatus::InProgress && task.id != task_id {
                    task.status = TaskStatus::Pending;
                }
            }
        }

        // Validate dependency references
        if let Some(ref block_ids) = add_blocks {
            for bid in block_ids {
                if bid == task_id {
                    return Err("A task cannot block itself".to_string());
                }
                if !self.tasks.iter().any(|t| t.id == *bid) {
                    return Err(format!("Referenced task '{}' not found", bid));
                }
            }
        }
        if let Some(ref blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if bid == task_id {
                    return Err("A task cannot be blocked by itself".to_string());
                }
                if !self.tasks.iter().any(|t| t.id == *bid) {
                    return Err(format!("Referenced task '{}' not found", bid));
                }
            }
        }

        // Apply updates to the task
        let task = self.get_task_mut(task_id).unwrap();

        if let Some(s) = new_status {
            task.status = s;
        }
        if let Some(subj) = subject {
            task.subject = subj;
        }
        if let Some(desc) = description {
            task.description = desc;
        }
        if active_form.is_some() {
            task.active_form = active_form;
        }
        if let Some(block_ids) = add_blocks {
            for bid in block_ids {
                if !task.blocks.contains(&bid) {
                    task.blocks.push(bid.clone());
                }
                // Also add the reverse relationship on the other task
                // We need to drop the mutable borrow first, so we collect and do it below
            }
        }
        if let Some(blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if !task.blocked_by.contains(&bid) {
                    task.blocked_by.push(bid.clone());
                }
            }
        }

        // Now handle reverse relationships for add_blocks/add_blocked_by
        // We need to re-borrow after the first mutable borrow ends
        let task_id_owned = task_id.to_string();

        // For add_blocks: if task A blocks task B, then B.blocked_by should include A
        if let Some(s) = status {
            // Re-read the blocks that were just added (they're on the task now)
            // Actually we need the original add_blocks/add_blocked_by args, but they've been moved.
            // We handle this by doing a second pass.
            let _ = s; // suppress unused warning
        }

        // Second pass: sync reverse dependencies
        // Collect the current blocks and blocked_by for the target task
        let current_blocks: Vec<String> = self.get_task(&task_id_owned).map(|t| t.blocks.clone()).unwrap_or_default();
        let current_blocked_by: Vec<String> = self.get_task(&task_id_owned).map(|t| t.blocked_by.clone()).unwrap_or_default();

        // For each task that this task blocks, ensure they have us in blocked_by
        for bid in &current_blocks {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocked_by.contains(&task_id_owned) {
                    other.blocked_by.push(task_id_owned.clone());
                }
            }
        }

        // For each task that blocks this task, ensure they have us in blocks
        for bid in &current_blocked_by {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocks.contains(&task_id_owned) {
                    other.blocks.push(task_id_owned.clone());
                }
            }
        }

        Ok(self.get_task(&task_id_owned).unwrap())
    }

    /// List all tasks.
    pub fn list_tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Get the currently in-progress task, if any.
    pub fn current_task(&self) -> Option<&Task> {
        self.tasks.iter().find(|t| t.status == TaskStatus::InProgress)
    }

    /// Format a task summary for display.
    pub fn format_task_summary(task: &Task) -> String {
        let status_icon = match task.status {
            TaskStatus::Pending => "[ ]",
            TaskStatus::InProgress => "[>]",
            TaskStatus::Completed => "[x]",
        };

        let mut summary = format!(
            "{} {} {} ({})",
            status_icon, task.id, task.subject, task.status
        );

        if let Some(ref af) = task.active_form {
            summary.push_str(&format!(" -- {}", af));
        }

        if !task.blocks.is_empty() {
            summary.push_str(&format!("\n    blocks: {}", task.blocks.join(", ")));
        }
        if !task.blocked_by.is_empty() {
            summary.push_str(&format!("\n    blocked_by: {}", task.blocked_by.join(", ")));
        }

        summary
    }

    /// Format full task details for display.
    pub fn format_task_detail(task: &Task) -> String {
        let mut detail = String::new();
        detail.push_str(&format!("ID: {}\n", task.id));
        detail.push_str(&format!("Subject: {}\n", task.subject));
        detail.push_str(&format!("Status: {}\n", task.status));
        detail.push_str(&format!("Description: {}\n", task.description));
        if let Some(ref af) = task.active_form {
            detail.push_str(&format!("Active form: {}\n", af));
        }
        detail.push_str(&format!("Created: {}\n", task.created_at.format("%Y-%m-%d %H:%M:%S UTC")));
        if !task.blocks.is_empty() {
            detail.push_str(&format!("Blocks: {}\n", task.blocks.join(", ")));
        }
        if !task.blocked_by.is_empty() {
            detail.push_str(&format!("Blocked by: {}\n", task.blocked_by.join(", ")));
        }
        detail
    }
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

/// Token usage from a single API response
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens billed
    pub input_tokens: u64,
    /// Output tokens billed
    pub output_tokens: u64,
    /// Tokens read from cache (reduced cost)
    pub cache_read_tokens: u64,
    /// Tokens written to cache
    pub cache_write_tokens: u64,
}

impl TokenUsage {
    /// Total tokens (input + output)
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Accumulate usage from another TokenUsage
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

/// Metrics for a single API turn (round-trip)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMetrics {
    /// Turn number within the session
    pub turn_number: u64,
    /// Pre-request estimated input tokens (from our estimator)
    pub estimated_input_tokens: usize,
    /// Actual usage reported by the provider (if available)
    pub actual_usage: Option<TokenUsage>,
    /// Tokens consumed by injected context (rules, hooks, session, MCP tools)
    pub injected_context_tokens: usize,
    /// Tokens consumed by system prompt
    pub system_prompt_tokens: usize,
    /// Tokens consumed by tool definitions
    pub tool_def_tokens: usize,
    /// When this turn occurred
    pub timestamp: DateTime<Utc>,
    /// VDD: number of adversarial iterations this turn (if VDD active)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vdd_iterations: Option<u32>,
    /// VDD: genuine findings count
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vdd_genuine_findings: Option<u32>,
    /// VDD: false positive count
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vdd_false_positives: Option<u32>,
    /// VDD: tokens used by adversary model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vdd_adversary_tokens: Option<TokenUsage>,
    /// VDD: whether the loop converged
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vdd_converged: Option<bool>,
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
    /// Per-turn metrics history
    #[serde(default)]
    pub turn_metrics: Vec<TurnMetrics>,
}

impl Session {
    /// Create a new initializer session
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
        }
    }

    /// Create a new coding session continuing from a parent
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

    /// Record metrics for an API turn (pre-request estimation)
    pub fn record_turn_estimate(
        &mut self,
        estimated_input_tokens: usize,
        injected_context_tokens: usize,
        system_prompt_tokens: usize,
        tool_def_tokens: usize,
    ) -> u64 {
        let turn_number = self.turn_metrics.len() as u64 + 1;
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
    pub fn stats_summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("Session: {}\n", self.id));
        s.push_str(&format!("Mode: {:?}\n", self.mode));
        s.push_str(&format!("Turns: {}\n", self.turn_metrics.len()));
        s.push_str(&format!("Requests: {}\n", self.request_count));
        s.push_str(&format!(
            "Input tokens:  {} (cumulative)\n",
            self.cumulative_usage.input_tokens
        ));
        s.push_str(&format!(
            "Output tokens: {} (cumulative)\n",
            self.cumulative_usage.output_tokens
        ));
        s.push_str(&format!(
            "Cache read:    {}\n",
            self.cumulative_usage.cache_read_tokens
        ));
        s.push_str(&format!(
            "Cache write:   {}\n",
            self.cumulative_usage.cache_write_tokens
        ));
        s.push_str(&format!(
            "Total tokens:  {}\n",
            self.cumulative_usage.total()
        ));

        if let Some(last) = self.turn_metrics.last() {
            s.push_str(&format!(
                "\nLast turn #{}: estimated {} input tokens",
                last.turn_number, last.estimated_input_tokens
            ));
            if let Some(actual) = &last.actual_usage {
                s.push_str(&format!(
                    ", actual {}in/{}out",
                    actual.input_tokens, actual.output_tokens
                ));
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
    pub fn generate_handoff(&self) -> String {
        let mut handoff = String::new();

        handoff.push_str("## Session Handoff\n\n");
        handoff.push_str(&format!("Previous Session: {}\n", self.id));
        handoff.push_str(&format!("Mode: {:?}\n", self.mode));
        handoff.push_str(&format!(
            "Duration: {} to {}\n\n",
            self.created_at.format("%Y-%m-%d %H:%M UTC"),
            self.updated_at.format("%Y-%m-%d %H:%M UTC")
        ));

        if !self.progress.completed_tasks.is_empty() {
            handoff.push_str("### Completed Tasks\n");
            for task in &self.progress.completed_tasks {
                handoff.push_str(&format!("- [x] {}\n", task));
            }
            handoff.push('\n');
        }

        if !self.progress.in_progress_tasks.is_empty() {
            handoff.push_str("### In Progress\n");
            for task in &self.progress.in_progress_tasks {
                handoff.push_str(&format!("- [ ] {}\n", task));
            }
            handoff.push('\n');
        }

        if !self.progress.pending_tasks.is_empty() {
            handoff.push_str("### Pending Tasks\n");
            for task in &self.progress.pending_tasks {
                handoff.push_str(&format!("- [ ] {}\n", task));
            }
            handoff.push('\n');
        }

        if !self.progress.decisions.is_empty() {
            handoff.push_str("### Key Decisions\n");
            for decision in &self.progress.decisions {
                handoff.push_str(&format!("- {}\n", decision));
            }
            handoff.push('\n');
        }

        if !self.progress.files_modified.is_empty() {
            handoff.push_str("### Files Modified\n");
            for file in &self.progress.files_modified {
                handoff.push_str(&format!("- {}\n", file));
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
            handoff.push_str(&format!(
                "- Input: {} tokens\n",
                self.cumulative_usage.input_tokens
            ));
            handoff.push_str(&format!(
                "- Output: {} tokens\n",
                self.cumulative_usage.output_tokens
            ));
            handoff.push_str(&format!(
                "- Cache read: {} tokens\n",
                self.cumulative_usage.cache_read_tokens
            ));
            handoff.push_str(&format!("- Turns: {}\n", self.turn_metrics.len()));
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
    /// Structured task manager for task_create/update/get/list tools
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
    pub fn get_or_create_session(&mut self) -> &Session {
        if self.current_session.is_none() {
            self.current_session = Some(self.create_session());
        }
        self.current_session.as_ref().unwrap()
    }

    /// Get the current session mutably
    pub fn get_session_mut(&mut self) -> Option<&mut Session> {
        self.current_session.as_mut()
    }

    /// Get the current session immutably
    pub fn get_session(&self) -> Option<&Session> {
        self.current_session.as_ref()
    }

    /// Store VDD advisory context to inject into the next turn
    pub fn store_vdd_context(&mut self, context: String) {
        self.vdd_pending_context = Some(context);
    }

    /// Take (consume) the pending VDD context for injection
    pub fn take_vdd_context(&mut self) -> Option<String> {
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
    pub fn start_initializer(&mut self) -> &Session {
        let session = Session::new_initializer();
        info!(session_id = %session.id, "Started initializer session");
        self.current_session = Some(session);
        self.current_session.as_ref().unwrap()
    }

    /// Start a coding session from a parent
    pub fn start_coding(&mut self, parent_id: &str) -> &Session {
        let session = Session::new_coding(parent_id);
        info!(
            session_id = %session.id,
            parent_id = %parent_id,
            "Started coding session"
        );
        self.current_session = Some(session);
        self.current_session.as_ref().unwrap()
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

    /// Persist a session to disk
    fn persist_session(&self, session: &Session) -> anyhow::Result<()> {
        let filename = format!("{}.json", session.id);
        let path = self.persist_dir.join(&filename);

        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json)?;

        debug!(path = ?path, "Persisted session");

        // Also update the "latest" symlink/file
        let latest_path = self.persist_dir.join("latest.json");
        fs::write(&latest_path, serde_json::to_string_pretty(session)?)?;

        // Generate and save handoff document
        let handoff_path = self.persist_dir.join("handoff.md");
        fs::write(&handoff_path, session.generate_handoff())?;

        Ok(())
    }

    /// Load a session by ID
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        let path = self.persist_dir.join(format!("{}.json", session_id));
        self.load_session_from_path(&path)
    }

    /// Load the most recent session
    pub fn load_latest_session(&self) -> Option<Session> {
        let path = self.persist_dir.join("latest.json");
        self.load_session_from_path(&path)
    }

    /// Load a session from a file path
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
    pub fn get_handoff_context(&self) -> Option<String> {
        let handoff_path = self.persist_dir.join("handoff.md");
        fs::read_to_string(&handoff_path).ok()
    }

    /// List all persisted sessions
    pub fn list_sessions(&self) -> Vec<Session> {
        let mut sessions = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.persist_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    // Skip latest.json as it's a copy
                    if path.file_stem().map(|s| s == "latest").unwrap_or(false) {
                        continue;
                    }
                    if let Some(session) = self.load_session_from_path(&path) {
                        sessions.push(session);
                    }
                }
            }
        }

        // Sort by created_at descending
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
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

/// Plan mode state for the agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanModeState {
    /// Whether plan mode is currently active
    pub active: bool,
    /// Path to the plan file
    pub plan_file: PathBuf,
    /// Allowed prompts when exiting plan mode
    pub allowed_prompts: Vec<AllowedPrompt>,
}

/// An allowed prompt constraint for plan mode exit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedPrompt {
    /// Tool name this prompt applies to
    pub tool: String,
    /// Prompt/description for the allowed operation
    pub prompt: String,
}

/// Tools that are allowed in plan mode (read-only + user interaction)
pub const PLAN_MODE_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "grep",
    "web_fetch",
    "web_search",
    "web_browser",
    "ask_user_question",
    "task",
    "agent_output",
    "todo_read",
    "chainlink",
    "bash_output",
];

/// Tools that are always blocked in plan mode (write/mutate operations)
pub const PLAN_MODE_BLOCKED_TOOLS: &[&str] = &[
    "bash",
    "edit_file",
    "kill_shell",
    "todo_write",
];

/// Check if a tool is allowed in plan mode.
/// write_file is special: it's allowed only if targeting the plan file path.
pub fn is_tool_allowed_in_plan_mode(tool_name: &str, plan_file: &Path, args: &serde_json::Value) -> bool {
    // Always-allowed tools
    if PLAN_MODE_ALLOWED_TOOLS.contains(&tool_name) {
        return true;
    }

    // Always-blocked tools
    if PLAN_MODE_BLOCKED_TOOLS.contains(&tool_name) {
        return false;
    }

    // write_file is allowed ONLY if targeting the plan file
    if tool_name == "write_file" {
        if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
            let target = Path::new(path_str);
            // Compare canonical paths to handle relative vs absolute
            let target_canonical = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
            let plan_canonical = std::fs::canonicalize(plan_file).unwrap_or_else(|_| plan_file.to_path_buf());
            return target_canonical == plan_canonical;
        }
        return false;
    }

    // enter_plan_mode and exit_plan_mode are always allowed
    if tool_name == "enter_plan_mode" || tool_name == "exit_plan_mode" {
        return true;
    }

    // Unknown tools are blocked in plan mode
    false
}

/// Context to inject at session start based on mode
pub fn get_session_context(session: &Session) -> String {
    match session.mode {
        SessionMode::Initializer => r#"## Session Context: Initializer Agent

You are the first agent working on this task. Your responsibilities:
1. Understand the full scope of the work
2. Create a clear plan with actionable steps
3. Document key decisions and rationale
4. Set up any necessary project structure
5. Prepare detailed handoff notes for subsequent sessions

Focus on establishing a solid foundation that future agents can build upon."#
            .to_string(),
        SessionMode::Coding => {
            let mut context = r#"## Session Context: Coding Agent

You are continuing work from a previous session. Your responsibilities:
1. Review the handoff notes from the previous session
2. Continue from where the last agent left off
3. Track your progress and decisions
4. Prepare handoff notes if you won't complete the task

"#
            .to_string();

            // Add parent session info if available
            if let Some(parent_id) = &session.parent_session_id {
                context.push_str(&format!("Previous session ID: {}\n", parent_id));
            }

            context
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

    // ====================================================================
    // TaskManager Tests
    // ====================================================================

    #[test]
    fn test_task_manager_create() {
        let mut tm = TaskManager::new();
        let task = tm.create_task(
            "Implement feature".to_string(),
            "Add the new feature".to_string(),
            Some("Implementing feature".to_string()),
        );
        assert_eq!(task.id, "task-1");
        assert_eq!(task.subject, "Implement feature");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.active_form, Some("Implementing feature".to_string()));
    }

    #[test]
    fn test_task_manager_auto_increment() {
        let mut tm = TaskManager::new();
        tm.create_task("A".to_string(), "Desc".to_string(), None);
        tm.create_task("B".to_string(), "Desc".to_string(), None);
        tm.create_task("C".to_string(), "Desc".to_string(), None);

        let tasks = tm.list_tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "task-1");
        assert_eq!(tasks[1].id, "task-2");
        assert_eq!(tasks[2].id, "task-3");
    }

    #[test]
    fn test_task_manager_update_status() {
        let mut tm = TaskManager::new();
        tm.create_task("Task A".to_string(), "Desc".to_string(), None);

        let result = tm.update_task("task-1", Some("in_progress"), None, None, None, None, None);
        assert!(result.is_ok());
        assert_eq!(tm.get_task("task-1").unwrap().status, TaskStatus::InProgress);
    }

    #[test]
    fn test_task_manager_single_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("Task A".to_string(), "Desc".to_string(), None);
        tm.create_task("Task B".to_string(), "Desc".to_string(), None);

        tm.update_task("task-1", Some("in_progress"), None, None, None, None, None).unwrap();
        assert_eq!(tm.get_task("task-1").unwrap().status, TaskStatus::InProgress);

        tm.update_task("task-2", Some("in_progress"), None, None, None, None, None).unwrap();
        assert_eq!(tm.get_task("task-1").unwrap().status, TaskStatus::Pending);
        assert_eq!(tm.get_task("task-2").unwrap().status, TaskStatus::InProgress);
    }

    #[test]
    fn test_task_manager_delete() {
        let mut tm = TaskManager::new();
        tm.create_task("To delete".to_string(), "Desc".to_string(), None);
        assert_eq!(tm.list_tasks().len(), 1);

        let result = tm.update_task("task-1", Some("deleted"), None, None, None, None, None);
        // "deleted" returns an Err with the deletion message
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deleted"));
        assert_eq!(tm.list_tasks().len(), 0);
    }

    #[test]
    fn test_task_manager_invalid_status() {
        let mut tm = TaskManager::new();
        tm.create_task("Task".to_string(), "Desc".to_string(), None);

        let result = tm.update_task("task-1", Some("invalid"), None, None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid status"));
    }

    #[test]
    fn test_task_manager_not_found() {
        let mut tm = TaskManager::new();
        let result = tm.update_task("task-999", Some("completed"), None, None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_task_manager_dependencies() {
        let mut tm = TaskManager::new();
        tm.create_task("Setup".to_string(), "First step".to_string(), None);
        tm.create_task("Build".to_string(), "Second step".to_string(), None);

        // task-2 blocked by task-1
        tm.update_task("task-2", None, None, None, None, None, Some(vec!["task-1".to_string()])).unwrap();

        let task1 = tm.get_task("task-1").unwrap();
        let task2 = tm.get_task("task-2").unwrap();
        assert!(task2.blocked_by.contains(&"task-1".to_string()));
        assert!(task1.blocks.contains(&"task-2".to_string()));
    }

    #[test]
    fn test_task_manager_self_dependency_blocked() {
        let mut tm = TaskManager::new();
        tm.create_task("Task".to_string(), "Desc".to_string(), None);

        let result = tm.update_task("task-1", None, None, None, None, Some(vec!["task-1".to_string()]), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot block itself"));
    }

    #[test]
    fn test_task_manager_current_task() {
        let mut tm = TaskManager::new();
        assert!(tm.current_task().is_none());

        tm.create_task("Task".to_string(), "Desc".to_string(), None);
        assert!(tm.current_task().is_none()); // still pending

        tm.update_task("task-1", Some("in_progress"), None, None, None, None, None).unwrap();
        assert!(tm.current_task().is_some());
        assert_eq!(tm.current_task().unwrap().id, "task-1");
    }

    #[test]
    fn test_task_manager_format_summary() {
        let mut tm = TaskManager::new();
        let task = tm.create_task(
            "Fix bug".to_string(),
            "Fix the null pointer".to_string(),
            Some("Fixing bug".to_string()),
        );
        let summary = TaskManager::format_task_summary(task);
        assert!(summary.contains("[ ]")); // pending icon
        assert!(summary.contains("task-1"));
        assert!(summary.contains("Fix bug"));
        assert!(summary.contains("Fixing bug"));
    }

    #[test]
    fn test_session_manager_has_task_manager() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));
        // Verify we can access and use the task manager
        manager.task_manager.create_task(
            "Test".to_string(),
            "Test task".to_string(),
            None,
        );
        assert_eq!(manager.task_manager.list_tasks().len(), 1);
    }
}
