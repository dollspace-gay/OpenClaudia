//! Session state types: token usage, turn metrics, plan mode.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};

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
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Accumulate usage from another `TokenUsage`
    pub const fn accumulate(&mut self, other: &Self) {
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

/// Plan mode state for the agent session.
///
/// # Security: TOCTOU-safe plan-file identity (crosslink #334)
///
/// `plan_realpath` is the **canonical** absolute path of the plan file,
/// computed **once** at plan-mode entry via [`PlanModeState::enter`]. All
/// subsequent allow-checks compare against this stored realpath -- the
/// path is never re-resolved against the current working directory or
/// filesystem state at check time, which closes the cwd-swap and
/// symlink-swap TOCTOU windows the previous implementation suffered from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanModeState {
    /// Whether plan mode is currently active
    pub active: bool,
    /// Path the user/agent originally requested for the plan file.
    /// Kept for display / editor invocation; **not** used for security
    /// comparisons -- use [`Self::plan_realpath`] for that.
    pub plan_file: PathBuf,
    /// Canonical absolute path of the plan file, resolved exactly once at
    /// plan-mode entry. Allow-checks for `write_file` compare the
    /// canonical target against this value. Must point to a regular file
    /// (not a symlink, directory, or special file).
    pub plan_realpath: PathBuf,
    /// Allowed prompts when exiting plan mode
    pub allowed_prompts: Vec<AllowedPrompt>,
}

/// Error returned when plan-mode entry fails to pin a safe plan-file
/// identity. Each variant carries the path that triggered the failure so
/// the REPL can surface an actionable error message.
#[derive(Debug, thiserror::Error)]
pub enum PlanModeEntryError {
    /// The plan file does not exist on disk.
    #[error("plan file does not exist: {path}")]
    PlanFileMissing {
        /// The path that was checked.
        path: PathBuf,
    },
    /// The plan file path resolves through a symlink.
    #[error("plan file path is a symlink (not allowed): {path}")]
    PlanFileIsSymlink {
        /// The path that resolved to a symlink.
        path: PathBuf,
    },
    /// The plan file is not a regular file (directory, FIFO, socket, etc).
    #[error("plan file is not a regular file: {path}")]
    PlanFileNotRegular {
        /// The path that pointed at a non-regular file.
        path: PathBuf,
    },
    /// The plan file could not be canonicalized.
    #[error("failed to canonicalize plan file {path}: {source}")]
    CanonicalizeFailed {
        /// The path that failed to canonicalize.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The plan file could not be opened for the FD-based identity check.
    #[error("failed to open plan file {path}: {source}")]
    OpenFailed {
        /// The path that failed to open.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl PlanModeState {
    /// Enter plan mode by pinning a TOCTOU-safe identity for `plan_file`.
    ///
    /// Performs symlink-metadata + `File::open` + FD-based metadata +
    /// canonicalize. Refuses on any failure -- the previous fallback to
    /// string-based path comparison after a `current_dir()` lookup is
    /// the exact bypass crosslink #334 closes.
    ///
    /// # Errors
    ///
    /// Returns [`PlanModeEntryError`] if any of the four steps fails.
    pub fn enter(plan_file: PathBuf) -> Result<Self, PlanModeEntryError> {
        let lmeta = match std::fs::symlink_metadata(&plan_file) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(PlanModeEntryError::PlanFileMissing { path: plan_file });
            }
            Err(e) => {
                return Err(PlanModeEntryError::OpenFailed {
                    path: plan_file,
                    source: e,
                });
            }
        };
        if lmeta.file_type().is_symlink() {
            return Err(PlanModeEntryError::PlanFileIsSymlink { path: plan_file });
        }

        let f = File::open(&plan_file).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                PlanModeEntryError::PlanFileMissing {
                    path: plan_file.clone(),
                }
            } else {
                PlanModeEntryError::OpenFailed {
                    path: plan_file.clone(),
                    source,
                }
            }
        })?;

        let fmeta = f
            .metadata()
            .map_err(|source| PlanModeEntryError::OpenFailed {
                path: plan_file.clone(),
                source,
            })?;
        if !fmeta.file_type().is_file() {
            return Err(PlanModeEntryError::PlanFileNotRegular { path: plan_file });
        }

        let plan_realpath = std::fs::canonicalize(&plan_file).map_err(|source| {
            PlanModeEntryError::CanonicalizeFailed {
                path: plan_file.clone(),
                source,
            }
        })?;

        drop(f);

        Ok(Self {
            active: true,
            plan_file,
            plan_realpath,
            allowed_prompts: Vec::new(),
        })
    }
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
///
/// `write_file` is special: it is allowed **only** when its `path`
/// argument resolves to the same canonical path as `plan_realpath`, the
/// pre-pinned realpath produced by [`PlanModeState::enter`].
///
/// # Security: TOCTOU-safe `write_file` gate (crosslink #334)
///
/// `plan_realpath` is assumed to already be canonical and is **never**
/// re-canonicalized here -- re-resolving would re-introduce the cwd-swap
/// race the entry-time pin closes. The target is validated with the same
/// FD-pinned pattern used at entry: `symlink_metadata` (reject symlinks)
/// then `File::open` (pin the inode) then FD-based `File::metadata`
/// (reject non-regular) then `canonicalize` (compare to `plan_realpath`).
/// Any failure is a hard refusal -- the old string-comparison and
/// `current_dir`-join fallbacks are removed.
#[must_use]
pub fn is_tool_allowed_in_plan_mode(
    tool_name: &str,
    plan_realpath: &Path,
    args: &serde_json::Value,
) -> bool {
    if PLAN_MODE_ALLOWED_TOOLS.contains(&tool_name) {
        return true;
    }

    if PLAN_MODE_BLOCKED_TOOLS.contains(&tool_name) {
        return false;
    }

    if tool_name == "write_file" {
        let Some(path_str) = args.get("path").and_then(|v| v.as_str()) else {
            return false;
        };
        let target = Path::new(path_str);

        let Ok(lmeta) = std::fs::symlink_metadata(target) else {
            return false;
        };
        if lmeta.file_type().is_symlink() {
            return false;
        }

        let Ok(f) = File::open(target) else {
            return false;
        };

        let Ok(fmeta) = f.metadata() else {
            return false;
        };
        if !fmeta.file_type().is_file() {
            return false;
        }

        let Ok(target_canonical) = std::fs::canonicalize(target) else {
            return false;
        };

        drop(f);

        return target_canonical == plan_realpath;
    }

    if tool_name == "enter_plan_mode" || tool_name == "exit_plan_mode" {
        return true;
    }

    false
}

/// Context to inject at session start based on mode
#[must_use]
pub fn get_session_context(session: &Session) -> String {
    match session.mode {
        SessionMode::Initializer => "## Session Context: Initializer Agent\n\
            \n\
            You are the first agent working on this task. Your responsibilities:\n\
            1. Understand the full scope of the work\n\
            2. Create a clear plan with actionable steps\n\
            3. Document key decisions and rationale\n\
            4. Set up any necessary project structure\n\
            5. Prepare detailed handoff notes for subsequent sessions\n\
            \n\
            Focus on establishing a solid foundation that future agents can build upon."
            .to_string(),
        SessionMode::Coding => {
            let mut context = "## Session Context: Coding Agent\n\
                \n\
                You are continuing work from a previous session. Your responsibilities:\n\
                1. Review the handoff notes from the previous session\n\
                2. Continue from where the last agent left off\n\
                3. Track your progress and decisions\n\
                4. Prepare handoff notes if you won't complete the task\n\
                \n"
            .to_string();

            if let Some(parent_id) = &session.parent_session_id {
                let _ = writeln!(context, "Previous session ID: {parent_id}");
            }

            context
        }
    }
}

#[cfg(test)]
mod plan_mode_tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Entry refuses when the plan file does not exist (#334).
    #[test]
    fn enter_refuses_nonexistent_plan_file() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("does_not_exist.md");
        let err = PlanModeState::enter(nonexistent.clone())
            .expect_err("must refuse non-existent plan file");
        assert!(
            matches!(err, PlanModeEntryError::PlanFileMissing { ref path } if path == &nonexistent),
            "expected PlanFileMissing, got {err:?}"
        );
    }

    /// Entry refuses when the plan-file path is a symlink (#334).
    #[cfg(unix)]
    #[test]
    fn enter_refuses_symlink_at_plan_file_path() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.md");
        std::fs::write(&target, "# real plan\n").unwrap();
        let link = dir.path().join("plan.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = PlanModeState::enter(link.clone()).expect_err("must refuse symlink as plan file");
        assert!(
            matches!(err, PlanModeEntryError::PlanFileIsSymlink { ref path } if path == &link),
            "expected PlanFileIsSymlink, got {err:?}"
        );
    }

    /// Entry refuses when the plan-file path points at a directory (#334).
    #[test]
    fn enter_refuses_directory_at_plan_file_path() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("plans");
        std::fs::create_dir(&subdir).unwrap();
        let err =
            PlanModeState::enter(subdir.clone()).expect_err("must refuse directory as plan file");
        match err {
            PlanModeEntryError::PlanFileNotRegular { path }
            | PlanModeEntryError::OpenFailed { path, .. } => {
                assert_eq!(path, subdir);
            }
            other => panic!("expected NotRegular or OpenFailed, got {other:?}"),
        }
    }

    /// `write_file` allow-check rejects a symlink target even when the
    /// link points at the canonical plan file (TOCTOU defence, #334).
    #[cfg(unix)]
    #[test]
    fn allow_check_rejects_symlink_target_even_pointing_at_plan_file() {
        let dir = TempDir::new().unwrap();
        let plan = dir.path().join("plan.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let state = PlanModeState::enter(plan.clone()).expect("enter must succeed");
        let evil_link = dir.path().join("evil_link.md");
        std::os::unix::fs::symlink(&plan, &evil_link).unwrap();
        let args = json!({ "path": evil_link.to_string_lossy() });
        assert!(
            !is_tool_allowed_in_plan_mode("write_file", &state.plan_realpath, &args),
            "symlink to plan file must NOT pass the allow-check (TOCTOU)"
        );
        let ok_args = json!({ "path": plan.to_string_lossy() });
        assert!(
            is_tool_allowed_in_plan_mode("write_file", &state.plan_realpath, &ok_args),
            "the real plan-file path must still be allowed after the fix"
        );
    }

    /// `write_file` allow-check refuses non-existent target paths
    /// (the documented #334 bypass): no string fallback.
    #[test]
    fn allow_check_refuses_nonexistent_target_no_string_fallback() {
        let dir = TempDir::new().unwrap();
        let plan = dir.path().join("plan.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let state = PlanModeState::enter(plan).expect("enter must succeed");
        let nonexistent = dir.path().join("ghost.md");
        let args = json!({ "path": nonexistent.to_string_lossy() });
        assert!(
            !is_tool_allowed_in_plan_mode("write_file", &state.plan_realpath, &args),
            "non-existent target must NOT silently pass (#334)"
        );
        let sibling_dir = TempDir::new().unwrap();
        let sibling_plan = sibling_dir.path().join("plan.md");
        std::fs::write(&sibling_plan, "# decoy\n").unwrap();
        let args2 = json!({ "path": sibling_plan.to_string_lossy() });
        assert!(
            !is_tool_allowed_in_plan_mode("write_file", &state.plan_realpath, &args2),
            "different file with same basename must NOT pass (#334)"
        );
    }

    /// `write_file` allow-check ignores the current working directory (#334).
    #[test]
    fn allow_check_relative_target_refused_when_not_resolvable() {
        let dir = TempDir::new().unwrap();
        let plan = dir.path().join("plan.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let state = PlanModeState::enter(plan).expect("enter must succeed");
        let args = json!({
            "path": "this_relative_path_does_not_exist_anywhere_334.md"
        });
        assert!(
            !is_tool_allowed_in_plan_mode("write_file", &state.plan_realpath, &args),
            "relative path that does not resolve must be refused without consulting cwd"
        );
    }

    /// Static allow- and block-lists preserved after the #334 refactor.
    #[test]
    fn allow_check_preserves_static_allow_and_block_lists() {
        let dir = TempDir::new().unwrap();
        let plan = dir.path().join("plan.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let state = PlanModeState::enter(plan).expect("enter must succeed");
        let no_args = json!({});
        for allowed in PLAN_MODE_ALLOWED_TOOLS {
            assert!(
                is_tool_allowed_in_plan_mode(allowed, &state.plan_realpath, &no_args),
                "{allowed} must remain in the allow-list after the #334 refactor"
            );
        }
        for blocked in PLAN_MODE_BLOCKED_TOOLS {
            assert!(
                !is_tool_allowed_in_plan_mode(blocked, &state.plan_realpath, &no_args),
                "{blocked} must remain in the block-list after the #334 refactor"
            );
        }
        assert!(is_tool_allowed_in_plan_mode(
            "enter_plan_mode",
            &state.plan_realpath,
            &no_args
        ));
        assert!(is_tool_allowed_in_plan_mode(
            "exit_plan_mode",
            &state.plan_realpath,
            &no_args
        ));
        assert!(!is_tool_allowed_in_plan_mode(
            "unknown_tool_xyz",
            &state.plan_realpath,
            &no_args
        ));
    }
}
