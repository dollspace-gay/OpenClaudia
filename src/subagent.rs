//! Subagent System for `OpenClaudia`
//!
//! Provides Claude Code-style subagent capabilities:
//! - Task tool for spawning autonomous sub-agents
//! - `AgentOutput` tool for retrieving background agent results
//! - Agent type configurations with specialized system prompts
//! - Isolated conversation contexts per subagent
//! - Background execution with async tracking

use crate::config::AppConfig;
use crate::tools::{safe_truncate, ToolCall};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use uuid::Uuid;

/// Maximum turns a subagent can execute before forced termination
const MAX_SUBAGENT_TURNS: usize = 50;

/// Maximum tokens for subagent responses
const SUBAGENT_MAX_TOKENS: u32 = 8192;

/// Agent types available for spawning
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    /// General-purpose agent for complex multi-step tasks
    GeneralPurpose,
    /// Fast agent for codebase exploration and searches
    Explore,
    /// Software architect agent for designing implementation plans
    Plan,
    /// Documentation lookup agent
    Guide,
    /// Multi-agent orchestrator that delegates work to worker agents
    Coordinator,
}

impl AgentType {
    /// Every agent type, in display order. Stable order so `/agents`
    /// output doesn't shuffle between runs.
    pub const ALL: &'static [Self] = &[
        Self::GeneralPurpose,
        Self::Explore,
        Self::Plan,
        Self::Guide,
        Self::Coordinator,
    ];

    /// Canonical kebab-case name as accepted by `parse_type` and the
    /// `task` tool's `subagent_type` field.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::GeneralPurpose => "general-purpose",
            Self::Explore => "explore",
            Self::Plan => "plan",
            Self::Guide => "claude-code-guide",
            Self::Coordinator => "coordinator",
        }
    }

    /// One-line human-readable description for help output.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        match self {
            Self::GeneralPurpose => "Complex multi-step tasks with full tool access",
            Self::Explore => "Fast codebase exploration and searches (read-only)",
            Self::Plan => "Software architect for implementation plans (read-only)",
            Self::Guide => "Documentation lookup and usage questions",
            Self::Coordinator => "Multi-agent orchestrator that delegates work",
        }
    }

    /// Parse agent type from string
    #[must_use]
    pub fn parse_type(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "general-purpose" | "general_purpose" | "generalpurpose" => Some(Self::GeneralPurpose),
            "explore" | "explorer" => Some(Self::Explore),
            "plan" | "planner" => Some(Self::Plan),
            "guide" | "claude-code-guide" => Some(Self::Guide),
            "coordinator" => Some(Self::Coordinator),
            _ => None,
        }
    }

    /// Get the system prompt for this agent type
    #[must_use]
    pub const fn system_prompt(&self) -> &'static str {
        match self {
            Self::GeneralPurpose => GENERAL_PURPOSE_PROMPT,
            Self::Explore => EXPLORE_PROMPT,
            Self::Plan => PLAN_PROMPT,
            Self::Guide => GUIDE_PROMPT,
            Self::Coordinator => COORDINATOR_PROMPT,
        }
    }

    /// Get the tools available to this agent type
    #[must_use]
    pub fn allowed_tools(&self) -> Vec<&'static str> {
        match self {
            Self::GeneralPurpose => vec![
                "bash",
                "bash_output",
                "kill_shell",
                "read_file",
                "write_file",
                "edit_file",
                "list_files",
                "web_fetch",
                "web_search",
            ],
            Self::Explore => {
                vec!["bash", "read_file", "list_files", "web_fetch", "web_search"]
            }
            Self::Plan => vec!["bash", "read_file", "list_files", "web_fetch", "web_search"],
            Self::Guide => vec!["read_file", "list_files", "web_fetch", "web_search"],
            Self::Coordinator => vec![
                "task",
                "agent_output",
                "task_create",
                "task_update",
                "task_get",
                "task_list",
                "ask_user_question",
                "read_file",
                "list_files",
                "web_search",
                "web_fetch",
            ],
        }
    }

    /// Get model preference for this agent type
    #[must_use]
    pub const fn preferred_model(&self) -> Option<&'static str> {
        match self {
            Self::Explore | Self::Guide => Some("haiku"),
            Self::Coordinator | Self::GeneralPurpose | Self::Plan => None,
        }
    }
}

// === System Prompts for Agent Types ===

const GENERAL_PURPOSE_PROMPT: &str = r"You are a specialized subagent spawned to handle a complex task autonomously.

Your goal is to complete the assigned task thoroughly and return a comprehensive summary of what you accomplished.

Guidelines:
- Work autonomously to complete the task
- Use tools as needed to accomplish your goal
- Be thorough but efficient
- When you're done, provide a clear summary of:
  - What was accomplished
  - Any files created or modified
  - Any issues encountered
  - Recommendations for follow-up if needed

You have access to file and shell tools. Use them to explore the codebase, make changes, and verify your work.";

const EXPLORE_PROMPT: &str = r"You are a fast exploration agent specialized for searching codebases.

Your goal is to quickly find relevant files, code patterns, and answer questions about the codebase structure.

Guidelines:
- Use bash with grep, find, or similar tools to search efficiently
- Read files to understand their contents
- Be fast and focused - don't over-explore
- Return a concise summary of what you found including:
  - Relevant file paths
  - Key code snippets or patterns
  - Direct answers to the question asked

Focus on speed and relevance. Don't modify any files - this is read-only exploration.";

const PLAN_PROMPT: &str = r"You are a software architect agent for designing implementation plans.

Your goal is to analyze the codebase and design a clear implementation strategy for the requested feature or change.

Guidelines:
- Explore the existing codebase to understand patterns and architecture
- Identify the files that need to be modified
- Consider edge cases and potential issues
- Design a step-by-step implementation plan

Return a structured plan including:
- Overview of the approach
- Files to create or modify
- Step-by-step implementation steps
- Potential risks or considerations
- Dependencies or prerequisites

Do NOT implement the changes - only plan them.";

const GUIDE_PROMPT: &str = r"You are a documentation lookup agent.

Your goal is to find and summarize relevant documentation for the user's question.

Guidelines:
- Search for relevant documentation files
- Use web search to find official documentation
- Provide clear, accurate information
- Include relevant code examples when helpful

Return a helpful answer with sources cited.";

const COORDINATOR_PROMPT: &str = "You are a coordinator agent responsible for multi-agent orchestration.

You break down complex tasks into smaller units of work and delegate them to specialized worker agents. You do NOT execute tools directly \u{2014} no bash commands, no file writes, no file edits. Your job is to plan, delegate, monitor, and synthesize.

## Workflow

1. **Research**: Use read_file, list_files, web_search, and web_fetch to understand the problem space, codebase structure, and requirements before delegating.
2. **Planning**: Decompose the task into discrete sub-tasks. Use task_create to track each one. Identify dependencies and ordering constraints.
3. **Delegation**: Spawn worker agents via the `task` tool to execute each sub-task. Each worker prompt must be fully self-contained \u{2014} include all file paths, context, and acceptance criteria the worker needs. Never assume workers share your context.
4. **Monitoring**: Use agent_output to check on background workers. Use task_update to record progress. Re-delegate or adjust the plan if a worker fails or produces unexpected results.
5. **Synthesis**: Once all sub-tasks complete, combine worker outputs into a coherent final summary. Report what was accomplished, what failed, and any follow-up needed.

## Worker Types

- **general-purpose**: Implementation workers that can read, write, and edit files, run shell commands. Use for coding tasks, refactoring, test writing.
- **explore**: Fast read-only search agents. Use for finding files, code patterns, or understanding codebase structure.
- **plan**: Architecture agents that analyze code and produce implementation plans. Use when you need a detailed design before implementation.
- **guide**: Documentation lookup agents. Use for finding API docs, library usage, or reference material.

## Rules

- NEVER use bash, write_file, or edit_file \u{2014} you do not have access to these tools.
- Every worker prompt must be self-contained: include file paths, expected behavior, and all relevant context.
- Use task_create/task_update to maintain a clear record of sub-tasks and their status.
- Prefer spawning workers in background (run_in_background: true) when tasks are independent, then collect results with agent_output.
- If a worker fails, analyze the failure and either retry with a corrected prompt or adjust your plan.
- Use ask_user_question when requirements are ambiguous or you need clarification before proceeding.
- Always provide a final summary that maps each sub-task to its outcome.";

// === Background Agent Management ===

/// Retention TTL for finished background agents (1 hour).
///
/// Entries that have been finished for longer than this are evicted by
/// [`BackgroundAgentManager::gc`] on the next manager touch. Exposed as a
/// constant so tests can compare against it.
///
/// Fix for crosslink #422: without a sweep the `agents` map grew
/// unboundedly — a session spawning ~10 agents/hour over 8 hours leaked
/// ~80 finished `BackgroundAgent` Arcs, each carrying full output and
/// task description.
pub const FINISHED_AGENT_TTL_SECS: u64 = 60 * 60;

/// State of a running or completed background agent
#[derive(Debug)]
pub struct BackgroundAgent {
    /// Unique agent ID
    pub id: String,
    /// Agent type
    pub agent_type: AgentType,
    /// Task description
    pub task: String,
    /// Whether the agent has finished
    pub finished: AtomicBool,
    /// Final result (populated when finished)
    pub result: Mutex<Option<String>>,
    /// Error message if failed
    pub error: Mutex<Option<String>>,
    /// Number of turns executed
    pub turns: AtomicU64,
    /// When the agent transitioned to `finished`. `None` while still running.
    /// Used by [`BackgroundAgentManager::gc`] to evict entries past
    /// [`FINISHED_AGENT_TTL_SECS`].
    pub finished_at: Mutex<Option<Instant>>,
}

/// Manager for background agents
pub struct BackgroundAgentManager {
    agents: Mutex<HashMap<String, Arc<BackgroundAgent>>>,
}

impl BackgroundAgentManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new background agent.
    ///
    /// Also opportunistically sweeps expired finished agents
    /// (see [`Self::gc`]) so the map cannot grow unbounded across a session.
    pub fn register(&self, agent_type: AgentType, task: &str) -> String {
        // Sweep before insert so the cost of growth is amortized against
        // the spawn that causes it (crosslink #422).
        self.gc();

        let id = safe_truncate(&Uuid::new_v4().to_string(), 8).to_string();
        let agent = Arc::new(BackgroundAgent {
            id: id.clone(),
            agent_type,
            task: task.to_string(),
            finished: AtomicBool::new(false),
            result: Mutex::new(None),
            error: Mutex::new(None),
            turns: AtomicU64::new(0),
            finished_at: Mutex::new(None),
        });

        if let Ok(mut agents) = self.agents.lock() {
            agents.insert(id.clone(), agent);
        }

        id
    }

    /// Get an agent by ID
    pub fn get(&self, id: &str) -> Option<Arc<BackgroundAgent>> {
        self.agents.lock().ok()?.get(id).cloned()
    }

    /// Mark an agent as finished with a result
    pub fn finish(&self, id: &str, result: String) {
        if let Some(agent) = self.get(id) {
            if let Ok(mut r) = agent.result.lock() {
                *r = Some(result);
            }
            if let Ok(mut t) = agent.finished_at.lock() {
                *t = Some(Instant::now());
            }
            agent.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Mark an agent as failed with an error
    pub fn fail(&self, id: &str, error: String) {
        if let Some(agent) = self.get(id) {
            if let Ok(mut e) = agent.error.lock() {
                *e = Some(error);
            }
            if let Ok(mut t) = agent.finished_at.lock() {
                *t = Some(Instant::now());
            }
            agent.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Increment turn counter for an agent
    pub fn increment_turns(&self, id: &str) -> u64 {
        self.get(id)
            .map_or(0, |agent| agent.turns.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// List all agents.
    ///
    /// Sweeps expired finished agents first (see [`Self::gc`]) so callers
    /// — including the TUI agent list — never observe leaked stale entries.
    pub fn list(&self) -> Vec<(String, AgentType, String, bool)> {
        self.gc();
        self.agents.lock().map_or_else(
            |_| Vec::new(),
            |agents| {
                agents
                    .iter()
                    .map(|(id, agent)| {
                        (
                            id.clone(),
                            agent.agent_type,
                            agent.task.clone(),
                            agent.finished.load(Ordering::SeqCst),
                        )
                    })
                    .collect()
            },
        )
    }

    /// Remove an agent unconditionally
    pub fn remove(&self, id: &str) -> Option<Arc<BackgroundAgent>> {
        self.agents.lock().ok()?.remove(id)
    }

    /// Garbage-collect finished agents older than [`FINISHED_AGENT_TTL_SECS`].
    ///
    /// Running agents (`finished == false`) are never removed regardless of
    /// how long they have been registered — only completion age triggers
    /// eviction. Returns the number of removed entries.
    ///
    /// Fix for crosslink #422 — replaces unbounded growth with a bounded
    /// retention window. Safe to call from any context (poisoned lock is
    /// treated as a no-op).
    pub fn gc(&self) -> usize {
        let now = Instant::now();
        let Ok(mut agents) = self.agents.lock() else {
            return 0;
        };
        let before = agents.len();
        agents.retain(|_, agent| {
            if !agent.finished.load(Ordering::SeqCst) {
                return true;
            }
            // Finished: keep only if not yet past TTL. Missing/poisoned
            // timestamp counts as "evict" so a half-initialized entry
            // cannot pin memory forever.
            agent
                .finished_at
                .lock()
                .ok()
                .and_then(|t| *t)
                .is_some_and(|t| now.duration_since(t).as_secs() < FINISHED_AGENT_TTL_SECS)
        });
        before.saturating_sub(agents.len())
    }

    /// Public hook for shutdown paths (e.g. `tui.rs`) that want to drop
    /// every finished agent up-front rather than wait for TTL expiry.
    /// Returns the number of agents removed.
    pub fn cleanup_finished(&self) -> usize {
        let Ok(mut agents) = self.agents.lock() else {
            return 0;
        };
        let before = agents.len();
        agents.retain(|_, agent| !agent.finished.load(Ordering::SeqCst));
        before.saturating_sub(agents.len())
    }
}

impl Default for BackgroundAgentManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global background agent manager
pub static BACKGROUND_AGENTS: LazyLock<BackgroundAgentManager> =
    LazyLock::new(BackgroundAgentManager::new);

// === Transcript Storage for Resume ===

/// Stored transcript for a completed agent, enabling resume
pub(crate) struct StoredTranscript {
    messages: Vec<Value>,
    agent_type: AgentType,
    created_at: Instant,
}

/// TTL for stored transcripts (30 minutes).
const TRANSCRIPT_TTL_SECS: u64 = 30 * 60;

/// Hard cap on the number of transcripts retained at once. When the
/// store would exceed this, the oldest entry (by `created_at`) is
/// evicted in O(log N) via the auxiliary `BTreeSet` index — see
/// [`TranscriptStore::insert`]. Crosslink #415.
pub(crate) const MAX_STORED_TRANSCRIPTS: usize = 50;

/// Hard cap on the number of messages retained per transcript. When a
/// caller stores a longer message list, the head is dropped and only
/// the most recent `MAX_MESSAGES_PER_TRANSCRIPT` messages are kept; a
/// `tracing::warn!` is emitted noting how many were dropped. Crosslink
/// #415.
pub(crate) const MAX_MESSAGES_PER_TRANSCRIPT: usize = 500;

/// Interval at which the background sweep runs TTL eviction.
const SWEEP_INTERVAL_SECS: u64 = 60;

/// Bounded transcript store with O(log N) LRU eviction.
///
/// Crosslink #415: the previous implementation iterated the entire
/// `HashMap` on every insert (O(N)) and only ran eviction when a
/// caller stored or loaded — meaning a long-running session with no
/// new spawns would never reclaim memory. This struct:
///
/// 1. Hard-caps the number of transcripts at `MAX_STORED_TRANSCRIPTS`,
///    evicting the oldest in O(log N) via a `BTreeSet` index keyed by
///    `(created_at, id)`.
/// 2. Truncates per-transcript message lists at
///    `MAX_MESSAGES_PER_TRANSCRIPT`, keeping the most recent messages
///    so resume retains the latest conversation context.
/// 3. Provides a `sweep` entry point invoked from a background tokio
///    task (see [`spawn_transcript_sweeper`]) so TTL eviction runs
///    independently of insert/load traffic.
pub(crate) struct TranscriptStore {
    entries: HashMap<String, StoredTranscript>,
    /// Insertion-time-ordered index: `(created_at, agent_id)`.
    /// `Instant` is monotonic; collisions on the same instant are
    /// broken by `agent_id` so each entry has a unique key. The
    /// `BTreeSet` first element is always the oldest, giving O(log N)
    /// LRU eviction without a full `HashMap` scan.
    order: BTreeSet<(Instant, String)>,
}

impl TranscriptStore {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeSet::new(),
        }
    }

    /// Number of stored transcripts. Test-only.
    #[cfg(test)]
    fn len(&self) -> usize {
        debug_assert_eq!(self.entries.len(), self.order.len());
        self.entries.len()
    }

    /// Insert (or replace) a transcript for `agent_id`.
    ///
    /// On replace the prior entry's order-index slot is removed so the
    /// two indexes stay in sync. After insertion, if the store exceeds
    /// `MAX_STORED_TRANSCRIPTS`, the oldest entry is evicted.
    fn insert(&mut self, agent_id: String, transcript: StoredTranscript) {
        // If replacing, remove the prior entry from the order index so
        // we don't leak a stale key.
        if let Some(old) = self.entries.remove(&agent_id) {
            self.order.remove(&(old.created_at, agent_id.clone()));
        }
        self.order.insert((transcript.created_at, agent_id.clone()));
        self.entries.insert(agent_id, transcript);

        // Enforce the hard cap. O(log N) per eviction.
        while self.entries.len() > MAX_STORED_TRANSCRIPTS {
            // `order` is non-empty here: `entries.len() > 0` and the
            // two indexes are kept in lockstep, so unwrap-on-None
            // would indicate an invariant violation.
            let oldest = self
                .order
                .iter()
                .next()
                .cloned()
                .expect("order index must mirror entries when entries is non-empty");
            self.order.remove(&oldest);
            self.entries.remove(&oldest.1);
        }
    }

    fn get(&self, agent_id: &str) -> Option<&StoredTranscript> {
        self.entries.get(agent_id)
    }

    /// Remove every entry whose age exceeds `TRANSCRIPT_TTL_SECS`.
    /// Uses the ordered index so we stop scanning as soon as we hit
    /// the first non-expired entry (entries are ordered oldest-first).
    /// Returns the number of evicted entries.
    fn sweep(&mut self, now: Instant) -> usize {
        let ttl = Duration::from_secs(TRANSCRIPT_TTL_SECS);
        let mut removed = 0;
        while let Some(oldest) = self.order.iter().next().cloned() {
            if now.duration_since(oldest.0) < ttl {
                // Ordered oldest-first: nothing further can be expired.
                break;
            }
            self.order.remove(&oldest);
            self.entries.remove(&oldest.1);
            removed += 1;
        }
        removed
    }
}

/// Global transcript store for agent resume. Bounded by
/// `MAX_STORED_TRANSCRIPTS` and swept periodically by the background
/// task spawned in [`spawn_transcript_sweeper`].
pub(crate) static TRANSCRIPT_STORE: LazyLock<Mutex<TranscriptStore>> =
    LazyLock::new(|| Mutex::new(TranscriptStore::new()));

/// Guards the one-shot spawn of the transcript sweeper task. Calling
/// `spawn_transcript_sweeper` more than once is a no-op.
static SWEEPER_INIT: Once = Once::new();

/// Spawn (once per process) a tokio task that periodically sweeps
/// expired transcripts. Idempotent — subsequent calls are no-ops.
///
/// Returns `true` iff this call performed the spawn. The function is
/// safe to call when no tokio runtime is in scope (e.g. unit tests
/// without `#[tokio::test]`); in that case the spawn is skipped and
/// the `Once` is still marked complete so a later call doesn't try
/// again.
pub(crate) fn spawn_transcript_sweeper() -> bool {
    let mut spawned = false;
    SWEEPER_INIT.call_once(|| {
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async {
                let mut ticker =
                    tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
                // The first tick fires immediately; skip it so the
                // first sweep happens after one full interval (avoids
                // a thundering-herd sweep at process start).
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let removed = TRANSCRIPT_STORE
                        .lock()
                        .map_or(0, |mut store| store.sweep(Instant::now()));
                    if removed > 0 {
                        tracing::debug!(
                            evicted = removed,
                            "transcript sweep evicted expired entries"
                        );
                    }
                }
            });
            spawned = true;
        }
    });
    spawned
}

/// Store a transcript for future resume.
///
/// Enforces the per-transcript message cap (`MAX_MESSAGES_PER_TRANSCRIPT`)
/// by retaining the most recent messages; warns when truncation occurs.
/// Also ensures the background sweeper has been spawned so TTL
/// eviction does not depend on insert traffic.
fn store_transcript(agent_id: &str, mut messages: Vec<Value>, agent_type: AgentType) {
    // Make sure the background TTL sweep is running. Idempotent.
    let _ = spawn_transcript_sweeper();

    if messages.len() > MAX_MESSAGES_PER_TRANSCRIPT {
        let dropped = messages.len() - MAX_MESSAGES_PER_TRANSCRIPT;
        tracing::warn!(
            agent_id = %agent_id,
            total = messages.len(),
            cap = MAX_MESSAGES_PER_TRANSCRIPT,
            dropped,
            "transcript exceeds per-transcript message cap; dropping oldest messages"
        );
        // Keep the tail (most recent messages) so resume retains the
        // latest conversation context. `drain(..dropped)` is O(N) on
        // the dropped prefix but bounded by `dropped` and only runs
        // when the cap is exceeded.
        messages.drain(..dropped);
    }

    if let Ok(mut store) = TRANSCRIPT_STORE.lock() {
        store.insert(
            agent_id.to_string(),
            StoredTranscript {
                messages,
                agent_type,
                created_at: Instant::now(),
            },
        );
    }
}

/// Load a stored transcript for resume.
///
/// No longer scans the entire map for expired entries on every call
/// — the background sweep (see [`spawn_transcript_sweeper`]) handles
/// that. Per-call eviction is also unnecessary because every read
/// path verifies the entry's own age in O(1).
fn load_transcript(agent_id: &str) -> Option<(Vec<Value>, AgentType)> {
    // Tighten lock scope: read out what we need, then release before
    // the rest of the function body. The clippy
    // `significant_drop_tightening` lint flags holding a `MutexGuard`
    // longer than necessary.
    let snapshot = TRANSCRIPT_STORE
        .lock()
        .ok()
        .and_then(|store| {
            store
                .get(agent_id)
                .map(|entry| (entry.messages.clone(), entry.agent_type, entry.created_at))
        });
    let (messages, agent_type, created_at) = snapshot?;
    // Treat an expired entry as absent so resume fails cleanly even
    // if the background sweep is briefly behind.
    if Instant::now().duration_since(created_at).as_secs() >= TRANSCRIPT_TTL_SECS {
        return None;
    }
    Some((messages, agent_type))
}

// === Worktree Isolation ===

/// State for a git worktree used by an agent
#[derive(Debug, Clone)]
pub struct WorktreeIsolation {
    pub worktree_path: PathBuf,
    pub branch_name: String,
}

impl WorktreeIsolation {
    /// Create a new git worktree for agent isolation.
    ///
    /// # Errors
    ///
    /// Returns `Err` if git is not available, the current directory is not
    /// a git repository, or the worktree/branch creation fails.
    pub fn create(agent_id: &str) -> Result<Self, String> {
        let branch_name = format!("agent/{agent_id}");

        // Find the git root
        let git_root = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|e| format!("git not available: {e}"))?;

        if !git_root.status.success() {
            return Err("Not in a git repository".to_string());
        }

        let root = String::from_utf8_lossy(&git_root.stdout).trim().to_string();
        let worktree_dir = Path::new(&root).join(".openclaudia").join("worktrees");

        // Ensure worktree directory exists
        std::fs::create_dir_all(&worktree_dir)
            .map_err(|e| format!("Failed to create worktree directory: {e}"))?;

        let worktree_path = worktree_dir.join(agent_id);

        // Create the worktree
        let result = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                worktree_path.to_str().unwrap_or(""),
                "-b",
                &branch_name,
            ])
            .output()
            .map_err(|e| format!("Failed to create worktree: {e}"))?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(format!("git worktree add failed: {stderr}"));
        }

        Ok(Self {
            worktree_path,
            branch_name,
        })
    }

    /// Check if the worktree has uncommitted changes
    #[must_use]
    pub fn has_changes(&self) -> bool {
        let result = std::process::Command::new("git")
            .args([
                "-C",
                self.worktree_path.to_str().unwrap_or(""),
                "diff",
                "--stat",
            ])
            .output();

        match result {
            Ok(output) => !output.stdout.is_empty(),
            Err(_) => false,
        }
    }

    /// Remove the worktree (only if no changes).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the worktree has uncommitted changes or if the
    /// git worktree remove command fails.
    pub fn cleanup(&self) -> Result<(), String> {
        if self.has_changes() {
            return Err(format!(
                "Worktree has changes \u{2014} keeping at {} on branch {}",
                self.worktree_path.display(),
                self.branch_name
            ));
        }

        let result = std::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                self.worktree_path.to_str().unwrap_or(""),
                "--force",
            ])
            .output()
            .map_err(|e| format!("Failed to remove worktree: {e}"))?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(format!("git worktree remove failed: {stderr}"));
        }

        // Also delete the branch
        let _ = std::process::Command::new("git")
            .args(["branch", "-D", &self.branch_name])
            .output();

        Ok(())
    }
}

// === Model Name Resolution ===

/// Map friendly model names to actual model IDs
fn resolve_model_name(friendly: &str, _provider: &str) -> String {
    match friendly.to_lowercase().as_str() {
        "opus" => "claude-opus-4-6".to_string(),
        "sonnet" => "claude-sonnet-4-6".to_string(),
        "haiku" => "claude-haiku-4-5-20251001".to_string(),
        other => other.to_string(),
    }
}

// === Tool Definitions ===

/// Get the Task tool definition
#[must_use]
pub fn get_task_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task",
            "description": "Launch a subagent to handle a complex task autonomously. The subagent runs with its own conversation context and tool access, then returns a summary when done. Use 'run_in_background: true' for long tasks. Use 'resume' with a previous agent_id to continue from where it left off.",
            "parameters": {
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A short (3-5 word) description of the task"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Detailed task instructions for the subagent"
                    },
                    "subagent_type": {
                        "type": "string",
                        "enum": ["general-purpose", "explore", "plan", "guide"],
                        "description": "The type of specialized agent: 'general-purpose' for complex tasks, 'explore' for fast codebase searches, 'plan' for architecture design, 'guide' for documentation lookup"
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "If true, run in background and return an agent_id. Use agent_output to retrieve results later."
                    },
                    "resume": {
                        "type": "string",
                        "description": "Optional agent ID to resume from. The agent continues with its full previous context preserved."
                    },
                    "model": {
                        "type": "string",
                        "enum": ["sonnet", "opus", "haiku"],
                        "description": "Optional model to use. 'haiku' for quick tasks, 'opus' for complex reasoning, 'sonnet' (default) for balanced."
                    },
                    "isolation": {
                        "type": "string",
                        "enum": ["worktree"],
                        "description": "Set to 'worktree' to run the agent in an isolated git worktree. Changes are kept if the agent modifies files."
                    }
                },
                "required": ["description", "prompt", "subagent_type"]
            }
        }
    })
}

/// Get the `AgentOutput` tool definition
#[must_use]
pub fn get_agent_output_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_output",
            "description": "Retrieve the result from a background agent. If the agent is still running, returns current status. Use 'block: true' to wait for completion (only when you have nothing else to do).",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent ID returned from a task call with run_in_background=true"
                    },
                    "block": {
                        "type": "boolean",
                        "description": "If true, wait for the agent to complete (max 5 minutes). Default false."
                    }
                },
                "required": ["agent_id"]
            }
        }
    })
}

/// Get all subagent tool definitions
#[must_use]
pub fn get_subagent_tool_definitions() -> Value {
    json!([
        get_task_tool_definition(),
        get_agent_output_tool_definition()
    ])
}

// === Subagent Execution ===

/// Configuration for running a subagent
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub agent_type: AgentType,
    pub task: String,
    pub prompt: String,
    pub run_in_background: bool,
    pub model_override: Option<String>,
    pub resume_agent_id: Option<String>,
    pub isolation: Option<String>,
}

/// Result from a subagent execution
#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub agent_id: String,
    pub success: bool,
    pub output: String,
    pub turns_used: u64,
    pub is_background: bool,
    pub worktree: Option<WorktreeIsolation>,
}

/// Run a subagent synchronously, returning the final result
#[allow(clippy::too_many_lines)]
pub async fn run_subagent(
    config: &SubagentConfig,
    app_config: &AppConfig,
    client: &Client,
) -> SubagentResult {
    // Handle resume: reuse previous agent_id and load transcript
    let (agent_id, mut messages) = if let Some(ref resume_id) = config.resume_agent_id {
        match load_transcript(resume_id) {
            Some((prev_messages, _prev_type)) => {
                // Re-register with same ID for tracking
                let id = BACKGROUND_AGENTS.register(config.agent_type, &config.task);
                let mut msgs = prev_messages;
                // Append the new prompt as a continuation
                msgs.push(json!({
                    "role": "user",
                    "content": format!("Continuing from where you left off.\n\n{}", config.prompt)
                }));
                (id, msgs)
            }
            None => {
                return SubagentResult {
                    agent_id: resume_id.clone(),
                    success: false,
                    output: format!("No transcript found for agent '{resume_id}'. It may have expired (TTL: {} minutes).", TRANSCRIPT_TTL_SECS / 60),
                    turns_used: 0,
                    is_background: config.run_in_background,
                    worktree: None,
                };
            }
        }
    } else {
        let id = BACKGROUND_AGENTS.register(config.agent_type, &config.task);
        let system_prompt = config.agent_type.system_prompt();
        let msgs = vec![
            json!({
                "role": "system",
                "content": system_prompt
            }),
            json!({
                "role": "user",
                "content": format!("Task: {}\n\n{}", config.task, config.prompt)
            }),
        ];
        (id, msgs)
    };

    // Set up worktree isolation if requested
    let worktree = if config.isolation.as_deref() == Some("worktree") {
        match WorktreeIsolation::create(&agent_id) {
            Ok(wt) => {
                // Set working directory for tool execution by injecting context
                messages.push(json!({
                    "role": "system",
                    "content": format!(
                        "You are running in an isolated git worktree at: {}\nBranch: {}\nAll file operations should use paths relative to or within this directory.",
                        wt.worktree_path.display(), wt.branch_name
                    )
                }));
                Some(wt)
            }
            Err(e) => {
                return SubagentResult {
                    agent_id,
                    success: false,
                    output: format!("Failed to create worktree: {e}"),
                    turns_used: 0,
                    is_background: config.run_in_background,
                    worktree: None,
                };
            }
        }
    } else {
        None
    };

    let allowed_tools = config.agent_type.allowed_tools();

    // Filter tool definitions to only allowed tools
    let all_tools = crate::tools::get_tool_definitions();
    let filtered_tools: Vec<Value> = all_tools
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|tool| {
                    tool.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .is_some_and(|name| allowed_tools.contains(&name))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    // Determine the model to use
    let model = config
        .model_override
        .clone()
        .or_else(|| config.agent_type.preferred_model().map(String::from))
        .unwrap_or_else(|| {
            app_config
                .providers
                .get(&app_config.proxy.target)
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-6".to_string())
        });

    // Get provider config. `api_key` is `Option<ApiKey>`: an unconfigured
    // provider yields `None` and `make_api_call` omits the auth header —
    // previously this was `String::new()` (an empty key sent as
    // `Bearer <empty>`, which every upstream rejects). See crosslink #256.
    let (base_url, api_key) = app_config
        .providers
        .get(&app_config.proxy.target)
        .map_or_else(
            || ("https://api.anthropic.com/v1".to_string(), None),
            |provider_config| {
                (
                    provider_config.base_url.clone(),
                    provider_config.api_key.clone(),
                )
            },
        );

    // Run the agent loop
    let mut final_output = String::new();
    let mut turns: u64;

    // Library-layer permission gate — consulted by every
    // `execute_tool_with_memory` call inside this subagent's tool loop.
    // Closes crosslink #505 for the subagent path.
    let permission_mgr = crate::permissions::PermissionManager::new(
        std::path::PathBuf::from(".openclaudia/permissions.json"),
        true,
        app_config.permissions.default_allow.clone(),
    );

    loop {
        turns = BACKGROUND_AGENTS.increment_turns(&agent_id);

        if turns > MAX_SUBAGENT_TURNS as u64 {
            BACKGROUND_AGENTS.fail(
                &agent_id,
                format!("Agent exceeded maximum turns ({MAX_SUBAGENT_TURNS})"),
            );
            // Store transcript even on failure for potential resume
            store_transcript(&agent_id, messages, config.agent_type);
            return SubagentResult {
                agent_id,
                success: false,
                output: format!("Agent exceeded maximum turns ({MAX_SUBAGENT_TURNS})"),
                turns_used: turns,
                is_background: config.run_in_background,
                worktree: worktree.clone(),
            };
        }

        // Build the request
        let request_body = json!({
            "model": model,
            "messages": messages,
            "tools": filtered_tools,
            "max_tokens": SUBAGENT_MAX_TOKENS
        });

        // Make the API call — provider is plumbed through so the
        // ProviderAdapter trait (canonical implementation in
        // `src/providers/`) handles request/response transformation for
        // every supported provider. See crosslink #407.
        let response = match make_api_call(
            client,
            &app_config.proxy.target,
            &base_url,
            api_key.as_ref(),
            &request_body,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                BACKGROUND_AGENTS.fail(&agent_id, e.clone());
                store_transcript(&agent_id, messages, config.agent_type);
                return SubagentResult {
                    agent_id,
                    success: false,
                    output: e,
                    turns_used: turns,
                    is_background: config.run_in_background,
                    worktree: worktree.clone(),
                };
            }
        };

        // Parse the response
        let assistant_message = match parse_response(&response) {
            Ok(msg) => msg,
            Err(e) => {
                BACKGROUND_AGENTS.fail(&agent_id, e.clone());
                store_transcript(&agent_id, messages, config.agent_type);
                return SubagentResult {
                    agent_id,
                    success: false,
                    output: e,
                    turns_used: turns,
                    is_background: config.run_in_background,
                    worktree: worktree.clone(),
                };
            }
        };

        // Check for text content (final response)
        if let Some(content) = assistant_message.get("content") {
            if let Some(text) = content.as_str() {
                if !text.is_empty() {
                    final_output = text.to_string();
                }
            } else if let Some(arr) = content.as_array() {
                // Handle Anthropic-style content array
                for part in arr {
                    if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                final_output = text.to_string();
                            }
                        }
                    }
                }
            }
        }

        // Check for tool calls
        let tool_calls = assistant_message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // No tool calls means agent is done
            break;
        }

        // Add assistant message to history
        messages.push(assistant_message.clone());

        // Execute tool calls and add results
        let empty_obj = json!({});
        for tool_call in &tool_calls {
            let tool_id = tool_call
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("unknown");
            let function = tool_call.get("function").unwrap_or(&empty_obj);
            let name = function
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let arguments = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("{}");

            // Check if tool is allowed
            if !allowed_tools.contains(&name) {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_id,
                    "content": format!("Error: Tool '{name}' is not available to this agent type")
                }));
                continue;
            }

            // Execute the tool with the library-layer permission gate
            // engaged (crosslink #505).
            let tc = ToolCall {
                id: tool_id.to_string(),
                call_type: "function".to_string(),
                function: crate::tools::FunctionCall {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                },
            };

            // Bind the subagent's id as the session key so its task
            // list lives in its own bucket. Claude Code uses the
            // `agentId ?? sessionId` fallback; here agent_id is always
            // present. Closes crosslink #518 for subagents.
            let _session_guard = crate::tools::SessionIdGuard::set(&agent_id);
            let result = crate::tools::execute_tool_with_memory(&tc, None, Some(&permission_mgr));

            messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_id,
                "content": result.content
            }));
        }
    }

    // Mark as finished and store transcript for future resume
    BACKGROUND_AGENTS.finish(&agent_id, final_output.clone());
    store_transcript(&agent_id, messages, config.agent_type);

    // Handle worktree cleanup: remove if no changes, keep if changes exist
    let final_worktree = worktree.and_then(|wt| {
        if wt.has_changes() {
            Some(wt) // Keep -- return path and branch to caller
        } else {
            let _ = wt.cleanup(); // No changes, clean up silently
            None
        }
    });

    SubagentResult {
        agent_id,
        success: true,
        output: final_output,
        turns_used: turns,
        is_background: config.run_in_background,
        worktree: final_worktree,
    }
}

/// Canonical provider names accepted by the subagent dispatcher.
///
/// This list mirrors the explicit (non-fallback) arms of
/// [`crate::providers::get_adapter`]. The crate-level `get_adapter`
/// deliberately falls back to the OpenAI-compatible adapter for
/// unknown names (typo-tolerant proxy use case); subagent dispatch has
/// the opposite preference — an unknown provider is an operator
/// configuration error that must surface as a clean error rather than
/// silently translating Anthropic-targeted prompts through an
/// OpenAI-shape body. See crosslink #407.
const SUBAGENT_KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "gemini",
    "deepseek",
    "qwen",
    "alibaba",
    "zai",
    "glm",
    "zhipu",
    "ollama",
    "local",
    "lmstudio",
    "localai",
    "text-generation-webui",
];

/// Validate a provider name and return its canonical
/// [`crate::providers::ProviderAdapter`].
///
/// Returns a typed error string instead of silently falling back so
/// that misconfigured subagents fail fast at the dispatch boundary
/// instead of issuing wrong-shape HTTP requests upstream.
fn resolve_subagent_adapter(
    provider: &str,
) -> Result<&'static dyn crate::providers::ProviderAdapter, String> {
    let normalized = provider.to_ascii_lowercase();
    if !SUBAGENT_KNOWN_PROVIDERS.contains(&normalized.as_str()) {
        return Err(format!(
            "Unknown subagent provider '{provider}'. Configure one of: {}",
            SUBAGENT_KNOWN_PROVIDERS.join(", ")
        ));
    }
    crate::providers::get_adapter(&normalized).map_err(|e| {
        format!("Subagent provider '{provider}' adapter lookup failed: {e}")
    })
}

/// Decode the in-flight subagent `request_body` JSON into the typed
/// [`crate::proxy::ChatCompletionRequest`] that every adapter consumes.
///
/// The subagent loop builds its working state as untyped `serde_json`
/// to keep the message-append path cheap; the typed struct is the
/// canonical input expected by every provider adapter and is the
/// reason this refactor is type-safe rather than yet another bag of
/// `Value::get(...)` calls. Errors are surfaced verbatim so a
/// malformed request body produces a debuggable agent-error message.
fn build_chat_completion_request(
    request_body: &Value,
) -> Result<crate::proxy::ChatCompletionRequest, String> {
    serde_json::from_value::<crate::proxy::ChatCompletionRequest>(request_body.clone())
        .map_err(|e| format!("Failed to materialize ChatCompletionRequest: {e}"))
}

/// Make an API call to the LLM provider.
///
/// Provider transformation is delegated to the canonical
/// [`crate::providers::ProviderAdapter`] trait so subagent dispatch
/// supports every provider the proxy supports (Anthropic, `OpenAI`,
/// `Google`/`Gemini`, `DeepSeek`, Qwen, Z.AI/GLM, Ollama,
/// `OpenAI`-compatible) instead of a hardcoded Anthropic-vs-`OpenAI`
/// branch. The previous implementation duplicated provider
/// transformation logic from `src/providers/` and only handled two
/// out of seven formats — see crosslink #407.
///
/// `api_key` is an optional [`crate::providers::ApiKey`]; when `None`
/// the auth header is omitted rather than sent empty. See crosslink
/// #256.
async fn make_api_call(
    client: &Client,
    provider: &str,
    base_url: &str,
    api_key: Option<&crate::providers::ApiKey>,
    request_body: &Value,
) -> Result<Value, String> {
    // Resolve the typed adapter for this provider — strict validation
    // so an unknown provider name fails fast at the dispatch boundary
    // (see `resolve_subagent_adapter`).
    let adapter = resolve_subagent_adapter(provider)?;

    // Materialize the typed request the adapter trait consumes.
    let typed_request = build_chat_completion_request(request_body)?;

    // Transform via the canonical adapter — handles every provider's
    // wire format, including Anthropic prompt-cache `cache_control`
    // headers, Google `generationConfig`, Ollama `options`, etc.
    let body = adapter
        .transform_request(&typed_request)
        .map_err(|e| format!("Adapter transform_request failed: {e}"))?;

    // Endpoint path is adapter-owned (Google's path embeds the model
    // name, Ollama uses /api/chat, Anthropic uses /v1/messages, etc.).
    // We strip the `/v1` suffix from the configured base_url because
    // every adapter's endpoint already encodes its own version
    // segment — matching the URL composition rule in
    // `src/vdd/transport.rs::forward_request`.
    let normalized_base = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    let endpoint = format!("{normalized_base}{}", adapter.chat_endpoint(&typed_request.model));

    // Headers come from the adapter when an api_key is present. We
    // ensure a content-type header is set in all cases so providers
    // without an explicit content-type contribution still receive
    // valid JSON.
    let mut headers: Vec<(String, String)> = api_key
        .map(|k| adapter.get_headers(k))
        .unwrap_or_default();
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push((
            "content-type".to_string(),
            "application/json".to_string(),
        ));
    }

    let mut req = client.post(&endpoint);
    for (key, value) in headers {
        req = req.header(&key, &value);
    }
    req = req.json(&body);

    let response = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("API error ({status}): {text}"));
    }

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;

    // Translate provider-native response back to OpenAI chat shape so
    // `parse_response` (which expects `choices[0].message`) keeps
    // working unchanged for every provider.
    adapter
        .transform_response(json, false)
        .map_err(|e| format!("Adapter transform_response failed: {e}"))
}

/// Parse the response to extract the assistant message
fn parse_response(response: &Value) -> Result<Value, String> {
    // OpenAI format
    if let Some(choices) = response.get("choices").and_then(|c| c.as_array()) {
        if let Some(first) = choices.first() {
            if let Some(message) = first.get("message") {
                return Ok(message.clone());
            }
        }
    }

    // Direct message (already transformed)
    if response.get("role").is_some() {
        return Ok(response.clone());
    }

    Err("Could not parse response".to_string())
}

// === Tool Execution ===

/// Execute the Task tool
pub fn execute_task_tool<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    app_config: &AppConfig,
) -> (String, bool) {
    let Some(description) = args.get("description").and_then(|v| v.as_str()) else {
        return ("Missing 'description' argument".to_string(), true);
    };

    let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
        return ("Missing 'prompt' argument".to_string(), true);
    };

    // Handle resume: if resume ID is provided, load previous transcript
    let resume_id = args
        .get("resume")
        .and_then(|v| v.as_str())
        .map(String::from);

    let Some(subagent_type_str) = args.get("subagent_type").and_then(|v| v.as_str()) else {
        return ("Missing 'subagent_type' argument".to_string(), true);
    };

    let Some(agent_type) = AgentType::parse_type(subagent_type_str) else {
        return (
            format!(
                "Unknown agent type '{subagent_type_str}'. Valid types: general-purpose, explore, plan, guide"
            ),
            true,
        );
    };

    let run_in_background = args
        .get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Resolve model: map friendly names to actual model IDs
    let model_override = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|m| resolve_model_name(m, &app_config.proxy.target));

    let isolation = args
        .get("isolation")
        .and_then(|v| v.as_str())
        .map(String::from);

    let config = SubagentConfig {
        agent_type,
        task: description.to_string(),
        prompt: prompt.to_string(),
        run_in_background,
        model_override,
        resume_agent_id: resume_id,
        isolation,
    };

    // Create HTTP client
    let client = Client::new();

    if run_in_background {
        // Register the agent and spawn the task
        let agent_id = BACKGROUND_AGENTS.register(agent_type, description);

        // Spawn the background task
        let config_bg = config;
        let app_config_bg = app_config.clone();
        let client_bg = client;
        let agent_id_bg = agent_id.clone();

        // Use tokio runtime to spawn the background task
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let result = run_subagent(&config_bg, &app_config_bg, &client_bg).await;

                if !result.success {
                    BACKGROUND_AGENTS.fail(&agent_id_bg, result.output);
                }
            });
        }

        let message = format!(
            "Background agent started with ID: {agent_id}\nTask: {description}\nType: {agent_type:?}\n\nUse agent_output with this agent_id to retrieve results."
        );

        (message, false)
    } else {
        // Run synchronously via defensive runtime dispatch (#719).
        dispatch_subagent_sync(&config, app_config, &client)
    }
}

/// Synchronous-call-from-tool-dispatch path for `run_subagent`.
///
/// Defensive runtime dispatch (#719): `tokio::task::block_in_place` PANICS
/// when called from a `current_thread` runtime (e.g. `#[tokio::test]`,
/// `tokio_test::block_on`, many CLI harnesses). `Handle::block_on` from
/// inside any runtime worker is also a documented deadlock risk because
/// the inner future may yield back to the same executor that's blocked
/// on its completion.
///
/// Resolution policy:
///   * No runtime in scope    → create a dedicated runtime (CLI/tool
///     dispatch boundary; acceptable, single allocation).
///   * `MultiThread` runtime  → `block_in_place` + `block_on` is safe;
///     `block_in_place` moves us off the worker thread.
///   * `CurrentThread`        → fail fast with a typed error. We cannot
///     `block_in_place` (panics) and cannot `block_on` (deadlocks the
///     single worker). The caller must dispatch through the async path.
fn dispatch_subagent_sync(
    config: &SubagentConfig,
    app_config: &AppConfig,
    client: &Client,
) -> (String, bool) {
    let result = match Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| {
                handle.block_on(run_subagent(config, app_config, client))
            }),
            _ => {
                return (
                    "Task tool dispatched from a current_thread tokio runtime: \
                     cannot block_on without deadlock. Invoke the task tool from \
                     a multi_thread runtime or from the async tool dispatcher."
                        .to_string(),
                    true,
                );
            }
        },
        Err(_) => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(run_subagent(config, app_config, client)),
            Err(e) => {
                return (format!("Failed to create runtime: {e}"), true);
            }
        },
    };

    if result.success {
        let mut message = format!(
            "Agent completed in {} turns.\n\n{}",
            result.turns_used, result.output
        );
        if let Some(ref wt) = result.worktree {
            let _ = write!(
                message,
                "\n\nWorktree: {}\nBranch: {}",
                wt.worktree_path.display(),
                wt.branch_name
            );
        }
        (message, false)
    } else {
        (format!("Agent failed: {}", result.output), true)
    }
}

/// Execute the `AgentOutput` tool
pub fn execute_agent_output_tool<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
) -> (String, bool) {
    let Some(agent_id) = args.get("agent_id").and_then(|v| v.as_str()) else {
        // List all agents if no ID provided
        let agents = BACKGROUND_AGENTS.list();
        if agents.is_empty() {
            return ("No background agents running.".to_string(), false);
        }
        let mut result = format!("Background agents ({}):\n", agents.len());
        for (id, agent_type, task, finished) in agents {
            let status = if finished { "finished" } else { "running" };
            let task_preview = if task.len() > 50 {
                format!("{}...", safe_truncate(&task, 50))
            } else {
                task
            };
            let _ = writeln!(result, "  {id} [{agent_type:?}] [{status}]: {task_preview}");
        }
        return (result, false);
    };

    let block = args
        .get("block")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let Some(agent) = BACKGROUND_AGENTS.get(agent_id) else {
        return (format!("Agent '{agent_id}' not found"), true);
    };

    if block && !agent.finished.load(Ordering::SeqCst) {
        // Wait for completion (up to 5 minutes)
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_mins(5);

        while !agent.finished.load(Ordering::SeqCst) && start.elapsed() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    let finished = agent.finished.load(Ordering::SeqCst);
    let turns = agent.turns.load(Ordering::SeqCst);

    if finished {
        // Get the result or error
        let result = agent.result.lock().ok().and_then(|r| r.clone());
        let error = agent.error.lock().ok().and_then(|e| e.clone());

        // Crosslink #422: once a finished agent has had its output consumed
        // by the caller, drop the map entry so the manager cannot leak
        // finished `BackgroundAgent` Arcs across a long-running session.
        // Drop the local `Arc` clone first so `remove` returns the last
        // strong reference and the heap allocation can actually be freed.
        drop(agent);
        let _ = BACKGROUND_AGENTS.remove(agent_id);

        error.map_or_else(
            || {
                result.map_or_else(
                    || {
                        (
                            format!("Agent '{agent_id}' finished but produced no output"),
                            false,
                        )
                    },
                    |output| {
                        (
                            format!("Agent '{agent_id}' completed in {turns} turns:\n\n{output}"),
                            false,
                        )
                    },
                )
            },
            |err| {
                (
                    format!("Agent '{agent_id}' failed after {turns} turns:\n{err}"),
                    true,
                )
            },
        )
    } else {
        (
            format!(
                "Agent '{agent_id}' is still running ({turns} turns so far)\nTask: {}",
                agent.task
            ),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_parsing() {
        assert_eq!(
            AgentType::parse_type("general-purpose"),
            Some(AgentType::GeneralPurpose)
        );
        assert_eq!(AgentType::parse_type("explore"), Some(AgentType::Explore));
        assert_eq!(AgentType::parse_type("plan"), Some(AgentType::Plan));
        assert_eq!(AgentType::parse_type("guide"), Some(AgentType::Guide));
        assert_eq!(AgentType::parse_type("test-builder"), None);
        assert_eq!(AgentType::parse_type("unknown"), None);
    }

    #[test]
    fn agent_type_all_is_exhaustive() {
        // ALL must list every variant so /agents output stays
        // complete when new agents are added. Round-trip each name
        // through parse_type to catch name/parse drift at compile
        // time… actually at test time. Compile-time would need a
        // match — but test-time is close enough and cheaper.
        for kind in AgentType::ALL {
            let parsed = AgentType::parse_type(kind.name())
                .unwrap_or_else(|| panic!("{} not round-trippable", kind.name()));
            assert_eq!(&parsed, kind);
            assert!(!kind.description().is_empty());
        }
        // Sanity check on the current set — bump this when a variant
        // is added and list it in ALL.
        assert_eq!(AgentType::ALL.len(), 5);
    }

    #[test]
    fn test_tool_definitions() {
        let task_tool = get_task_tool_definition();
        assert!(task_tool.get("function").is_some());
        assert_eq!(
            task_tool
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str(),
            Some("task")
        );

        let agent_output_tool = get_agent_output_tool_definition();
        assert!(agent_output_tool.get("function").is_some());
        assert_eq!(
            agent_output_tool
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str(),
            Some("agent_output")
        );
    }

    #[test]
    fn test_background_agent_manager() {
        let manager = BackgroundAgentManager::new();

        // Register an agent
        let id = manager.register(AgentType::Explore, "Test task");
        assert!(!id.is_empty());

        // Get the agent
        let agent = manager.get(&id);
        assert!(agent.is_some());
        let agent = agent.unwrap();
        assert_eq!(agent.task, "Test task");
        assert!(!agent.finished.load(Ordering::SeqCst));

        // Increment turns
        let turns = manager.increment_turns(&id);
        assert_eq!(turns, 1);

        // Finish the agent
        manager.finish(&id, "Test result".to_string());
        assert!(agent.finished.load(Ordering::SeqCst));
        assert_eq!(
            agent.result.lock().unwrap().as_ref(),
            Some(&"Test result".to_string())
        );
    }

    // ── Crosslink #407: ProviderAdapter dispatch in subagent ────────────────
    //
    // The previous `transform_to_anthropic` / `transform_from_anthropic`
    // functions were a stovepiped reimplementation of
    // `providers::AnthropicAdapter`, with branches that only handled
    // Anthropic + OpenAI. The four tests below pin the new behaviour:
    //
    //   1. Anthropic produces the canonical adapter shape (system array
    //      with cache_control, not the bare string the duplicate emitted).
    //   2. OpenAI passthrough produces a well-formed OpenAI-shape body.
    //   3. Google produces Gemini-shape contents (was broken: the old
    //      Anthropic-vs-OpenAI branch routed Gemini calls through OpenAI
    //      wire format, which Gemini's REST API does not accept).
    //   4. Unknown provider returns a clean typed error rather than
    //      silently falling back to the OpenAI shape.

    fn anthropic_request_body() -> Value {
        json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "system", "content": "System prompt"},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1000
        })
    }

    /// Snapshot the adapter-produced Anthropic body so it matches the
    /// canonical `AnthropicAdapter` contract: `system` is an array of
    /// content blocks with `cache_control`, messages exclude system,
    /// `max_tokens` and `model` round-trip verbatim.
    #[test]
    fn crosslink407_anthropic_request_uses_adapter_shape() {
        let body = anthropic_request_body();
        let typed = build_chat_completion_request(&body).expect("decodable");
        let adapter = resolve_subagent_adapter("anthropic").expect("anthropic is known");

        let out = adapter.transform_request(&typed).expect("transform ok");

        assert_eq!(out.get("model").and_then(|v| v.as_str()), Some("claude-sonnet-4-6"));
        assert_eq!(out.get("max_tokens").and_then(Value::as_u64), Some(1000));

        // System is now the canonical Anthropic array shape with
        // cache_control — the old duplicate emitted a bare string and
        // dropped prompt-cache hits, which #407 fixes by construction.
        let system_arr = out
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must be array, not a bare string");
        assert_eq!(system_arr.len(), 1);
        assert_eq!(system_arr[0].get("type").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(
            system_arr[0].get("text").and_then(|v| v.as_str()),
            Some("System prompt")
        );
        assert_eq!(
            system_arr[0]
                .get("cache_control")
                .and_then(|c| c.get("type"))
                .and_then(|v| v.as_str()),
            Some("ephemeral")
        );

        // Messages exclude the system entry (handled separately at top
        // level by Anthropic) — only the user turn remains.
        let messages = out
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].get("role").and_then(|v| v.as_str()), Some("user"));
    }

    /// `OpenAI` subagent dispatch: produces a well-formed `OpenAI`-shape
    /// body via the canonical adapter. The duplicate code path used to
    /// rely on the literal `request_body.clone()` (no transformation);
    /// going through the adapter is now uniform with every other
    /// provider and ensures the request validates against the typed
    /// `ChatCompletionRequest` contract.
    #[test]
    fn crosslink407_openai_request_passes_through_adapter() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "ping"}
            ],
            "max_tokens": 256
        });
        let typed = build_chat_completion_request(&body).expect("decodable");
        let adapter = resolve_subagent_adapter("openai").expect("openai is known");

        let out = adapter.transform_request(&typed).expect("transform ok");

        assert_eq!(out.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
        let messages = out
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].get("role").and_then(|v| v.as_str()), Some("user"));

        // Endpoint is the OpenAI-shape chat completions path — the
        // adapter owns this string, not subagent.rs.
        assert_eq!(adapter.chat_endpoint("gpt-4o"), "/v1/chat/completions");
    }

    /// Google subagent dispatch — previously broken because the old
    /// `transform_to_anthropic`/`OpenAI`-only branch sent the body as
    /// `{model, messages, ...}` which Gemini's REST API rejects.
    /// Going through `GoogleAdapter` emits the Gemini-native shape
    /// (`contents`, `systemInstruction`, `generationConfig`) and the
    /// model-aware endpoint path.
    #[test]
    fn crosslink407_google_request_uses_gemini_shape() {
        let body = json!({
            "model": "gemini-2.5-pro",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hi"}
            ],
            "temperature": 0.5,
            "max_tokens": 512
        });
        let typed = build_chat_completion_request(&body).expect("decodable");
        let adapter = resolve_subagent_adapter("google").expect("google is known");

        let out = adapter.transform_request(&typed).expect("transform ok");

        // Gemini native shape: `contents`, not OpenAI `messages`.
        assert!(
            out.get("contents").and_then(|v| v.as_array()).is_some(),
            "Google adapter must emit `contents`, got {out:?}"
        );
        assert!(
            out.get("systemInstruction").is_some(),
            "Google adapter must emit `systemInstruction` for system prompt"
        );
        assert_eq!(
            out["generationConfig"]["maxOutputTokens"]
                .as_u64()
                .expect("maxOutputTokens"),
            512
        );

        // Endpoint embeds the model name — proof the adapter (not a
        // hardcoded subagent string) owns URL composition.
        assert!(
            adapter
                .chat_endpoint("gemini-2.5-pro")
                .contains("gemini-2.5-pro"),
            "Gemini endpoint must embed the model name"
        );

        // `gemini` alias also resolves to the Google adapter.
        let alias = resolve_subagent_adapter("gemini").expect("gemini alias is known");
        assert_eq!(alias.name(), "google");
    }

    /// Negative test — an unknown provider name must surface as a
    /// clean typed error at the dispatch boundary, NOT silently fall
    /// back to `OpenAI`. The crate-level `get_adapter` is typo-tolerant
    /// by design (the proxy treats unknown providers as
    /// `OpenAI`-compatible local servers); subagent dispatch has the
    /// opposite preference because a wrongly-routed Anthropic agent
    /// would issue malformed HTTP requests upstream. See crosslink
    /// #407.
    #[test]
    fn crosslink407_unknown_provider_returns_clean_error() {
        let Err(err) = resolve_subagent_adapter("not-a-real-provider") else {
            panic!("unknown provider must error, not silently fall back")
        };
        assert!(
            err.contains("Unknown subagent provider"),
            "error must name the misconfigured provider, got: {err}"
        );
        assert!(
            err.contains("not-a-real-provider"),
            "error must echo the bad provider name, got: {err}"
        );
        // Empty string is also rejected (operator left the field blank).
        assert!(resolve_subagent_adapter("").is_err());

        // Every name in the allow-list resolves successfully.
        for known in SUBAGENT_KNOWN_PROVIDERS {
            assert!(
                resolve_subagent_adapter(known).is_ok(),
                "{known} must resolve"
            );
        }
        // Case-insensitive: operators sometimes capitalise provider
        // names in config (e.g. "Anthropic"). The strict gate must
        // still accept them.
        assert!(resolve_subagent_adapter("Anthropic").is_ok());
        assert!(resolve_subagent_adapter("OPENAI").is_ok());
    }

    // ── Spec #527 behavior 1: run_in_background returns agent_id immediately ──

    /// Spec #527 §1 — `task` with `run_in_background: true` registers a new agent
    /// in `BACKGROUND_AGENTS` and returns a plain-text string containing the ID,
    /// the description, and the agent type. `is_error` must be `false`.
    ///
    /// Pins OC's current output format. CC returns a typed `AgentId`; OC returns an
    /// opaque 8-char UUID prefix — format differs, behavior is pinned as-is.
    #[test]
    fn spec1_run_in_background_registers_agent_returns_id() {
        let mgr = BackgroundAgentManager::new();
        let id = mgr.register(AgentType::Explore, "scan codebase for dead code");
        assert_eq!(id.len(), 8, "OC uses 8-char UUID prefix (safe_truncate)");

        // The agent is immediately retrievable, not yet finished.
        let agent = mgr.get(&id).expect("registered agent must exist");
        assert!(!agent.finished.load(Ordering::SeqCst));
        assert_eq!(agent.task, "scan codebase for dead code");
        assert_eq!(agent.agent_type, AgentType::Explore);
    }

    /// Spec #527 §1 — The message format produced for background spawn includes
    /// the `agent_id`, task description, and a hint to use `agent_output`.
    #[test]
    fn spec1_background_message_format() {
        let mgr = BackgroundAgentManager::new();
        let id = mgr.register(AgentType::Plan, "design the auth layer");

        // Simulate the format string from execute_task_tool (line ~1333).
        let description = "design the auth layer";
        let agent_type = AgentType::Plan;
        let message = format!(
            "Background agent started with ID: {id}\nTask: {description}\nType: {agent_type:?}\n\nUse agent_output with this agent_id to retrieve results."
        );

        assert!(message.contains(&id), "message must embed the agent_id");
        assert!(message.contains(description));
        assert!(message.contains("agent_output"));
    }

    /// Spec #527 §1 — At spawn time the agent is not finished, has no error, and
    /// has no result. `is_error` is `false` for a background spawn.
    #[test]
    fn spec1_is_error_false_and_not_finished_at_spawn() {
        let mgr = BackgroundAgentManager::new();
        let id = mgr.register(AgentType::GeneralPurpose, "refactor module");
        let agent = mgr.get(&id).expect("must exist after register");

        assert!(!agent.finished.load(Ordering::SeqCst));
        assert!(agent.error.lock().unwrap().is_none());
        assert!(agent.result.lock().unwrap().is_none());
    }

    // ── Spec #527 behavior 2: resume loads transcript and appends new prompt ──

    /// Spec #527 §2 — When the transcript store has no entry for the id,
    /// `load_transcript` returns `None`. `run_subagent` converts this to
    /// `success=false` with the "No transcript found" message.
    #[test]
    fn spec2_resume_miss_returns_not_found_error() {
        let missing = load_transcript("00000000-dead-beef-0000-000000000000");
        assert!(
            missing.is_none(),
            "unknown agent_id must return None from transcript store"
        );
    }

    /// Spec #527 §2 — Storing and loading a transcript round-trips correctly;
    /// the loaded messages and `agent_type` match what was stored.
    #[test]
    fn spec2_transcript_store_and_load_round_trip() {
        let msgs = vec![
            json!({"role": "system", "content": "You are a worker."}),
            json!({"role": "user", "content": "Do the thing"}),
            json!({"role": "assistant", "content": "Done."}),
        ];
        let fake_id = format!("tt-{}", Uuid::new_v4());
        store_transcript(&fake_id, msgs.clone(), AgentType::Explore);

        let loaded = load_transcript(&fake_id).expect("stored transcript must be loadable");
        assert_eq!(loaded.0.len(), msgs.len());
        assert_eq!(loaded.1, AgentType::Explore);
        assert_eq!(loaded.0[0]["role"].as_str(), Some("system"));
        assert_eq!(loaded.0[2]["content"].as_str(), Some("Done."));
    }

    /// Spec #527 §2 gap #582 — OC generates a NEW `agent_id` on resume; it does NOT
    /// reuse the original id. CC reuses the original `agentId` for transcript
    /// continuity and prompt-cache hits. This pins the divergent OC behavior.
    #[test]
    fn spec2_gap582_resume_allocates_new_agent_id_not_old_one() {
        let original_id = format!("orig-{}", Uuid::new_v4());
        store_transcript(
            &original_id,
            vec![json!({"role": "user", "content": "Hello"})],
            AgentType::Plan,
        );

        assert!(load_transcript(&original_id).is_some());

        // On resume, OC calls `BACKGROUND_AGENTS.register(...)` → new id.
        let new_id = BACKGROUND_AGENTS.register(AgentType::Plan, "resume task");
        assert_ne!(
            new_id, original_id,
            "gap #582: OC issues a new agent_id on resume (CC reuses original)"
        );
    }

    // ── Spec #527 behavior 5: task_stop — confirmed MISSING (gap #580) ──

    /// Spec #527 §5 gap #580 — OC has no `task_stop` mechanism. The
    /// `BackgroundAgentManager` exposes no abort method. `fail()` exists as
    /// an internal marker but cannot abort a spawned tokio task.
    #[test]
    fn spec5_gap580_no_stop_method_on_background_agent_manager() {
        let mgr = BackgroundAgentManager::new();
        let id = mgr.register(AgentType::GeneralPurpose, "long running task");

        // fail() marks finished=true but does NOT abort the tokio task.
        mgr.fail(&id, "externally terminated".to_string());

        let agent = mgr.get(&id).unwrap();
        assert!(
            agent.finished.load(Ordering::SeqCst),
            "fail() marks finished but cannot abort the tokio task (gap #580)"
        );
        assert_eq!(
            agent.error.lock().unwrap().as_deref(),
            Some("externally terminated")
        );
        // The real task handle (tokio JoinHandle) is not stored in the struct.
        // No field or method enables external abort — that is gap #580.
    }

    /// Spec #527 §5 gap #580 — `get_subagent_tool_definitions()` does NOT include
    /// a `task_stop` tool. CC has `TaskStopTool`; OC does not.
    #[test]
    fn spec5_gap580_task_stop_not_in_tool_definitions() {
        let defs = get_subagent_tool_definitions();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("function")?.get("name")?.as_str())
            .collect();

        assert!(
            !names.contains(&"task_stop"),
            "gap #580: task_stop tool is absent from OC tool definitions (CC has TaskStopTool)"
        );
        assert!(names.contains(&"task"), "task tool must be present");
        assert!(
            names.contains(&"agent_output"),
            "agent_output tool must be present"
        );
    }

    // ── Spec #527 §1 — agent_output edge cases ──

    /// Spec #527 §1 — querying an `agent_id` that was never registered returns
    /// `is_error = true` with a "not found" message.
    #[test]
    fn spec1_agent_output_unknown_id_returns_error() {
        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("agent_id".to_string(), json!("00000000-not-registered"));

        let (msg, is_err) = execute_agent_output_tool(&args);
        assert!(is_err, "unknown agent_id must be an error");
        assert!(
            msg.contains("not found"),
            "message must say not found, got: {msg}"
        );
    }

    /// Spec #527 §1 — `agent_output` for a finished agent returns the output text
    /// and `is_error = false`.
    #[test]
    fn spec1_agent_output_finished_agent_returns_result() {
        let id = BACKGROUND_AGENTS.register(AgentType::Explore, "search task");
        BACKGROUND_AGENTS.finish(&id, "Found 3 matches.".to_string());

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("agent_id".to_string(), json!(id));

        let (msg, is_err) = execute_agent_output_tool(&args);
        assert!(!is_err, "finished agent must not be an error: {msg}");
        assert!(
            msg.contains("Found 3 matches."),
            "must include result: {msg}"
        );
        assert!(msg.contains(&id));
    }

    /// Spec #527 §1 — `agent_output` for a failed agent returns `is_error = true`
    /// and the error text.
    #[test]
    fn spec1_agent_output_failed_agent_returns_error() {
        let id = BACKGROUND_AGENTS.register(AgentType::Plan, "failing task");
        BACKGROUND_AGENTS.fail(&id, "tool denied".to_string());

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("agent_id".to_string(), json!(id));

        let (msg, is_err) = execute_agent_output_tool(&args);
        assert!(is_err, "failed agent must return is_error=true");
        assert!(msg.contains("tool denied"), "must include error: {msg}");
    }

    // ── Crosslink #422: BACKGROUND_AGENTS unbounded-growth fix ──

    /// Crosslink #422 — `gc()` evicts agents whose `finished_at` is past
    /// the TTL. Backdating `finished_at` by `TTL + 1s` triggers eviction
    /// without sleeping in the test.
    #[test]
    fn issue422_gc_removes_finished_agents_past_ttl() {
        let mgr = BackgroundAgentManager::new();
        let stale_id = mgr.register(AgentType::Explore, "stale finished task");
        mgr.finish(&stale_id, "output".to_string());

        // Backdate the completion timestamp past the TTL.
        let stale = mgr.get(&stale_id).expect("registered");
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(FINISHED_AGENT_TTL_SECS + 1))
            .expect("clock supports subtraction by 1h+1s");
        *stale.finished_at.lock().unwrap() = Some(past);
        drop(stale);

        let removed = mgr.gc();
        assert_eq!(removed, 1, "exactly the stale finished agent must be GC'd");
        assert!(
            mgr.get(&stale_id).is_none(),
            "stale finished agent must no longer be in the map"
        );
    }

    /// Crosslink #422 — `gc()` must NOT remove agents that are still running,
    /// nor finished agents whose retention window has not yet expired.
    /// Guards against the obvious wrong-direction fix where the GC is too
    /// aggressive and drops live work.
    #[test]
    fn issue422_gc_keeps_in_progress_and_recently_finished_agents() {
        let mgr = BackgroundAgentManager::new();

        let running_id = mgr.register(AgentType::Plan, "still running");
        let recent_id = mgr.register(AgentType::Explore, "recently finished");
        mgr.finish(&recent_id, "fresh output".to_string());

        // Running agents have `finished_at = None`; the recent finish was
        // a few microseconds ago — both must survive a GC pass.
        let removed = mgr.gc();
        assert_eq!(
            removed, 0,
            "neither the running nor the recently-finished agent should be evicted"
        );
        assert!(
            mgr.get(&running_id).is_some(),
            "in-progress agent must never be GC'd"
        );
        assert!(
            mgr.get(&recent_id).is_some(),
            "agent within TTL must not be GC'd"
        );

        // Sanity: the in-progress agent has no completion timestamp.
        let running = mgr.get(&running_id).unwrap();
        assert!(running.finished_at.lock().unwrap().is_none());
    }

    /// Crosslink #422 — `agent_output` must surface the result/error to the
    /// caller *and then* drop the entry from the manager on the same call,
    /// so a session that polls `agent_output` for every spawned worker
    /// cannot accumulate finished `BackgroundAgent` Arcs.
    /// Covers both the success and failure paths.
    #[test]
    fn issue422_agent_output_returns_result_then_removes_finished_entry() {
        // Success path: result text is returned, then the entry vanishes.
        let ok_id = BACKGROUND_AGENTS.register(AgentType::Explore, "consume-on-read ok");
        BACKGROUND_AGENTS.finish(&ok_id, "the answer is 42".to_string());
        assert!(
            BACKGROUND_AGENTS.get(&ok_id).is_some(),
            "agent must exist before retrieval"
        );

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("agent_id".to_string(), json!(ok_id));
        let (msg, is_err) = execute_agent_output_tool(&args);
        assert!(!is_err, "finished agent must not be an error: {msg}");
        assert!(
            msg.contains("the answer is 42"),
            "result must be returned to caller BEFORE removal: {msg}"
        );
        assert!(
            BACKGROUND_AGENTS.get(&ok_id).is_none(),
            "finished agent must be removed from the manager after agent_output reads it"
        );

        // Failure path: error text is returned, then the entry vanishes.
        let err_id = BACKGROUND_AGENTS.register(AgentType::Plan, "consume-on-read fail");
        BACKGROUND_AGENTS.fail(&err_id, "synthetic failure".to_string());
        let mut args2: HashMap<String, Value> = HashMap::new();
        args2.insert("agent_id".to_string(), json!(err_id));
        let (msg2, is_err2) = execute_agent_output_tool(&args2);
        assert!(is_err2, "failed agent must return is_error=true");
        assert!(
            msg2.contains("synthetic failure"),
            "error text must be returned BEFORE removal: {msg2}"
        );
        assert!(
            BACKGROUND_AGENTS.get(&err_id).is_none(),
            "failed agent must be removed from the manager after agent_output reads it"
        );
    }

    /// Crosslink #422 — `cleanup_finished` is the explicit shutdown hook
    /// for callers like `tui.rs`: it drops every finished agent but
    /// preserves any still-running ones.
    #[test]
    fn issue422_cleanup_finished_drops_finished_keeps_running() {
        let mgr = BackgroundAgentManager::new();
        let done_a = mgr.register(AgentType::Explore, "done a");
        let done_b = mgr.register(AgentType::Plan, "done b");
        let live = mgr.register(AgentType::GeneralPurpose, "still working");

        mgr.finish(&done_a, "ok".to_string());
        mgr.fail(&done_b, "bad".to_string());

        let removed = mgr.cleanup_finished();
        assert_eq!(removed, 2, "both finished agents must be removed");
        assert!(mgr.get(&done_a).is_none());
        assert!(mgr.get(&done_b).is_none());
        assert!(
            mgr.get(&live).is_some(),
            "running agent must survive cleanup_finished"
        );
    }

    // ── Crosslink #719: runtime-flavor-aware sync dispatch ────────────────
    //
    // The sync branch of `execute_task_tool` used to call
    // `tokio::task::block_in_place(|| handle.block_on(...))` unconditionally
    // whenever a tokio `Handle` was in scope. `block_in_place` PANICS under
    // a `current_thread` runtime, so a Task tool call dispatched from a
    // `#[tokio::test]` (default flavor: current_thread) or from any
    // single-threaded CLI harness would crash the worker.
    //
    // The fix branches on `Handle::runtime_flavor()`:
    //   * MultiThread     → `block_in_place` + `block_on` (safe).
    //   * CurrentThread   → fail fast with a typed error message; no panic.
    //   * No runtime      → spin up a dedicated `Runtime::new()`.
    //
    // The tests below pin all three branches. They use a fake `resume` id
    // to make `run_subagent` return instantly with "No transcript found",
    // so the test never touches the network and never depends on a real
    // provider — we only care about which dispatch branch was taken.

    /// Build a minimal `AppConfig` suitable for exercising
    /// `execute_task_tool`. The Task tool only needs `proxy.target` and a
    /// matching provider entry; the tests never reach the network because
    /// they feed a bogus `resume` id that short-circuits `run_subagent`.
    fn issue719_app_config() -> AppConfig {
        use crate::config::ThinkingConfig;
        use crate::config::{
            GuardrailsConfig, HooksConfig, KeybindingsConfig, PermissionsConfig, ProviderConfig,
            ProxyConfig, SessionConfig, VddConfig,
        };

        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                base_url: "http://127.0.0.1:1".to_string(),
                api_key: Some(
                    crate::providers::ApiKey::try_from_string("test-key".to_string()).unwrap(),
                ),
                model: None,
                headers: HashMap::new(),
                thinking: ThinkingConfig::default(),
            },
        );
        AppConfig {
            proxy: ProxyConfig::default(),
            providers,
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
            vdd: VddConfig::default(),
            guardrails: GuardrailsConfig::default(),
            permissions: PermissionsConfig::default(),
            managed_settings_path: None,
        }
    }

    /// Build args that drive `execute_task_tool` through its sync branch
    /// (`run_in_background=false`) and through `run_subagent`'s resume
    /// fast-fail (`resume` set to an id guaranteed not to exist).
    fn issue719_args() -> HashMap<String, Value> {
        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("description".to_string(), json!("issue719 test"));
        args.insert("prompt".to_string(), json!("noop"));
        args.insert("subagent_type".to_string(), json!("general-purpose"));
        args.insert("run_in_background".to_string(), json!(false));
        // Unknown id → `run_subagent` returns instantly with
        // "No transcript found...". Keeps the test off the network.
        args.insert(
            "resume".to_string(),
            json!(format!("issue719-missing-{}", Uuid::new_v4())),
        );
        args
    }

    /// #719 — From a `current_thread` runtime the function must NOT panic
    /// (the old code's `block_in_place` would). It must return the typed
    /// "cannot `block_on` without deadlock" error with `is_error=true`.
    #[test]
    fn issue719_current_thread_runtime_returns_error_without_panicking() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current_thread runtime must build");

        let (msg, is_err) = rt.block_on(async {
            // `execute_task_tool` is a sync fn; calling it inside an async
            // block on a current_thread runtime is exactly the dispatch
            // shape that used to panic via `block_in_place`.
            let app_config = issue719_app_config();
            let args = issue719_args();
            execute_task_tool(&args, &app_config)
        });

        assert!(
            is_err,
            "current_thread dispatch must surface an error, not silently succeed: {msg}"
        );
        assert!(
            msg.contains("current_thread") && msg.contains("deadlock"),
            "current_thread branch must return the typed deadlock-guard message; got: {msg}"
        );
        assert!(
            !msg.contains("No transcript found"),
            "current_thread branch must short-circuit BEFORE run_subagent; got: {msg}"
        );
    }

    /// #719 — From a `multi_thread` runtime the function must dispatch
    /// through `block_in_place` + `block_on` and reach `run_subagent`.
    /// We verify by checking the output came from `run_subagent`'s resume
    /// fast-fail path, not from the deadlock-guard branch.
    #[test]
    fn issue719_multi_thread_runtime_dispatches_to_run_subagent() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi_thread runtime must build");

        let (msg, is_err) = rt.block_on(async {
            let app_config = issue719_app_config();
            let args = issue719_args();
            execute_task_tool(&args, &app_config)
        });

        assert!(
            is_err,
            "resume of unknown id must propagate as is_error=true: {msg}"
        );
        assert!(
            msg.contains("No transcript found"),
            "multi_thread branch must reach run_subagent (resume fast-fail); got: {msg}"
        );
        assert!(
            !msg.contains("cannot block_on without deadlock"),
            "multi_thread branch must NOT trigger the deadlock guard; got: {msg}"
        );
    }

    /// #719 — With no tokio runtime in scope, the function must build its
    /// own `Runtime::new()` and dispatch successfully. As with the
    /// `multi_thread` case we verify the output came from `run_subagent`.
    #[test]
    fn issue719_no_runtime_creates_runtime_and_dispatches() {
        // `execute_task_tool` is sync; calling it directly from the
        // `#[test]` thread means `Handle::try_current()` returns `Err`,
        // hitting the `Runtime::new()` fallback.
        let app_config = issue719_app_config();
        let args = issue719_args();
        let (msg, is_err) = execute_task_tool(&args, &app_config);

        assert!(
            is_err,
            "resume of unknown id must propagate as is_error=true: {msg}"
        );
        assert!(
            msg.contains("No transcript found"),
            "no-runtime branch must reach run_subagent (resume fast-fail); got: {msg}"
        );
        assert!(
            !msg.contains("Failed to create runtime"),
            "Runtime::new() must succeed inside a normal #[test] thread; got: {msg}"
        );
        assert!(
            !msg.contains("cannot block_on without deadlock"),
            "no-runtime branch must NOT trigger the deadlock guard; got: {msg}"
        );
    }

    // ── Crosslink #415: TRANSCRIPT_STORE bounded growth + bg sweep ──
    //
    // These tests exercise the private `TranscriptStore` struct
    // directly so the global `TRANSCRIPT_STORE` is not polluted by
    // test data and so each test gets a fresh, deterministic store.
    // The global is exercised indirectly by the existing spec2
    // round-trip tests above and by the no-double-spawn test below.

    /// #415 — A 51st insert must evict the oldest entry, leaving the
    /// store at exactly `MAX_STORED_TRANSCRIPTS` (= 50) and the very
    /// first insert gone.
    #[test]
    fn issue415_lru_cap_evicts_oldest_at_51st_insert() {
        let mut store = TranscriptStore::new();
        // Insert 51 distinct transcripts with strictly-ordered
        // `created_at` so eviction order is deterministic.
        let base = Instant::now();
        let mut first_id = String::new();
        for i in 0u64..51 {
            let id = format!("agent-{i:03}");
            if i == 0 {
                first_id = id.clone();
            }
            store.insert(
                id,
                StoredTranscript {
                    messages: vec![json!({"role": "user", "content": "x"})],
                    agent_type: AgentType::Explore,
                    // Stagger timestamps so #0 is unambiguously oldest.
                    created_at: base + Duration::from_micros(i),
                },
            );
        }

        assert_eq!(
            store.len(),
            MAX_STORED_TRANSCRIPTS,
            "store must be capped at MAX_STORED_TRANSCRIPTS after 51 inserts"
        );
        assert!(
            store.get(&first_id).is_none(),
            "first-inserted (oldest) transcript must be the one evicted"
        );
        // A representative middle-aged entry must still be present.
        assert!(
            store.get("agent-025").is_some(),
            "non-oldest entries must be retained"
        );
        // The most-recent entry must still be present.
        assert!(
            store.get("agent-050").is_some(),
            "newest entry must be retained"
        );
    }

    /// #415 — A transcript with 600 messages must be truncated to 500
    /// at store time. We exercise the public `store_transcript` path
    /// because it owns the truncation + warn behavior.
    #[test]
    fn issue415_per_transcript_message_cap_truncates_at_500() {
        let id = format!("issue415-cap-{}", Uuid::new_v4());
        let big: Vec<Value> = (0..600)
            .map(|i| json!({"role": "user", "content": format!("msg {i}")}))
            .collect();

        store_transcript(&id, big, AgentType::Plan);

        let loaded = load_transcript(&id).expect("just-stored transcript must be loadable");
        assert_eq!(
            loaded.0.len(),
            MAX_MESSAGES_PER_TRANSCRIPT,
            "transcript must be truncated to MAX_MESSAGES_PER_TRANSCRIPT (=500)"
        );
        // Truncation drops the OLDEST messages; the tail (newest) is
        // what's kept, so the last message must be the original 599th
        // and the first kept message must be the original 100th.
        assert_eq!(
            loaded.0[0]["content"].as_str(),
            Some("msg 100"),
            "truncation must drop the oldest 100 messages, keeping the tail"
        );
        assert_eq!(
            loaded.0[MAX_MESSAGES_PER_TRANSCRIPT - 1]["content"].as_str(),
            Some("msg 599"),
            "last message must be the most-recent input"
        );
    }

    /// #415 — Calling `sweep` against an empty store must be a no-op
    /// (no panic, no eviction). This is the safety invariant the
    /// background timer relies on: it ticks every 60s regardless of
    /// store state.
    #[test]
    fn issue415_sweep_on_empty_store_is_noop() {
        let mut store = TranscriptStore::new();
        let removed = store.sweep(Instant::now());
        assert_eq!(removed, 0, "empty-store sweep must remove nothing");
        assert_eq!(store.len(), 0, "empty-store sweep must leave store empty");
    }

    /// #415 — `sweep` must evict entries older than the TTL while
    /// retaining fresh ones. We simulate "old" entries by setting
    /// their `created_at` to `now - (TTL + 1s)`.
    #[test]
    fn issue415_sweep_evicts_expired_entries() {
        let mut store = TranscriptStore::new();
        let now = Instant::now();
        let past = now
            .checked_sub(Duration::from_secs(TRANSCRIPT_TTL_SECS + 1))
            .expect("clock must permit subtracting TTL+1s");

        // Two stale + one fresh.
        store.insert(
            "stale-a".to_string(),
            StoredTranscript {
                messages: vec![],
                agent_type: AgentType::Explore,
                created_at: past,
            },
        );
        store.insert(
            "stale-b".to_string(),
            StoredTranscript {
                messages: vec![],
                agent_type: AgentType::Plan,
                // Slightly older still; ordering breakup needs unique keys.
                created_at: past
                    .checked_sub(Duration::from_micros(1))
                    .expect("clock must permit TTL+1s+1us subtraction"),
            },
        );
        store.insert(
            "fresh".to_string(),
            StoredTranscript {
                messages: vec![],
                agent_type: AgentType::GeneralPurpose,
                created_at: now,
            },
        );

        let removed = store.sweep(now);
        assert_eq!(removed, 2, "exactly the two stale entries must be evicted");
        assert!(store.get("stale-a").is_none());
        assert!(store.get("stale-b").is_none());
        assert!(
            store.get("fresh").is_some(),
            "fresh entry must survive the sweep"
        );
        assert_eq!(store.len(), 1);
    }

    /// #415 — Calling `spawn_transcript_sweeper` twice from inside a
    /// tokio runtime must spawn exactly once. The second call returns
    /// `false` because the `Once` guard has already fired.
    #[tokio::test]
    async fn issue415_background_sweep_does_not_double_spawn() {
        // First call inside the runtime: may or may not spawn depending
        // on whether some earlier test in this binary already tripped
        // the global `Once`. Either way, the SECOND call from the
        // same runtime must return `false` because the `Once` has
        // fired by then.
        let _first = spawn_transcript_sweeper();
        let second = spawn_transcript_sweeper();
        assert!(
            !second,
            "spawn_transcript_sweeper must be idempotent: second call must not spawn"
        );

        // Repeating doesn't drift either.
        let third = spawn_transcript_sweeper();
        assert!(!third, "third call must also be a no-op");
    }
}
