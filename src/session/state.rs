//! Session state types: token usage, turn metrics, plan mode.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::Session;
use super::SessionMode;

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

/// Plan mode state for the agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanModeState {
    /// Whether plan mode is currently active
    pub active: bool,
    /// Path to the plan file
    pub plan_file: std::path::PathBuf,
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
pub const PLAN_MODE_BLOCKED_TOOLS: &[&str] = &["bash", "edit_file", "kill_shell", "todo_write"];

/// Check if a tool is allowed in plan mode.
/// write_file is special: it's allowed only if targeting the plan file path.
pub fn is_tool_allowed_in_plan_mode(
    tool_name: &str,
    plan_file: &Path,
    args: &serde_json::Value,
) -> bool {
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
            // Try canonical comparison first (handles symlinks, relative paths)
            if let (Ok(tc), Ok(pc)) = (
                std::fs::canonicalize(target),
                std::fs::canonicalize(plan_file),
            ) {
                return tc == pc;
            }
            // Fallback: normalize both to absolute paths for comparison.
            // This handles the case where the file doesn't exist yet or
            // canonicalize fails for other reasons.
            let abs_target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(target))
                    .unwrap_or_else(|_| target.to_path_buf())
            };
            let abs_plan = if plan_file.is_absolute() {
                plan_file.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(plan_file))
                    .unwrap_or_else(|_| plan_file.to_path_buf())
            };
            return abs_target == abs_plan;
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
