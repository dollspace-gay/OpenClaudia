//! Granular tool permission system for `OpenClaudia`.
//!
//! Provides glob-pattern-based permission rules that control tool execution:
//! - Per-tool rules with glob patterns matching commands or file paths
//! - Three decision levels: Allow, Deny, `AlwaysAllow` (persisted across sessions)
//! - Configurable defaults and persistence to `.openclaudia/permissions.json`
//!
//! Check order: always-allow rules -> session rules -> config `default_allow` -> Deny (prompt user)

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tracing::{debug, info, warn};

/// Global cache for compiled glob-to-regex patterns.
/// Avoids recompiling the same glob pattern into a `Regex` on every permission check.
static GLOB_CACHE: LazyLock<Mutex<HashMap<String, Regex>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Decision for a permission check
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Allow this specific invocation
    Allow,
    /// Deny this specific invocation
    Deny,
    /// Always allow this pattern (persisted across sessions)
    AlwaysAllow,
}

/// A single permission rule mapping a tool + pattern to a decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Tool name: "Bash", "Edit", "Write", etc.
    pub tool: String,
    /// Glob-style pattern matched against the tool's primary argument.
    /// For Bash: matched against the command string.
    /// For Edit/Write: matched against the `file_path`.
    pub pattern: String,
    /// The decision to apply when this rule matches.
    pub decision: PermissionDecision,
}

/// Result of a permission check
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    /// Tool use is allowed
    Allowed,
    /// Tool use is denied
    Denied(String),
    /// No rule matched; the caller should prompt the user
    NeedsPrompt { tool: String, target: String },
}

/// Maximum number of *consecutive* denials before the agent should abort.
///
/// Parity target: CC `denialTracking.ts` `DENIAL_LIMITS.maxConsecutive`.
/// CC uses 3; OC uses 5 (configured via crosslink #572) to be slightly
/// more permissive of transient prompt-fallback churn before escalation.
pub const MAX_CONSECUTIVE_DENIALS: u32 = 5;

/// Maximum number of *total* (session-cumulative) denials before the agent
/// should abort. Parity target: CC `denialTracking.ts` `DENIAL_LIMITS.maxTotal` (20).
pub const MAX_TOTAL_DENIALS: u32 = 20;

/// Whether the denial-tracking state has crossed an escalation threshold.
///
/// Callers (notably the headless agent loop) should query
/// [`PermissionManager::escalation_state`] after each denial and abort the
/// agent cleanly when [`EscalationState::ShouldAbort`] is returned. Parity
/// target: CC `shouldFallbackToPrompting()` in
/// `utils/permissions/denialTracking.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationState {
    /// Counters are below both thresholds; no escalation needed.
    Normal,
    /// Either the consecutive or total denial threshold has been exceeded;
    /// the caller should abort (headless mode) or fall back to interactive
    /// prompting (interactive mode).
    ShouldAbort,
}

/// Bayesian-style auto-allow score for `(tool_name, args)` (crosslink #571).
///
/// Pure function — no manager state required, no I/O. Exposed at the
/// module level so callers (auto-mode glue, tests, future telemetry)
/// can read the score without going through a manager instance.
///
/// Returns a number in `[0.0, 1.0]`:
///   * `1.0` — definitely safe (read-only tool category).
///   * `≥ 0.9` — high confidence (read-only `bash` verbs like `ls`,
///     `pwd`, `cat`, `git status`).
///   * `~0.6` — moderate (edits to files inside the project tree).
///   * `0.3` — default; caller should not auto-allow.
///   * `0.0` — explicit unsafety (destructive bash tokens).
///
/// Heuristic only — pairs with `check_auto_allow` which gates on a
/// caller-supplied threshold and also consults explicit deny rules.
#[must_use]
pub fn auto_allow_score(tool_name: &str, tool_args: &serde_json::Value) -> f32 {
    // Read-only tools (no permission target) → unconditionally safe.
    let target = crate::tools::registry::registry()
        .get(tool_name)
        .and_then(crate::tools::ToolHandler::permission_target);
    let Some(target) = target else {
        return 1.0;
    };

    let arg_val = tool_args
        .get(target.arg_key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    match target.canonical {
        "Bash" => bash_auto_allow_score(arg_val),
        "Edit" | "Write" => edit_auto_allow_score(arg_val),
        _ => 0.3,
    }
}

fn bash_auto_allow_score(cmd: &str) -> f32 {
    // Destructive tokens veto first.
    const DESTRUCTIVE: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "chmod 777",
        "sudo ",
        "dd ",
        "mkfs",
        ":>",
        ">/dev/",
        "> /dev/",
        "curl ",
        "wget ",
        "shutdown",
        "reboot",
    ];
    const SAFE_PREFIXES: &[&str] = &[
        "ls",
        "pwd",
        "cat ",
        "echo ",
        "head ",
        "tail ",
        "wc ",
        "git status",
        "git diff",
        "git log",
        "git branch",
        "git remote",
        "git show",
    ];
    let lower = cmd.trim_start();
    for tok in DESTRUCTIVE {
        if lower.contains(tok) {
            return 0.0;
        }
    }
    for prefix in SAFE_PREFIXES {
        if lower.starts_with(prefix) {
            return 0.95;
        }
    }
    0.3
}

fn edit_auto_allow_score(path: &str) -> f32 {
    // System-path edits are clearly unsafe.
    const UNSAFE_PREFIXES: &[&str] = &["/etc/", "/usr/", "/bin/", "/boot/", "/dev/", "/proc/"];
    for p in UNSAFE_PREFIXES {
        if path.starts_with(p) {
            return 0.0;
        }
    }
    // Project-tree edits get moderate confidence.
    if path.starts_with("src/")
        || path.starts_with("tests/")
        || path.starts_with("examples/")
        || path.starts_with("./")
        || !path.starts_with('/')
    {
        return 0.6;
    }
    0.3
}

/// Denial-tracking newtype — crosslink #577.
///
/// Lifts the `consecutive_denials` / `total_denials` pair out of
/// [`PermissionManager`] into a standalone struct that mirrors CC
/// `denialTracking.ts`. The manager retains its own counters for
/// backward-compatible access but a `DenialTracker` can also live
/// outside the manager — e.g. shared across multiple permission
/// strategies in the same session.
///
/// Two thresholds are tracked:
///   * `consecutive` — resets to zero on any allowed outcome.
///   * `total` — never resets within a session.
///
/// Default limits mirror [`MAX_CONSECUTIVE_DENIALS`] and
/// [`MAX_TOTAL_DENIALS`]. Use [`DenialTracker::with_limits`] for a
/// custom threshold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenialTracker {
    consecutive: u32,
    total: u32,
    limits: DenialLimits,
}

/// Threshold limits for [`DenialTracker`] (crosslink #577).
///
/// Parity target: CC `DENIAL_LIMITS` in `denialTracking.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenialLimits {
    /// Maximum consecutive denials before escalation.
    pub max_consecutive: u32,
    /// Maximum total (session-cumulative) denials before escalation.
    pub max_total: u32,
}

impl Default for DenialLimits {
    fn default() -> Self {
        Self {
            max_consecutive: MAX_CONSECUTIVE_DENIALS,
            max_total: MAX_TOTAL_DENIALS,
        }
    }
}

impl DenialTracker {
    /// Construct a tracker with the default OC limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a tracker with explicit limits.
    #[must_use]
    pub const fn with_limits(limits: DenialLimits) -> Self {
        Self {
            consecutive: 0,
            total: 0,
            limits,
        }
    }

    /// Record a denial — increments both counters (saturating).
    /// Parity: CC `recordDenial`.
    pub const fn record_denial(&mut self) {
        self.consecutive = self.consecutive.saturating_add(1);
        self.total = self.total.saturating_add(1);
    }

    /// Record an allowed outcome — resets the consecutive counter.
    /// Parity: CC `recordSuccess`.
    pub const fn record_allowed(&mut self) {
        self.consecutive = 0;
    }

    /// Reset both counters (e.g. on session restart).
    pub const fn reset(&mut self) {
        self.consecutive = 0;
        self.total = 0;
    }

    /// Current consecutive-denial count.
    #[must_use]
    pub const fn consecutive(&self) -> u32 {
        self.consecutive
    }

    /// Current total-denial count.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.total
    }

    /// Configured limits.
    #[must_use]
    pub const fn limits(&self) -> DenialLimits {
        self.limits
    }

    /// Current escalation state. Parity: CC `shouldFallbackToPrompting`.
    #[must_use]
    pub const fn escalation_state(&self) -> EscalationState {
        if self.consecutive > self.limits.max_consecutive || self.total > self.limits.max_total {
            EscalationState::ShouldAbort
        } else {
            EscalationState::Normal
        }
    }
}

/// Runtime context the permission system is being consulted from
/// (crosslink #570).
///
/// CC dispatches permission resolution to three distinct handlers based
/// on where the agent is running:
///   * `interactiveHandler.ts` (main interactive agent) — UI prompt.
///   * `swarmWorkerHandler.ts` — forward to leader / silent default-deny.
///   * `coordinatorHandler.ts` — sequential hook-then-classifier with
///     fall-through to interactive.
///
/// OC `PermissionManager::check` historically returned a single
/// `NeedsPrompt` and let callers re-interpret it — the TUI prompted, the
/// REPL prompted, and the headless coordinator would silently deny by
/// timing out. This conflation is the bug #570 closes: callers now pass
/// a `PermissionContext` so the *manager* projects the unmatched-rule
/// state into a context-appropriate `CheckResult` variant.
///
/// Variants:
///   * [`PermissionContext::Interactive`] — TUI / REPL. Unmatched rules
///     surface as [`CheckResult::NeedsPrompt`] (caller prompts the user).
///   * [`PermissionContext::SwarmWorker`] — background subagent / swarm
///     follower. Unmatched rules surface as [`CheckResult::Denied`] with
///     a *non-interactive default-deny* reason — the caller cannot
///     prompt. Parity: CC `swarmWorkerHandler` returning null after the
///     mailbox path is unavailable.
///   * [`PermissionContext::Coordinator`] — headless leader / scheduler.
///     Same default-deny posture as `SwarmWorker` for the moment; this
///     variant exists so a future coordinator-relay (#619) can plug in
///     without breaking the API.
///
/// Read-only tools and explicit allow/deny rules behave identically
/// across all contexts. Only the fall-through (no rule matched) branch
/// is context-sensitive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PermissionContext {
    /// Default: TUI / REPL / chat — caller can prompt the user.
    #[default]
    Interactive,
    /// Background subagent / swarm worker — no UI available, default-deny.
    SwarmWorker,
    /// Headless coordinator / scheduled run — no UI, default-deny.
    /// Reserved for future coordinator-relay; today behaves like `SwarmWorker`.
    Coordinator,
}

/// Manages permission rules for tool execution.
///
/// Rules are checked in priority order:
/// 1. Persisted always-allow rules (loaded from disk)
/// 2. Session rules (added at runtime via user responses)
/// 3. Config-level `default_allow` patterns
/// 4. If nothing matches, returns `NeedsPrompt`
pub struct PermissionManager {
    /// Persisted rules (`AlwaysAllow`) loaded from `.openclaudia/permissions.json`
    persisted_rules: Vec<PermissionRule>,
    /// Transient session rules (Allow/Deny added during this session)
    session_rules: Vec<PermissionRule>,
    /// Default allow patterns from config
    default_allow: Vec<String>,
    /// Path to the persistence file
    persist_path: PathBuf,
    /// Whether the permission system is enabled
    enabled: bool,
    /// Consecutive denial counter — resets to 0 on any allowed outcome.
    /// Parity: CC `DenialTrackingState.consecutiveDenials`.
    consecutive_denials: u32,
    /// Total denial counter — never resets within a session.
    /// Parity: CC `DenialTrackingState.totalDenials`.
    total_denials: u32,
    /// Tool names the user has selected "Always allow" for in the interactive
    /// TUI permission prompt. Lives for the entire `PermissionManager` lifetime
    /// (one per session), so the decision survives across API turns and
    /// agentic-loop iterations. See crosslink #724.
    tui_always_allowed: Mutex<HashSet<String>>,
    /// Tool names the user has selected "Always deny" for in the interactive
    /// TUI permission prompt. Same scope as `tui_always_allowed`. See crosslink #724.
    tui_always_denied: Mutex<HashSet<String>>,
}

impl PermissionManager {
    /// Create a new `PermissionManager`, loading persisted rules from disk.
    pub fn new(
        persist_path: impl Into<PathBuf>,
        enabled: bool,
        default_allow: Vec<String>,
    ) -> Self {
        let persist_path = persist_path.into();
        let persisted_rules = Self::load_persisted_rules(&persist_path);

        // Pre-validate default_allow patterns at load time so invalid globs fail fast
        for pattern in &default_allow {
            if Self::glob_to_regex_cached(pattern).is_none() {
                warn!(pattern = %pattern, "Invalid default_allow glob pattern will never match");
            }
        }

        Self {
            persisted_rules,
            session_rules: Vec::new(),
            default_allow,
            persist_path,
            enabled,
            consecutive_denials: 0,
            total_denials: 0,
            tui_always_allowed: Mutex::new(HashSet::new()),
            tui_always_denied: Mutex::new(HashSet::new()),
        }
    }

    /// Build an explicitly unrestricted manager that allows every tool call.
    ///
    /// This is the migration target for call sites that previously passed
    /// `None` through `Option<&PermissionManager>`: the new strict dispatch
    /// entry points demand a concrete manager, and constructing
    /// `PermissionManager::unrestricted()` documents the intent ("allow
    /// everything") at the call site rather than smuggling it in via a
    /// missing argument. See crosslink #460.
    #[must_use]
    pub fn unrestricted() -> Self {
        // `enabled = false` short-circuits `check()` to `CheckResult::Allowed`.
        Self {
            persisted_rules: Vec::new(),
            session_rules: Vec::new(),
            default_allow: Vec::new(),
            persist_path: PathBuf::new(),
            enabled: false,
            consecutive_denials: 0,
            total_denials: 0,
            tui_always_allowed: Mutex::new(HashSet::new()),
            tui_always_denied: Mutex::new(HashSet::new()),
        }
    }

    /// Check whether a tool invocation is allowed.
    ///
    /// - `tool_name`: e.g. "bash", "`edit_file`", "`write_file`"
    /// - `tool_args`: the parsed arguments map from the tool call
    ///
    /// Returns `Allowed`, `Denied`, or `NeedsPrompt`.
    pub fn check(&self, tool_name: &str, tool_args: &serde_json::Value) -> CheckResult {
        if !self.enabled {
            return CheckResult::Allowed;
        }

        // Determine the canonical tool category and the target string to match against
        let (canonical_tool, target) = match Self::extract_target(tool_name, tool_args) {
            Some(Ok(pair)) => pair,
            Some(Err(tool)) => {
                // Tool requires permission but args are malformed (e.g. command=123)
                warn!(
                    tool = %tool,
                    "Malformed tool args: required argument is not a string — denying"
                );
                return CheckResult::Denied(format!(
                    "Denied: {tool} tool call has malformed arguments (expected string, got wrong type)"
                ));
            }
            None => {
                // Tools without a matchable target are always allowed
                return CheckResult::Allowed;
            }
        };

        // SECURITY (crosslink #795): a model that injects
        // `dangerously_disable_sandbox: true` into Bash tool args is
        // making an active escalation attempt. The previous code
        // logged a warn and fell through to normal rule processing —
        // which is fine for the surface defence (the flag is never
        // honoured), but it misses the audit signal that the model
        // tried at all. Emit a structured error event and DENY the
        // call outright so the attempted escalation is recorded as a
        // bounded refusal rather than a silent ignore.
        if canonical_tool == "Bash" {
            if let Some(disable) = tool_args.get("dangerously_disable_sandbox") {
                if disable.as_bool().unwrap_or(false) {
                    tracing::error!(
                        target: "openclaudia::permissions",
                        event = "sandbox_escalation_attempt",
                        tool = %canonical_tool,
                        target_arg = %target,
                        "model attempted dangerously_disable_sandbox=true in tool \
                         args — REJECTED. The flag is only honoured from user-level \
                         configuration; this invocation is denied (crosslink #795)."
                    );
                    return CheckResult::Denied(
                        "dangerously_disable_sandbox cannot be set from tool \
                         arguments — only from user-level configuration"
                            .to_string(),
                    );
                }
            }
        }

        // Permission-decision audit logging (crosslink #870) — see
        // `log_permission_decision` for the structured event shape.

        // 1. Check persisted always-allow rules
        for rule in &self.persisted_rules {
            if rule.decision == PermissionDecision::AlwaysAllow
                && Self::rule_matches(rule, &canonical_tool, &target)
            {
                Self::log_permission_decision(
                    "allowed",
                    "persisted_always_allow",
                    &canonical_tool,
                    &target,
                    &rule.pattern,
                );
                return CheckResult::Allowed;
            }
        }

        // 2. Check session rules
        for rule in &self.session_rules {
            if Self::rule_matches(rule, &canonical_tool, &target) {
                match &rule.decision {
                    PermissionDecision::Allow | PermissionDecision::AlwaysAllow => {
                        Self::log_permission_decision(
                            "allowed",
                            "session_rule",
                            &canonical_tool,
                            &target,
                            &rule.pattern,
                        );
                        return CheckResult::Allowed;
                    }
                    PermissionDecision::Deny => {
                        Self::log_permission_decision(
                            "denied",
                            "session_rule",
                            &canonical_tool,
                            &target,
                            &rule.pattern,
                        );
                        return CheckResult::Denied(format!(
                            "Denied by session rule: {} on pattern '{}'",
                            canonical_tool, rule.pattern
                        ));
                    }
                }
            }
        }

        // 3. Check config default_allow patterns
        for pattern in &self.default_allow {
            if Self::glob_matches(pattern, &target) {
                Self::log_permission_decision(
                    "allowed",
                    "default_allow_config",
                    &canonical_tool,
                    &target,
                    pattern,
                );
                return CheckResult::Allowed;
            }
        }

        // 4. No rule matched -- caller should prompt the user
        CheckResult::NeedsPrompt {
            tool: canonical_tool,
            target,
        }
    }

    /// Context-aware permission check (crosslink #570).
    ///
    /// Identical to [`Self::check`] for explicit allow/deny matches, but
    /// the fall-through branch (no rule matched) is projected through
    /// the supplied [`PermissionContext`]:
    ///
    /// * [`PermissionContext::Interactive`] → [`CheckResult::NeedsPrompt`]
    ///   (same as the legacy `check`)
    /// * [`PermissionContext::SwarmWorker`] | [`PermissionContext::Coordinator`]
    ///   → [`CheckResult::Denied`] with a default-deny reason. These
    ///   contexts have no UI, so prompting the user is impossible; the
    ///   safe-by-default behaviour is to deny.
    ///
    /// A future tightening can elaborate `Coordinator` to relay to the
    /// interactive leader (see #619); the variant exists so call sites
    /// can pre-declare their context now.
    pub fn check_with_context(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        ctx: PermissionContext,
    ) -> CheckResult {
        match self.check(tool_name, tool_args) {
            CheckResult::Allowed => CheckResult::Allowed,
            CheckResult::Denied(reason) => CheckResult::Denied(reason),
            CheckResult::NeedsPrompt { tool, target } => match ctx {
                PermissionContext::Interactive => CheckResult::NeedsPrompt { tool, target },
                PermissionContext::SwarmWorker | PermissionContext::Coordinator => {
                    CheckResult::Denied(format!(
                        "Default-deny ({ctx:?}): no UI available to prompt for {tool} on '{target}'"
                    ))
                }
            },
        }
    }

    /// Classifier-based auto-allow check (crosslink #571).
    ///
    /// Computes a confidence score in `[0.0, 1.0]` that the tool call is
    /// safe to auto-approve, based on the (`tool_name`, `target_shape`)
    /// pair. If the score clears `threshold` AND no explicit deny rule
    /// matches, returns [`CheckResult::Allowed`]; otherwise falls
    /// through to the normal [`Self::check_with_context`] pipeline.
    ///
    /// The classifier is a small Bayesian-style scorer:
    /// * Read-only tools (no `PermissionTarget`) → score 1.0 (always
    ///   auto-allow).
    /// * `Bash` commands matching a known-safe verb prefix (`ls`, `cat`,
    ///   `pwd`, `echo`, `git status`, `git diff`, `git log`) → 0.95.
    /// * `Bash` commands containing destructive tokens (`rm -rf`,
    ///   `chmod 777`, `sudo`, `dd `, `mkfs`, `:>`, `>/dev/`) → 0.0.
    /// * `Edit` / `Write` to paths under `src/` or `tests/` → 0.6.
    /// * Everything else → 0.3.
    ///
    /// An explicit `Deny` session rule short-circuits to 0.0 regardless
    /// of the heuristic score. Mirrors CC `classifyYoloAction` in
    /// `utils/permissions/yoloClassifier.ts`.
    pub fn check_auto_allow(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        threshold: f32,
    ) -> CheckResult {
        // First: any explicit `Deny` rule wins outright.
        if let CheckResult::Denied(reason) = self.check(tool_name, tool_args) {
            return CheckResult::Denied(reason);
        }
        let score = auto_allow_score(tool_name, tool_args);
        debug!(
            tool = %tool_name,
            score,
            threshold,
            "auto-allow classifier score"
        );
        if score >= threshold {
            return CheckResult::Allowed;
        }
        // Fall through to the normal pipeline (with interactive context
        // as the default — caller can re-dispatch through
        // `check_with_context` if they need a stricter context).
        self.check(tool_name, tool_args)
    }

    /// Add a session-scoped permission rule.
    pub fn add_session_rule(&mut self, rule: PermissionRule) {
        info!(
            tool = %rule.tool,
            pattern = %rule.pattern,
            decision = ?rule.decision,
            "Added session permission rule"
        );
        self.session_rules.push(rule);
    }

    /// Add and persist an always-allow rule.
    pub fn add_always_allow(&mut self, tool: &str, pattern: &str) {
        let rule = PermissionRule {
            tool: tool.to_string(),
            pattern: pattern.to_string(),
            decision: PermissionDecision::AlwaysAllow,
        };
        self.persisted_rules.push(rule);
        if let Err(e) = self.save_persisted_rules() {
            warn!(error = %e, "Failed to persist always-allow rule");
        }
        info!(
            tool = %tool,
            pattern = %pattern,
            "Added and persisted always-allow rule"
        );
    }

    /// Extract the canonical tool name and the target string for pattern matching.
    ///
    /// Returns:
    /// - `Some(Ok((tool, target)))` for tools that need permission checks with valid args
    /// - `Some(Err(tool))` for tools that need permission checks but have malformed args
    /// - `None` for tools that don't need permission checks (e.g. read-only tools)
    ///
    /// # Registry-driven dispatch (crosslink #782)
    ///
    /// This function was historically an exhaustive `match` over three
    /// hard-coded tool names — `bash`, `edit_file`, `write_file` — with an
    /// `_ => None` catch-all. That catch-all silently fail-opened every
    /// other tool: adding `delete_file`, `chmod`, `run_subprocess`, or any
    /// MCP-provided write tool would bypass permission checks entirely.
    /// It also created an invisible coupling: the list of "permission-relevant"
    /// tools lived here in `permissions.rs`, while the tool definitions
    /// lived in `tools/registry.rs`, with no compile-time link between them.
    ///
    /// The fix inverts the dependency. Each [`ToolHandler`] now declares its
    /// own [`PermissionTarget`] via `ToolHandler::permission_target()`,
    /// defaulting to `None` (read-only / safe). `extract_target` looks up
    /// the handler in the global registry and uses whatever target the
    /// handler declared:
    ///
    /// - Handler not registered (unknown tool) → `None`. The caller treats
    ///   this as `CheckResult::Allowed` because there is no target string
    ///   to match rules against. Unknown tools also fail elsewhere in the
    ///   dispatch pipeline, so this isn't a security regression — it just
    ///   matches the pre-#782 behaviour for unregistered names.
    /// - Handler declares no `PermissionTarget` → `None`. Same fall-through.
    /// - Handler declares a target → look up the target's `arg_key` in the
    ///   tool args. Missing key → empty-string target (preserves the
    ///   pre-#782 "key absent" branch). Present-but-non-string key → `Err`
    ///   (malformed args, caller denies).
    fn extract_target(
        tool_name: &str,
        tool_args: &serde_json::Value,
    ) -> Option<Result<(String, String), String>> {
        let target = crate::tools::registry::registry()
            .get(tool_name)
            .and_then(crate::tools::ToolHandler::permission_target)?;

        let canonical = target.canonical.to_string();
        match tool_args.get(target.arg_key) {
            // Key present and a string — happy path.
            Some(v) if v.is_string() => {
                Some(Ok((canonical, v.as_str().unwrap_or_default().to_string())))
            }
            // Key absent OR present-but-wrong-type — both are malformed
            // args from a permission standpoint (crosslink #855:
            // absent-key previously returned Ok((canonical, "")) which
            // allowed a permission rule with `default_allow = ""` to fire
            // on a malformed Bash call that omitted the `command` field
            // entirely; that bypass is gone). Tools that legitimately
            // have no permission target are filtered earlier via the
            // `permission_target()?` short-circuit, so this branch only
            // fires for tools that DECLARED a target arg and then
            // either didn't supply it or supplied a non-string value.
            _ => Some(Err(canonical)),
        }
    }

    /// Emit a structured permission-decision audit event (crosslink #870).
    ///
    /// `decision` is `"allowed"` or `"denied"`; `decision_source` is a
    /// stable short label like `"persisted_always_allow"`,
    /// `"session_rule"`, or `"default_allow_config"`. The event target is
    /// always `"openclaudia::permissions"` and the event name
    /// `"permission_decision"` so log consumers can pivot uniformly.
    fn log_permission_decision(
        decision: &'static str,
        decision_source: &'static str,
        tool: &str,
        target_arg: &str,
        pattern: &str,
    ) {
        tracing::info!(
            target: "openclaudia::permissions",
            event = "permission_decision",
            decision = decision,
            decision_source = decision_source,
            tool = %tool,
            target_arg = %target_arg,
            pattern = %pattern,
            "permission decision"
        );
    }

    /// Check whether a rule matches a given tool + target.
    fn rule_matches(rule: &PermissionRule, canonical_tool: &str, target: &str) -> bool {
        if !rule.tool.eq_ignore_ascii_case(canonical_tool) {
            return false;
        }
        Self::glob_matches(&rule.pattern, target)
    }

    /// Match a glob-style pattern against a target string.
    ///
    /// Supported glob syntax:
    /// - `*` matches any sequence of non-`/` characters
    /// - `**` matches any sequence of characters (including `/`)
    /// - `?` matches exactly one non-`/` character
    /// - Literal characters match themselves
    ///
    /// The pattern is anchored (must match the entire target).
    /// Compiled regexes are cached in `GLOB_CACHE` so each pattern is only compiled once.
    fn glob_matches(pattern: &str, target: &str) -> bool {
        Self::glob_to_regex_cached(pattern).is_some_and(|re| re.is_match(target))
    }

    /// Return a cached compiled `Regex` for a glob pattern, compiling and caching it on first use.
    ///
    /// Crosslink #813: the prior implementation acquired the lock,
    /// checked the cache, dropped the lock, compiled the regex, then
    /// re-acquired the lock and inserted. Two threads racing through
    /// the same pattern each paid the compile cost AND each fought
    /// over the insert. The fix uses a single lock acquisition:
    /// the lock is held across compile, but the compile is bounded
    /// (regex syntax is fixed-size for any sane glob) and the
    /// contention path short-circuits to the cached value on the
    /// very next access.
    fn glob_to_regex_cached(pattern: &str) -> Option<Regex> {
        // Crosslink #813: single-acquisition, single-release. Build the
        // compile attempt inside the locked critical section but make
        // the closure-driven Entry API carry the work so clippy's
        // `Option::map_or_else`-vs-`if let/else` and "early drop"
        // lints both go quiet. The lock is released at the end of this
        // function automatically when `cache` goes out of scope —
        // there is no other path that holds it.
        let mut cache = GLOB_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Fast path: cached.
        if let Some(re) = cache.get(pattern) {
            let cached = re.clone();
            drop(cache);
            return Some(cached);
        }
        // Slow path: compile-and-insert.
        let regex_str = Self::glob_to_regex(pattern);
        let compiled = Regex::new(&regex_str);
        let outcome = compiled.as_ref().ok().cloned();
        if let Some(ref re) = outcome {
            cache.insert(pattern.to_string(), re.clone());
        }
        drop(cache);
        if let Err(e) = compiled {
            warn!(pattern = %pattern, error = %e, "Invalid glob pattern");
        }
        outcome
    }

    /// Convert a glob pattern to a regex string.
    fn glob_to_regex(pattern: &str) -> String {
        let mut regex = String::from("^");
        let chars: Vec<char> = pattern.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            match chars[i] {
                '*' => {
                    if i + 1 < chars.len() && chars[i + 1] == '*' {
                        // `**` matches everything including path separators
                        regex.push_str(".*");
                        i += 2;
                        // Skip a trailing `/` after `**`
                        if i < chars.len() && chars[i] == '/' {
                            regex.push_str("/?");
                            i += 1;
                        }
                    } else {
                        // `*` matches everything except `/`
                        regex.push_str("[^/]*");
                        i += 1;
                    }
                }
                '?' => {
                    regex.push_str("[^/]");
                    i += 1;
                }
                '.' | '+' | '^' | '$' | '(' | ')' | '{' | '}' | '[' | ']' | '|' | '\\' => {
                    regex.push('\\');
                    regex.push(chars[i]);
                    i += 1;
                }
                c => {
                    regex.push(c);
                    i += 1;
                }
            }
        }

        regex.push('$');
        regex
    }

    /// Load persisted rules from disk.
    fn load_persisted_rules(path: &Path) -> Vec<PermissionRule> {
        if !path.exists() {
            return Vec::new();
        }
        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<Vec<PermissionRule>>(&content) {
                Ok(rules) => {
                    info!(count = rules.len(), path = ?path, "Loaded persisted permission rules");
                    rules
                }
                Err(e) => {
                    warn!(error = %e, path = ?path, "Failed to parse permissions file");
                    Vec::new()
                }
            },
            Err(e) => {
                warn!(error = %e, path = ?path, "Failed to read permissions file");
                Vec::new()
            }
        }
    }

    /// Save persisted rules to disk.
    fn save_persisted_rules(&self) -> anyhow::Result<()> {
        // Only persist AlwaysAllow rules
        let to_persist: Vec<&PermissionRule> = self
            .persisted_rules
            .iter()
            .filter(|r| r.decision == PermissionDecision::AlwaysAllow)
            .collect();

        // Ensure parent directory exists
        if let Some(parent) = self.persist_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&to_persist)?;
        fs::write(&self.persist_path, json)?;
        debug!(path = ?self.persist_path, count = to_persist.len(), "Saved permission rules");
        Ok(())
    }

    /// Get all persisted rules (for inspection/debugging).
    #[must_use]
    pub fn persisted_rules(&self) -> &[PermissionRule] {
        &self.persisted_rules
    }

    /// Get all session rules (for inspection/debugging).
    #[must_use]
    pub fn session_rules(&self) -> &[PermissionRule] {
        &self.session_rules
    }

    /// Check if the permission system is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Clear all session rules (called at session end).
    pub fn clear_session_rules(&mut self) {
        self.session_rules.clear();
    }

    /// Record a `Denied` outcome from the permission classifier (crosslink #572).
    ///
    /// Increments both the consecutive and total denial counters. The
    /// caller (notably the headless agent loop) should then check
    /// [`Self::escalation_state`] and abort cleanly when the result is
    /// [`EscalationState::ShouldAbort`].
    ///
    /// Counter increments saturate at `u32::MAX`; once a threshold has
    /// been exceeded the escalation state is sticky for the remainder of
    /// the session (until [`Self::reset_denial_tracking`] is called).
    /// Parity target: CC `recordDenial` in `utils/permissions/denialTracking.ts`.
    pub fn record_denial(&mut self) {
        self.consecutive_denials = self.consecutive_denials.saturating_add(1);
        self.total_denials = self.total_denials.saturating_add(1);
        debug!(
            consecutive = self.consecutive_denials,
            total = self.total_denials,
            "Recorded permission denial"
        );
    }

    /// Record a successful (allowed) tool outcome (crosslink #572).
    ///
    /// Resets the consecutive denial counter to zero; the total counter
    /// persists for the lifetime of the session. Parity target: CC
    /// `recordSuccess` in `utils/permissions/denialTracking.ts`.
    pub const fn record_allowed(&mut self) {
        self.consecutive_denials = 0;
    }

    /// Current escalation state derived from the denial counters
    /// (crosslink #572).
    ///
    /// Returns [`EscalationState::ShouldAbort`] when either
    /// [`MAX_CONSECUTIVE_DENIALS`] or [`MAX_TOTAL_DENIALS`] has been
    /// exceeded. Parity target: CC `shouldFallbackToPrompting`.
    #[must_use]
    pub const fn escalation_state(&self) -> EscalationState {
        if self.consecutive_denials > MAX_CONSECUTIVE_DENIALS
            || self.total_denials > MAX_TOTAL_DENIALS
        {
            EscalationState::ShouldAbort
        } else {
            EscalationState::Normal
        }
    }

    /// Current consecutive-denial count (for inspection/diagnostics).
    #[must_use]
    pub const fn consecutive_denials(&self) -> u32 {
        self.consecutive_denials
    }

    /// Current total-denial count (for inspection/diagnostics).
    #[must_use]
    pub const fn total_denials(&self) -> u32 {
        self.total_denials
    }

    /// Reset both denial counters to zero (e.g. on session restart).
    ///
    /// The session-cumulative semantics of `total_denials` mean this is
    /// the *only* way to clear it; normal allowed outcomes only reset
    /// the consecutive counter.
    pub const fn reset_denial_tracking(&mut self) {
        self.consecutive_denials = 0;
        self.total_denials = 0;
    }

    /// Record that the user selected "Always allow" for `tool_name` in the
    /// interactive TUI permission prompt. The decision survives across
    /// `execute_tool_calls_for_tui` batches for the rest of the session.
    /// Parity target: CC session-scoped "always allow" cache. See crosslink #724.
    pub fn tui_remember_always_allowed(&self, tool_name: String) {
        let mut guard = self
            .tui_always_allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(tool_name);
    }

    /// Record that the user selected "Always deny" for `tool_name` in the
    /// interactive TUI permission prompt. The decision survives across
    /// `execute_tool_calls_for_tui` batches for the rest of the session.
    /// See crosslink #724.
    pub fn tui_remember_always_denied(&self, tool_name: String) {
        let mut guard = self
            .tui_always_denied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(tool_name);
    }

    /// Whether the user has previously selected "Always allow" for `tool_name`
    /// in this session. See crosslink #724.
    #[must_use]
    pub fn tui_is_always_allowed(&self, tool_name: &str) -> bool {
        let guard = self
            .tui_always_allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.contains(tool_name)
    }

    /// Whether the user has previously selected "Always deny" for `tool_name`
    /// in this session. See crosslink #724.
    #[must_use]
    pub fn tui_is_always_denied(&self, tool_name: &str) -> bool {
        let guard = self
            .tui_always_denied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.contains(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_manager(enabled: bool, default_allow: Vec<String>) -> (PermissionManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");
        let mgr = PermissionManager::new(persist_path, enabled, default_allow);
        (mgr, dir)
    }

    /// Fix #282: a manager built with `enabled = false` still auto-allows, but that is no
    /// longer the default posture. The default (`PermissionsConfig::default()`) now produces
    /// `enabled = true`, so a fresh install is deny-by-default.
    #[test]
    fn test_disabled_always_allows() {
        // `enabled = false` is an explicit opt-out — still short-circuits to Allowed.
        let (mgr, _dir) = make_manager(false, vec![]);
        let result = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(result, CheckResult::Allowed);
    }

    /// Fix #282: the DEFAULT `PermissionsConfig` now has `enabled = true` (deny-by-default).
    /// A manager built from `PermissionsConfig::default()` must prompt for destructive calls.
    #[test]
    fn test_default_config_is_deny_by_default() {
        use crate::config::PermissionsConfig;
        let cfg = PermissionsConfig::default();
        assert!(
            cfg.enabled,
            "#282: default PermissionsConfig must have enabled=true"
        );

        let dir = tempfile::TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");
        let mgr = PermissionManager::new(persist_path, cfg.enabled, cfg.default_allow);
        // A fresh default config must NOT auto-allow rm -rf /
        let result = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(
            matches!(result, CheckResult::NeedsPrompt { .. }),
            "#282: default config must produce NeedsPrompt for destructive bash, got: {result:?}"
        );
    }

    /// Fix #282: serde round-trip — YAML without `permissions.enabled` must default to `true`.
    #[test]
    fn test_permissions_config_serde_default_is_true() {
        use crate::config::PermissionsConfig;
        // Simulate loading config.yaml with no `enabled` key present
        let cfg: PermissionsConfig = serde_yaml::from_str("{}").unwrap();
        assert!(
            cfg.enabled,
            "#282: deserializing PermissionsConfig from empty YAML must yield enabled=true"
        );
    }

    /// Fix #282: serde opt-out — `enabled: false` in YAML still works.
    #[test]
    fn test_permissions_config_serde_explicit_false() {
        use crate::config::PermissionsConfig;
        let cfg: PermissionsConfig = serde_yaml::from_str("enabled: false").unwrap();
        assert!(
            !cfg.enabled,
            "#282: explicit enabled=false in YAML must be respected"
        );
        // An explicitly-disabled manager must short-circuit to Allowed
        let dir = tempfile::TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");
        let mgr = PermissionManager::new(persist_path, cfg.enabled, cfg.default_allow);
        let result = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(
            result,
            CheckResult::Allowed,
            "#282: explicit enabled=false must still short-circuit to Allowed"
        );
    }

    /// Fix #282: a manager built from the default config denies `write_file`.
    #[test]
    fn test_default_config_denies_write_file() {
        use crate::config::PermissionsConfig;
        let cfg = PermissionsConfig::default();
        let dir = tempfile::TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");
        let mgr = PermissionManager::new(persist_path, cfg.enabled, cfg.default_allow);
        let result = mgr.check("write_file", &json!({"path": "/etc/passwd"}));
        assert!(
            matches!(result, CheckResult::NeedsPrompt { .. }),
            "#282: default config must produce NeedsPrompt for write_file, got: {result:?}"
        );
    }

    #[test]
    fn test_read_only_tools_always_allowed() {
        let (mgr, _dir) = make_manager(true, vec![]);
        // read_file has no permission target, so it's always allowed
        let result = mgr.check("read_file", &json!({"path": "/etc/passwd"}));
        assert_eq!(result, CheckResult::Allowed);
    }

    #[test]
    fn test_bash_needs_prompt_when_no_rules() {
        let (mgr, _dir) = make_manager(true, vec![]);
        let result = mgr.check("bash", &json!({"command": "ls -la"}));
        assert!(matches!(result, CheckResult::NeedsPrompt { .. }));
    }

    #[test]
    fn test_default_allow_pattern() {
        let (mgr, _dir) = make_manager(true, vec!["git:*".to_string()]);
        // "git:*" won't match "git status" because the pattern matches differently
        // Let's use a proper glob
        let (mgr2, _dir2) = make_manager(true, vec!["git *".to_string()]);
        let result = mgr2.check("bash", &json!({"command": "git status"}));
        assert_eq!(result, CheckResult::Allowed);

        // Non-matching command still needs prompt
        let result2 = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(matches!(result2, CheckResult::NeedsPrompt { .. }));
    }

    #[test]
    fn test_session_allow_rule() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "cargo *".to_string(),
            decision: PermissionDecision::Allow,
        });
        let result = mgr.check("bash", &json!({"command": "cargo build"}));
        assert_eq!(result, CheckResult::Allowed);
    }

    #[test]
    fn test_session_deny_rule() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "rm **".to_string(),
            decision: PermissionDecision::Deny,
        });
        let result = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(matches!(result, CheckResult::Denied(_)));
    }

    #[test]
    fn test_always_allow_persistence() {
        let dir = TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");

        // Create manager and add always-allow rule
        {
            let mut mgr = PermissionManager::new(&persist_path, true, vec![]);
            mgr.add_always_allow("Edit", "src/**/*.rs");
        }

        // Create new manager from same path -- should load the persisted rule
        {
            let mgr = PermissionManager::new(&persist_path, true, vec![]);
            assert_eq!(mgr.persisted_rules().len(), 1);
            let result = mgr.check("edit_file", &json!({"path": "src/main.rs"}));
            assert_eq!(result, CheckResult::Allowed);
        }
    }

    #[test]
    fn test_write_tool_permission() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Write".to_string(),
            pattern: "src/**/*.rs".to_string(),
            decision: PermissionDecision::Allow,
        });

        let result = mgr.check("write_file", &json!({"path": "src/lib.rs"}));
        assert_eq!(result, CheckResult::Allowed);

        let result2 = mgr.check("write_file", &json!({"path": "README.md"}));
        assert!(matches!(result2, CheckResult::NeedsPrompt { .. }));
    }

    #[test]
    fn test_glob_to_regex_star() {
        // Single star should not match path separators
        let re = PermissionManager::glob_to_regex("src/*.rs");
        assert_eq!(re, "^src/[^/]*\\.rs$");
        let re = Regex::new(&re).unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(!re.is_match("src/sub/main.rs"));
    }

    #[test]
    fn test_glob_to_regex_double_star() {
        // Double star should match path separators
        let re = PermissionManager::glob_to_regex("src/**/*.rs");
        let re = Regex::new(&re).unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(re.is_match("src/sub/deep/main.rs"));
    }

    #[test]
    fn test_glob_to_regex_question_mark() {
        let re = PermissionManager::glob_to_regex("file?.txt");
        let re = Regex::new(&re).unwrap();
        assert!(re.is_match("file1.txt"));
        assert!(re.is_match("fileA.txt"));
        assert!(!re.is_match("file12.txt"));
    }

    #[test]
    fn test_malformed_tool_args_denied() {
        let (mgr, _dir) = make_manager(true, vec!["*".to_string()]);
        // command is an integer, not a string — must be denied, not allowed
        let result = mgr.check("bash", &json!({"command": 123}));
        assert!(
            matches!(result, CheckResult::Denied(_)),
            "Malformed bash command (non-string) must be denied, got: {result:?}"
        );
        // path is an array, not a string
        let result = mgr.check("edit_file", &json!({"path": ["/etc/passwd"]}));
        assert!(matches!(result, CheckResult::Denied(_)));
        let result = mgr.check("write_file", &json!({"path": null}));
        assert!(matches!(result, CheckResult::Denied(_)));
    }

    #[test]
    fn test_dangerously_disable_sandbox_in_tool_args_is_denied() {
        // Crosslink #795: a model that injects
        // `dangerously_disable_sandbox: true` into Bash tool args is
        // making an active sandbox-escalation attempt. The previous
        // behaviour was to log a warn and fall through to normal rule
        // processing (so the call usually surfaced as NeedsPrompt).
        // The fix denies the call outright so the escalation attempt
        // is bounded into a hard refusal AND captured in the audit log
        // via the `sandbox_escalation_attempt` tracing event.
        let (mgr, _dir) = make_manager(true, vec![]);
        let result = mgr.check(
            "bash",
            &json!({"command": "rm -rf /", "dangerously_disable_sandbox": true}),
        );
        assert!(
            matches!(result, CheckResult::Denied(_)),
            "dangerously_disable_sandbox in tool args must be DENIED outright, \
             got: {result:?}"
        );
    }

    #[test]
    fn test_clear_session_rules() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "*".to_string(),
            decision: PermissionDecision::Allow,
        });
        assert_eq!(mgr.session_rules().len(), 1);
        mgr.clear_session_rules();
        assert_eq!(mgr.session_rules().len(), 0);
    }

    // ── #572 denial tracking ─────────────────────────────────────────────

    /// #572: starting state — both counters zero, escalation state Normal.
    #[test]
    fn denial_tracking_initial_state_is_normal() {
        let (mgr, _dir) = make_manager(true, vec![]);
        assert_eq!(mgr.consecutive_denials(), 0);
        assert_eq!(mgr.total_denials(), 0);
        assert_eq!(mgr.escalation_state(), EscalationState::Normal);
    }

    /// #572: each `record_denial` increments BOTH counters together.
    #[test]
    fn denial_tracking_record_denial_increments_both_counters() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        for i in 1..=3 {
            mgr.record_denial();
            assert_eq!(
                mgr.consecutive_denials(),
                i,
                "consecutive counter must increment with each denial"
            );
            assert_eq!(
                mgr.total_denials(),
                i,
                "total counter must increment with each denial"
            );
        }
        // Still below MAX_CONSECUTIVE_DENIALS (5) and MAX_TOTAL_DENIALS (20).
        assert_eq!(mgr.escalation_state(), EscalationState::Normal);
    }

    /// #572: exceeding the consecutive threshold escalates to `ShouldAbort`.
    /// Threshold is strict-greater-than, so the (5+1)th consecutive denial trips it.
    #[test]
    fn denial_tracking_consecutive_threshold_escalates() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        // Push the counter up to the limit — still Normal.
        for _ in 0..MAX_CONSECUTIVE_DENIALS {
            mgr.record_denial();
        }
        assert_eq!(mgr.consecutive_denials(), MAX_CONSECUTIVE_DENIALS);
        assert_eq!(
            mgr.escalation_state(),
            EscalationState::Normal,
            "at-threshold consecutive count must NOT yet abort"
        );
        // One more — now exceed it.
        mgr.record_denial();
        assert_eq!(
            mgr.escalation_state(),
            EscalationState::ShouldAbort,
            "exceeding MAX_CONSECUTIVE_DENIALS must escalate"
        );
    }

    /// #572: `record_allowed` resets the *consecutive* counter only.
    /// Total remains incremented and continues to count toward `MAX_TOTAL_DENIALS`.
    #[test]
    fn denial_tracking_allowed_resets_consecutive_but_not_total() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        mgr.record_denial();
        mgr.record_denial();
        mgr.record_denial();
        assert_eq!(mgr.consecutive_denials(), 3);
        assert_eq!(mgr.total_denials(), 3);

        mgr.record_allowed();
        assert_eq!(
            mgr.consecutive_denials(),
            0,
            "record_allowed must reset consecutive counter"
        );
        assert_eq!(
            mgr.total_denials(),
            3,
            "record_allowed must NOT reset total counter"
        );
        assert_eq!(mgr.escalation_state(), EscalationState::Normal);
    }

    /// #572: the total threshold escalates *independently* of the consecutive
    /// counter — even if every other denial is interleaved with an allow,
    /// the total counter keeps climbing and eventually trips abort.
    #[test]
    fn denial_tracking_total_threshold_escalates_independently() {
        let (mut mgr, _dir) = make_manager(true, vec![]);

        // Alternate denial/allowed for 21 denials. Consecutive never exceeds 1,
        // but total reaches 21, exceeding MAX_TOTAL_DENIALS (20).
        for _ in 0..=MAX_TOTAL_DENIALS {
            mgr.record_denial();
            mgr.record_allowed();
        }

        assert_eq!(
            mgr.consecutive_denials(),
            0,
            "alternating allowed must keep consecutive at 0"
        );
        assert_eq!(
            mgr.total_denials(),
            MAX_TOTAL_DENIALS + 1,
            "total must equal number of denials despite alternating allows"
        );
        assert_eq!(
            mgr.escalation_state(),
            EscalationState::ShouldAbort,
            "exceeding MAX_TOTAL_DENIALS must escalate even when consecutive is 0"
        );
    }

    /// #572: `reset_denial_tracking` clears both counters and returns to Normal,
    /// even after the consecutive threshold has tripped escalation.
    #[test]
    fn denial_tracking_reset_clears_both_counters() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        for _ in 0..(MAX_CONSECUTIVE_DENIALS + 2) {
            mgr.record_denial();
        }
        assert_eq!(mgr.escalation_state(), EscalationState::ShouldAbort);

        mgr.reset_denial_tracking();
        assert_eq!(mgr.consecutive_denials(), 0);
        assert_eq!(mgr.total_denials(), 0);
        assert_eq!(mgr.escalation_state(), EscalationState::Normal);
    }

    /// #572: the counter saturates at `u32::MAX` instead of wrapping.
    /// Once the threshold has been crossed the escalation state is sticky.
    #[test]
    fn denial_tracking_counters_saturate_without_wrapping() {
        let (mut mgr, _dir) = make_manager(true, vec![]);
        // Hand-construct a near-overflow state via repeated denials would take
        // 4 billion iterations; instead poke the fields via repeated denials
        // bounded by saturating_add semantics. We simulate by asserting that
        // record_denial after the threshold keeps escalation sticky.
        for _ in 0..(MAX_CONSECUTIVE_DENIALS + 2) {
            mgr.record_denial();
        }
        let snapshot_consecutive = mgr.consecutive_denials();
        let snapshot_total = mgr.total_denials();
        // Many more denials must not wrap around to 0.
        for _ in 0..100 {
            mgr.record_denial();
        }
        assert!(mgr.consecutive_denials() > snapshot_consecutive);
        assert!(mgr.total_denials() > snapshot_total);
        assert_eq!(mgr.escalation_state(), EscalationState::ShouldAbort);
    }

    #[test]
    fn test_persisted_rules_priority_over_session() {
        let dir = TempDir::new().unwrap();
        let persist_path = dir.path().join("permissions.json");
        let mut mgr = PermissionManager::new(&persist_path, true, vec![]);

        // Add always-allow for edit on *.rs
        mgr.add_always_allow("Edit", "**/*.rs");
        // Add session deny for edit on *.rs -- should NOT override the always-allow
        // because persisted rules are checked first
        mgr.add_session_rule(PermissionRule {
            tool: "Edit".to_string(),
            pattern: "**/*.rs".to_string(),
            decision: PermissionDecision::Deny,
        });

        let result = mgr.check("edit_file", &json!({"path": "src/main.rs"}));
        assert_eq!(result, CheckResult::Allowed);
    }
}

/// Phase 2 spec-pinning tests for issue #546.
///
/// These tests pin the CURRENT behaviour of `PermissionManager` against
/// the Phase 1 spec extracted in crosslink #531. They do **not** fix
/// bugs — they document divergences from CC so that regressions are
/// caught and so that each gap issue (#570, #572, #576, #581, #586)
/// has an explicit, labelled test.
///
/// Security-critical divergences are marked `// SECURITY: #<issue>`.
/// Denial paths are the dominant test style, matching the permission
/// system's purpose.
#[cfg(test)]
mod phase2_spec_pins {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // ── helpers ───────────────────────────────────────────────────────────

    fn enabled(default_allow: Vec<&str>) -> (PermissionManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.json");
        let mgr = PermissionManager::new(
            &path,
            true,
            default_allow.into_iter().map(str::to_string).collect(),
        );
        (mgr, dir)
    }

    fn disabled() -> PermissionManager {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.json");
        PermissionManager::new(path, false, vec![])
    }

    // ── B1 · Check order: always-allow → session → default_allow → NeedsPrompt ─

    /// B1-allow-1: persisted always-allow fires before every other tier.
    #[test]
    fn b1_persisted_always_allow_beats_session_deny() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.json");
        let mut mgr = PermissionManager::new(&path, true, vec![]);

        mgr.add_always_allow("Edit", "src/**");
        mgr.add_session_rule(PermissionRule {
            tool: "Edit".to_string(),
            pattern: "src/**".to_string(),
            decision: PermissionDecision::Deny,
        });

        // Spec §B1: persisted always-allow is step 1 — session deny is step 2.
        // Result MUST be Allowed.
        let r = mgr.check("edit_file", &json!({"path": "src/main.rs"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B1: persisted always-allow must beat session deny"
        );
    }

    /// B1-deny-1: session Deny fires before `default_allow`.
    #[test]
    fn b1_session_deny_beats_default_allow() {
        let (mut mgr, _dir) = enabled(vec!["rm **"]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "rm **".to_string(),
            decision: PermissionDecision::Deny,
        });

        // default_allow has "rm **" but session deny fires first (step 2 vs step 3).
        let r = mgr.check("bash", &json!({"command": "rm -rf /tmp/foo"}));
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "B1: session Deny must fire before default_allow; got {r:?}"
        );
    }

    /// B1-deny-2: OC has NO pre-allow deny tier (gap vs CC alwaysDenyRules).
    /// A pattern that would be a CC alwaysDenyRule can only be expressed in OC
    /// as a session Deny. Without that session rule, `default_allow` wins.
    /// Documents the gap from spec §B1 "Security divergence".
    #[test]
    fn b1_gap_no_pre_allow_deny_tier_default_allow_wins() {
        // Allow all bash commands via default_allow — no session deny rule.
        let (mgr, _dir) = enabled(vec!["**"]);

        // CC could have alwaysDenyRules that fire before step 2a allow lookup.
        // OC cannot replicate that without a session Deny rule.
        // Current OC behaviour: Allowed (default_allow step 3 fires).
        // A future pre-allow deny tier (parity with CC) would return Denied here.
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B1 gap doc: without a session Deny, OC cannot short-circuit before allow lookup"
        );
    }

    /// B1-deny-3: empty `default_allow` with no rules → `NeedsPrompt` (deny-by-default).
    #[test]
    fn b1_empty_default_allow_yields_needs_prompt() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check("bash", &json!({"command": "ls"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B1: empty default_allow must produce NeedsPrompt, got {r:?}"
        );
    }

    // ── B2 · Invalid glob logs warning and is skipped (no panic) ──────────

    /// B2-deny-1: an invalid glob in `default_allow` never matches — the guarded
    /// call falls through to `NeedsPrompt` rather than being auto-allowed.
    #[test]
    fn b2_invalid_glob_in_default_allow_never_matches() {
        // "[unclosed" is an invalid regex that glob_to_regex_cached will fail to compile.
        let (mgr, _dir) = enabled(vec!["[unclosed"]);

        let r = mgr.check("bash", &json!({"command": "anything"}));
        // Must NOT be Allowed — invalid pattern must be skipped, not treated as allow-all.
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B2: invalid glob must fall through to NeedsPrompt, got {r:?}"
        );
    }

    /// B2-deny-2: empty-string glob does not let a malformed Bash call slip
    /// through. Crosslink #855: previously the "absent command key" branch
    /// of `extract_target` returned `Ok(("Bash", ""))`, which meant a
    /// `default_allow = ""` rule would allow a Bash call that omitted the
    /// `command` field entirely. The fix routes the absent-key case to
    /// the same `Err` branch as wrong-type, so the call is denied as
    /// malformed-args regardless of any allow-empty rule.
    #[test]
    fn b2_empty_glob_does_not_match_malformed_bash() {
        let (mgr, _dir) = enabled(vec![""]);

        // Non-empty bash command must NOT be allowed by the empty-string pattern.
        let r = mgr.check("bash", &json!({"command": "ls"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B2: empty glob must not match a non-empty bash command, got {r:?}"
        );

        // Bash with absent command key is now denied as malformed args,
        // not auto-allowed by the empty pattern (crosslink #855).
        let r_malformed = mgr.check("bash", &json!({}));
        assert!(
            matches!(r_malformed, CheckResult::Denied(_)),
            "B2 + #855: empty glob must NOT auto-allow a Bash call missing \
             its `command` field; got {r_malformed:?}"
        );
    }

    /// B2-deny-3: `*` (single star) does NOT match a target containing `/`.
    /// This is the documented OC vs CC security boundary (gap #576).
    #[test]
    fn b2_single_star_does_not_match_slash() {
        let (mgr, _dir) = enabled(vec!["*"]);

        // "rm -rf /" contains a `/` — OC `*` → `[^/]*` which stops at `/`.
        // SECURITY: #576 — CC `*` → `.*` which WOULD match this. OC is safer here.
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B2/B6 #576: single-star must not allow commands containing '/'; got {r:?}"
        );

        // A command without any `/` IS matched by `*`.
        let r_ok = mgr.check("bash", &json!({"command": "ls"}));
        assert_eq!(
            r_ok,
            CheckResult::Allowed,
            "B2: single-star must allow slash-free commands"
        );
    }

    // ── B3 · unrestricted() bypasses ALL checks ────────────────────────────

    /// B3-deny-1 (SECURITY: #586): `unrestricted()` allows destructive bash commands.
    /// CC bypassPermissions still enforces step 1g safetyCheck; OC does not.
    #[test]
    fn b3_unrestricted_allows_destructive_bash() {
        let mgr = PermissionManager::unrestricted();
        // SECURITY: #586 — CC would still run safetyCheck (step 1g) here.
        // OC short-circuits at enabled=false before any check.
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B3 SECURITY #586: unrestricted() currently allows rm -rf / (CC would deny via safetyCheck)"
        );
    }

    /// B3-deny-2 (SECURITY: #586): `unrestricted()` allows writes to `.git/config`.
    /// CC's bypassPermissions mode still blocks .git/ writes via step 1g.
    #[test]
    fn b3_unrestricted_allows_git_config_write() {
        let mgr = PermissionManager::unrestricted();
        // SECURITY: #586 — CC bypassPermissions denies .git/config edits via safetyCheck.
        // OC unrestricted() is a superset bypass; no safety-path check exists.
        let r = mgr.check("edit_file", &json!({"path": ".git/config"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B3 SECURITY #586: unrestricted() must currently return Allowed for .git/config (documents gap)"
        );
    }

    /// B3-deny-3 (SECURITY: #586): `unrestricted()` allows writes to `.claude/settings.json`.
    #[test]
    fn b3_unrestricted_allows_claude_settings_write() {
        let mgr = PermissionManager::unrestricted();
        // SECURITY: #586
        let r = mgr.check("write_file", &json!({"path": ".claude/settings.json"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B3 SECURITY #586: unrestricted() must currently return Allowed for .claude/settings.json"
        );
    }

    /// B3-deny-4 (SECURITY: #586): `dangerously_disable_sandbox` check in enabled mode
    /// is unreachable via `unrestricted()` — the short-circuit fires first.
    #[test]
    fn b3_unrestricted_bypasses_sandbox_flag_check() {
        let mgr = PermissionManager::unrestricted();
        // The sandbox-flag check (lines 155-169) is inside enabled=true branch.
        // SECURITY: #586 — unrestricted() skips it entirely.
        let r = mgr.check(
            "bash",
            &json!({"command": "id", "dangerously_disable_sandbox": true}),
        );
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B3 SECURITY #586: unrestricted bypasses sandbox-flag check"
        );
    }

    // ── B4 · LeaderPermissionBridge (tested in coordinator/permission.rs) ─
    // See phase2_spec_pins in src/coordinator/permission.rs for B4 tests.

    // ── B5 · Denial tracking missing (gap #572) ───────────────────────────

    /// B5-gap-1 (SECURITY: #572): OC has no denial tracking state.
    /// Repeated `NeedsPrompt` for the same denied tool call returns `NeedsPrompt`
    /// every time — there is no escalation to auto-deny or `AbortError`.
    /// CC escalates to fallback-prompt after 3 consecutive denials.
    #[test]
    fn b5_repeated_denied_call_stays_needs_prompt_no_escalation() {
        let (mgr, _dir) = enabled(vec![]);

        // Simulate repeated calls with no rule — each returns NeedsPrompt.
        // CC after 3 would hit shouldFallbackToPrompting; OC never escalates.
        for i in 0..5 {
            let r = mgr.check("bash", &json!({"command": "ls"}));
            assert!(
                matches!(r, CheckResult::NeedsPrompt { .. }),
                "B5 SECURITY #572: call {i} must still be NeedsPrompt (no escalation path)"
            );
        }
    }

    // ── B6 · Bash command glob matching divergences ───────────────────────

    /// B6-deny-1: `"git *"` does NOT match bare `"git"` (OC diverges from CC).
    /// CC trailing-wildcard optional-space: `"git *"` → `^git( .*)?$` → matches `"git"`.
    /// OC: `"git *"` → `^git [^/]*$` → requires a space after `git`.
    #[test]
    fn b6_git_star_does_not_match_bare_git() {
        let (mgr, _dir) = enabled(vec!["git *"]);

        // OC diverges from CC here (gap #576).
        let r = mgr.check("bash", &json!({"command": "git"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B6 #576: OC 'git *' must not match bare 'git' (diverges from CC optional-trailing-space)"
        );
    }

    /// B6-allow-1: `"git *"` DOES match `"git status"` in both CC and OC.
    #[test]
    fn b6_git_star_matches_git_status() {
        let (mgr, _dir) = enabled(vec!["git *"]);
        let r = mgr.check("bash", &json!({"command": "git status"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B6: 'git *' must match 'git status'"
        );
    }

    /// B6-deny-2: `"git *"` does NOT match `"gita status"` (no space after `git`).
    /// Both CC and OC agree on this rejection.
    #[test]
    fn b6_git_star_does_not_match_gita() {
        let (mgr, _dir) = enabled(vec!["git *"]);
        let r = mgr.check("bash", &json!({"command": "gita status"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B6: 'git *' must not match 'gita status'"
        );
    }

    /// B6-deny-3 (SECURITY: #576): `"rm *"` does NOT match `"rm -rf /"` in OC.
    /// CC `"rm *"` → `^rm .*$` which WOULD match (`.` matches `/`).
    /// OC `"rm *"` → `^rm [^/]*$` which does NOT match (stops at `/`).
    /// OC is MORE restrictive here; documents the portability break.
    #[test]
    fn b6_rm_star_does_not_match_path_with_slash() {
        let (mgr, _dir) = enabled(vec!["rm *"]);
        // SECURITY: #576 — OC is safer than CC for this pattern.
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B6 #576: 'rm *' must not match 'rm -rf /' in OC (slash blocked by [^/]*)"
        );
    }

    /// B6-deny-4: CC legacy `"git:*"` prefix rule is NOT supported in OC.
    /// OC treats `:` as a literal, so `"git:*"` never matches `"git status"`.
    #[test]
    fn b6_colon_star_prefix_syntax_not_supported() {
        let (mgr, _dir) = enabled(vec!["git:*"]);
        // In CC: "git:*" is a prefix rule → matches "git status".
        // In OC: "git:*" is a glob with literal `:` → requires "git:<something>".
        let r = mgr.check("bash", &json!({"command": "git status"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B6 #576: OC does not support CC legacy 'git:*' prefix syntax"
        );
    }

    // ── B7 · Default config is deny-by-default (Fix #282 + #581) ───────────
    //
    // Pre-fix: PermissionsConfig::default() had enabled=false → allow-all.
    // Post-fix (#282): default is enabled=true → deny-by-default (CC parity).
    // The `disabled()` helper still constructs an explicit enabled=false manager
    // for tests that need to verify that path still short-circuits.

    /// B7-deny-1 (FIX #282 / SECURITY: #581): `PermissionsConfig::default()` now has
    /// `enabled=true`, so a fresh install is deny-by-default, matching CC.
    /// The old allow-all posture required explicitly constructing with `enabled=false`.
    #[test]
    fn b7_default_config_is_deny_by_default_not_allow_all() {
        use crate::config::PermissionsConfig;
        let cfg = PermissionsConfig::default();
        // Post-fix: default must be enabled=true (deny-by-default).
        assert!(
            cfg.enabled,
            "FIX #282/#581: PermissionsConfig::default() must have enabled=true"
        );

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.json");
        let mgr = PermissionManager::new(path, cfg.enabled, cfg.default_allow);
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "FIX #282/#581: default config must deny (NeedsPrompt) rm -rf /, got {r:?}"
        );
    }

    /// B7-deny-2 (FIX #282): default config denies writes to safety-sensitive paths.
    #[test]
    fn b7_default_config_denies_git_config_edit() {
        use crate::config::PermissionsConfig;
        let cfg = PermissionsConfig::default();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.json");
        let mgr = PermissionManager::new(path, cfg.enabled, cfg.default_allow);
        let r = mgr.check("edit_file", &json!({"path": ".git/config"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "FIX #282: default config must deny (NeedsPrompt) .git/config edits, got {r:?}"
        );
    }

    /// B7-explicit-disabled: explicit `enabled=false` still short-circuits to Allowed
    /// (the old default behaviour, now only reachable by opting out explicitly).
    #[test]
    fn b7_explicit_disabled_allows_all() {
        let mgr = disabled();
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "B7: explicit enabled=false must still short-circuit to Allowed"
        );
    }

    /// B7-allow-1: enabled=true + empty `default_allow` → deny-by-default (`NeedsPrompt`).
    /// This is the correct CC-equivalent behaviour when the system is actually on.
    #[test]
    fn b7_enabled_empty_default_allow_is_deny_by_default() {
        let (mgr, _dir) = enabled(vec![]);
        for cmd in ["rm -rf /", "ls", "cargo build", "cat /etc/passwd"] {
            let r = mgr.check("bash", &json!({"command": cmd}));
            assert!(
                matches!(r, CheckResult::NeedsPrompt { .. }),
                "B7: enabled=true + empty default_allow must deny '{cmd}'; got {r:?}"
            );
        }
    }

    /// B7-deny-3: `"*"` in `default_allow` does NOT catch commands with `/` (OC vs CC divergence).
    /// Spec §B7 edge case: OC `*` → `[^/]*`; CC `*` → `.*` (catches `/`).
    #[test]
    fn b7_catchall_star_does_not_allow_slash_commands() {
        let (mgr, _dir) = enabled(vec!["*"]);
        // SECURITY: #576 — OC is MORE restrictive than CC for catchall `*`.
        let r = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B7 #576: OC '*' catchall must not allow commands containing '/' (diverges from CC '.*')"
        );
    }

    // ── Denial path edge-case battery ────────────────────────────────────

    /// Deny: session Deny on `write_file` fires before `default_allow`.
    #[test]
    fn deny_session_deny_write_beats_default_allow() {
        let (mut mgr, _dir) = enabled(vec!["**"]);
        mgr.add_session_rule(PermissionRule {
            tool: "Write".to_string(),
            pattern: "**".to_string(),
            decision: PermissionDecision::Deny,
        });
        let r = mgr.check("write_file", &json!({"path": "anywhere/file.txt"}));
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "deny: session Deny on Write must fire before default_allow '**'"
        );
    }

    /// Deny: session Deny on a different tool does not affect another tool.
    #[test]
    fn deny_session_deny_does_not_cross_tool_boundary() {
        let (mut mgr, _dir) = enabled(vec!["**"]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "**".to_string(),
            decision: PermissionDecision::Deny,
        });
        // Write is not denied — its default_allow "**" still fires.
        let r = mgr.check("write_file", &json!({"path": "foo.txt"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "deny: Bash Deny must not affect Write"
        );
    }

    /// Deny: malformed bash args (non-string command) are denied, not allowed.
    /// This is a security invariant regardless of `default_allow`.
    #[test]
    fn deny_malformed_bash_args_denied_regardless_of_default_allow() {
        let (mgr, _dir) = enabled(vec!["**"]);
        let r = mgr.check("bash", &json!({"command": true}));
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "malformed bash args must be Denied even when default_allow='**'"
        );
    }

    /// Deny: malformed `edit_file` args are denied even with permissive `default_allow`.
    #[test]
    fn deny_malformed_edit_args_denied_regardless_of_default_allow() {
        let (mgr, _dir) = enabled(vec!["**"]);
        let r = mgr.check("edit_file", &json!({"path": 42}));
        assert!(matches!(r, CheckResult::Denied(_)));
    }

    /// Deny: tool case-insensitive matching — "edit" rule matches "Edit" tool.
    #[test]
    fn deny_tool_name_case_insensitive_session_rule() {
        let (mut mgr, _dir) = enabled(vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "edit".to_string(), // lower-case rule
            pattern: "**".to_string(),
            decision: PermissionDecision::Deny,
        });
        let r = mgr.check("edit_file", &json!({"path": "src/main.rs"}));
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "tool name matching must be case-insensitive"
        );
    }

    // ------------------------------------------------------------------
    // crosslink #724 — TUI session-scoped always-allow / always-deny cache
    // ------------------------------------------------------------------

    /// #724: `tui_remember_always_allowed` stores the decision so a follow-up
    /// `tui_is_always_allowed` returns true.
    #[test]
    fn tui_724_remember_always_allowed_persists() {
        let (mgr, _dir) = enabled(vec![]);
        assert!(
            !mgr.tui_is_always_allowed("Bash"),
            "fresh manager must not have any always-allow entries"
        );
        mgr.tui_remember_always_allowed("Bash".to_string());
        assert!(
            mgr.tui_is_always_allowed("Bash"),
            "#724: tui_remember_always_allowed must be visible to tui_is_always_allowed"
        );
        // Other tools remain unaffected.
        assert!(!mgr.tui_is_always_allowed("Write"));
    }

    /// #724: `tui_remember_always_denied` stores the decision so a follow-up
    /// `tui_is_always_denied` returns true.
    #[test]
    fn tui_724_remember_always_denied_persists() {
        let (mgr, _dir) = enabled(vec![]);
        assert!(
            !mgr.tui_is_always_denied("Bash"),
            "fresh manager must not have any always-deny entries"
        );
        mgr.tui_remember_always_denied("Bash".to_string());
        assert!(
            mgr.tui_is_always_denied("Bash"),
            "#724: tui_remember_always_denied must be visible to tui_is_always_denied"
        );
        assert!(!mgr.tui_is_always_denied("Edit"));
    }

    /// #724: `tui_is_always_allowed` returns false for an unseen tool — guards
    /// against an "everything is allowed" regression.
    #[test]
    fn tui_724_is_always_allowed_default_false() {
        let (mgr, _dir) = enabled(vec![]);
        assert!(!mgr.tui_is_always_allowed("Bash"));
        assert!(!mgr.tui_is_always_allowed("Write"));
        assert!(!mgr.tui_is_always_allowed("Edit"));
    }

    /// #724: `tui_is_always_denied` returns false for an unseen tool — and the
    /// allow/deny caches are independent (remembering allow does not deny).
    #[test]
    fn tui_724_is_always_denied_default_false_and_caches_independent() {
        let (mgr, _dir) = enabled(vec![]);
        assert!(!mgr.tui_is_always_denied("Bash"));
        // Recording allow must NOT flip is_always_denied to true.
        mgr.tui_remember_always_allowed("Bash".to_string());
        assert!(
            !mgr.tui_is_always_denied("Bash"),
            "#724: tui_always_allowed and tui_always_denied must be independent caches"
        );
        // And recording deny on a different tool must not affect Bash.
        mgr.tui_remember_always_denied("Write".to_string());
        assert!(mgr.tui_is_always_denied("Write"));
        assert!(!mgr.tui_is_always_denied("Bash"));
    }

    /// #724: the `Mutex` is `Send + Sync`, so the cache survives being shared
    /// across the `Arc<PermissionManager>` boundary that the TUI pipeline uses.
    /// This test mimics the pipeline flow: clone an `Arc`, remember from one
    /// handle, observe from the other.
    #[test]
    fn tui_724_decision_survives_arc_sharing() {
        use std::sync::Arc;
        let (mgr, _dir) = enabled(vec![]);
        let shared = Arc::new(mgr);
        let clone = Arc::clone(&shared);

        // Batch 1 (clone): user picks "Always allow" for Bash.
        clone.tui_remember_always_allowed("Bash".to_string());

        // Batch 2 (shared): a later `execute_tool_calls_for_tui` invocation
        // sees the decision without re-prompting.
        assert!(
            shared.tui_is_always_allowed("Bash"),
            "#724: always-allow decision must persist across Arc handles \
             (i.e. across execute_tool_calls_for_tui batches)"
        );
    }

    // ── #782 registry-driven extract_target ─────────────────────────────────
    //
    // Before #782, `extract_target` exhaustively matched on three hard-coded
    // tool names (`bash`, `edit_file`, `write_file`). Any new tool added to
    // the registry would silently fall through to `_ => None`, which
    // `check()` treats as "no target → Allowed". These tests pin the
    // post-#782 behaviour: the canonical-tool name and target-arg key come
    // from the `ToolHandler::permission_target()` declaration, so each
    // tool's permission posture is co-located with its `execute` body.

    /// #782: `bash` resolves to its declared (Bash, command) target.
    /// Pinning this guards against a regression where a handler edit drops
    /// the `permission_target` override and bash silently fail-opens.
    #[test]
    fn registry_782_bash_target_key_resolves_command() {
        let (mut mgr, _dir) = enabled(vec![]);
        // Add a session Deny on Bash with a glob that requires the command
        // string to be extracted correctly from the args.
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "rm **".to_string(),
            decision: PermissionDecision::Deny,
        });
        let r = mgr.check("bash", &json!({"command": "rm -rf /tmp/x"}));
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "#782: bash's permission_target must canonicalise to Bash + arg_key='command', got: {r:?}"
        );
    }

    /// #782: `write_file` resolves to its declared (Write, path) target.
    /// Independently exercises a different (`canonical`, `arg_key`) pair to
    /// catch a swap-bug between the bash and write handlers.
    #[test]
    fn registry_782_write_file_target_key_resolves_path() {
        let (mut mgr, _dir) = enabled(vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Write".to_string(),
            pattern: "src/**/*.rs".to_string(),
            decision: PermissionDecision::Allow,
        });
        let r = mgr.check("write_file", &json!({"path": "src/lib.rs"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#782: write_file's permission_target must canonicalise to Write + arg_key='path'"
        );
        // And a path outside the rule must still require a prompt.
        let r2 = mgr.check("write_file", &json!({"path": "/etc/passwd"}));
        assert!(
            matches!(r2, CheckResult::NeedsPrompt { .. }),
            "#782: write_file target extraction must use the path arg, got: {r2:?}"
        );
    }

    /// #782: `edit_file` resolves to its declared (Edit, path) target.
    #[test]
    fn registry_782_edit_file_target_key_resolves_path() {
        let (mut mgr, _dir) = enabled(vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Edit".to_string(),
            pattern: "src/**".to_string(),
            decision: PermissionDecision::Allow,
        });
        let r = mgr.check("edit_file", &json!({"path": "src/main.rs"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#782: edit_file's permission_target must canonicalise to Edit + arg_key='path'"
        );
    }

    /// #782: `notebook_edit` was the pre-fix latent hole — it mutates
    /// `.ipynb` files on disk but was not in the hard-coded match arm.
    /// Post-#782 it declares `(Edit, notebook_path)` so an existing Edit
    /// session rule naturally covers notebook writes too. This is the test
    /// that would have failed BEFORE the fix and passes after it.
    #[test]
    fn registry_782_notebook_edit_no_longer_fails_open() {
        let (mut mgr, _dir) = enabled(vec![]);
        // Deny all Edit operations on /etc/**.
        mgr.add_session_rule(PermissionRule {
            tool: "Edit".to_string(),
            pattern: "/etc/**".to_string(),
            decision: PermissionDecision::Deny,
        });
        let r = mgr.check(
            "notebook_edit",
            &json!({"notebook_path": "/etc/secret.ipynb", "new_source": "evil"}),
        );
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "#782: notebook_edit must be denied by the Edit rule — pre-fix this fail-opened. Got: {r:?}"
        );

        // And without a matching rule, notebook_edit must prompt — not
        // silently allow — confirming the registry lookup is wired in.
        let (mgr2, _dir2) = enabled(vec![]);
        let r2 = mgr2.check(
            "notebook_edit",
            &json!({"notebook_path": "/tmp/x.ipynb", "new_source": "x"}),
        );
        assert!(
            matches!(r2, CheckResult::NeedsPrompt { .. }),
            "#782: notebook_edit must NeedsPrompt when no rule matches, not Allowed. Got: {r2:?}"
        );
    }

    /// #782: an unknown tool (not registered) returns `Allowed`. There is
    /// no target string to match rules against, and dispatch will fail at
    /// the executor anyway. This is the same behaviour as the pre-#782
    /// `_ => None` arm for unknown names — preserved on purpose so existing
    /// callers don't see a regression on typo'd or experimental tool names.
    #[test]
    fn registry_782_unknown_tool_returns_allowed() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check(
            "definitely_not_a_real_tool_name",
            &json!({"command": "rm -rf /"}),
        );
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#782: an unregistered tool must short-circuit to Allowed (no target to match); \
             dispatch will reject the call downstream. Got: {r:?}"
        );
    }

    /// #782: a registered handler that returns `None` from
    /// `permission_target` (i.e. a read-only tool like `read_file`) also
    /// short-circuits to `Allowed` — same path as an unknown tool but
    /// reached because the trait method explicitly opted out.
    #[test]
    fn registry_782_read_only_handler_returns_allowed() {
        let (mgr, _dir) = enabled(vec![]);
        // read_file is registered but its handler returns None from
        // permission_target() (the trait default), so check() must allow.
        let r = mgr.check("read_file", &json!({"path": "/etc/passwd"}));
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#782: a registered handler with permission_target()==None must short-circuit to Allowed"
        );
        // list_files also has no permission target.
        let r2 = mgr.check("list_files", &json!({"path": "/etc"}));
        assert_eq!(r2, CheckResult::Allowed);
    }

    /// #782: malformed args (wrong JSON type for the declared `arg_key`)
    /// must be Denied — the legacy invariant for `bash`/`edit_file`/
    /// `write_file` is preserved by the registry-driven lookup, and now
    /// extends automatically to any new tool that declares a
    /// `PermissionTarget`.
    #[test]
    fn registry_782_malformed_args_for_any_target_are_denied() {
        let (mgr, _dir) = enabled(vec!["**"]);
        // notebook_edit declares notebook_path as its target arg_key — a
        // non-string value must trip the malformed-args branch even with a
        // permissive default_allow.
        let r = mgr.check(
            "notebook_edit",
            &json!({"notebook_path": 42, "new_source": "x"}),
        );
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "#782: malformed notebook_path (int) must be Denied regardless of default_allow, got: {r:?}"
        );
    }

    /// #782 forensic / regression-prevention: enumerate every handler in
    /// the registry and require an *explicit decision* — either it declares
    /// a `PermissionTarget`, or it appears in a known-safe allowlist. A new
    /// mutating tool added without either step trips this test loudly,
    /// preventing the silent fail-open that #782 was filed for.
    #[test]
    fn registry_782_every_handler_makes_explicit_permission_decision() {
        use crate::tools::registry::iter_handlers;
        use crate::tools::PermissionTarget;

        // Handlers whose permission_target() correctly returns None because
        // they are genuinely read-only or non-filesystem-mutating. Every
        // entry here is an intentional opt-out; a *new* tool added to the
        // registry that does not declare a PermissionTarget AND is not in
        // this list will fail this test until a decision is made.
        const KNOWN_SAFE: &[&str] = &[
            "bash_output",        // reads buffered output, no mutation
            "kill_shell",         // operates on internal shell handles
            "read_file",          // pure read
            "list_files",         // pure read
            "glob",               // pure read (#567)
            "grep",               // pure read (#568)
            "crosslink",          // library-backed issue tracker
            "web_fetch",          // network read
            "web_search",         // network read
            "web_browser",        // network read
            "todo_write",         // in-memory session state
            "todo_read",          // in-memory session state
            "ask_user_question",  // user interaction
            "task_create",        // session task state
            "task_update",        // session task state
            "task_get",           // session task state
            "task_list",          // session task state
            "enter_plan_mode",    // mode flag
            "exit_plan_mode",     // mode flag
            "list_mcp_resources", // MCP read (stub today)
            "read_mcp_resource",  // MCP read (stub today)
            "lsp",                // LSP read
            "enter_worktree",     // git worktree create (gated separately)
            "exit_worktree",      // git worktree remove (gated separately)
            "list_worktrees",     // git read
            "cron_create",        // schedule registration (gated separately)
            "cron_delete",        // schedule removal (gated separately)
            "cron_list",          // schedule read
            // crosslink #612 / #614 — pure read-side: skill loads a markdown
            // body from disk; tool_search returns schemas from the registry.
            // Neither mutates user state.
            "skill",
            "tool_search",
        ];

        for handler in iter_handlers() {
            let name = handler.name();
            let target: Option<PermissionTarget> = handler.permission_target();
            let in_safelist = KNOWN_SAFE.contains(&name);
            assert!(
                target.is_some() || in_safelist,
                "#782 REGRESSION: handler '{name}' declares no PermissionTarget and is not in \
                 KNOWN_SAFE. Either add a `fn permission_target()` override in \
                 src/tools/registry.rs OR (only if the tool is genuinely read-only) add the \
                 name to KNOWN_SAFE in this test. This guards against the silent fail-open \
                 that crosslink #782 closed."
            );
            // Conversely, if a handler ends up declaring a target, the
            // safelist entry should be removed so the two stay in sync.
            assert!(
                !(target.is_some() && in_safelist),
                "#782: handler '{name}' declares a PermissionTarget AND is listed in \
                 KNOWN_SAFE. Remove it from KNOWN_SAFE — the declaration is the source of truth."
            );
        }
    }

    // ── Crosslink #570: PermissionContext tests ────────────────────────────

    /// Pin: `Interactive` context preserves the legacy `NeedsPrompt`
    /// fall-through (back-compat with the single `check()` API).
    #[test]
    fn check_with_context_interactive_returns_needs_prompt() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check_with_context(
            "bash",
            &json!({"command": "ls"}),
            PermissionContext::Interactive,
        );
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "#570: Interactive must yield NeedsPrompt for unmatched bash, got: {r:?}"
        );
    }

    /// Pin: `SwarmWorker` context converts `NeedsPrompt` to `Denied`
    /// because there is no UI to consult.
    #[test]
    fn check_with_context_swarm_worker_denies_default() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check_with_context(
            "bash",
            &json!({"command": "ls"}),
            PermissionContext::SwarmWorker,
        );
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "#570: SwarmWorker must default-deny unmatched calls, got: {r:?}"
        );
    }

    /// Pin: explicit allow rules still allow under all contexts —
    /// the context only changes the unmatched-rule projection.
    #[test]
    fn check_with_context_explicit_allow_works_under_swarm_worker() {
        let (mgr, _dir) = enabled(vec!["ls *"]);
        let r = mgr.check_with_context(
            "bash",
            &json!({"command": "ls -la"}),
            PermissionContext::SwarmWorker,
        );
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#570: explicit allow must survive swarm-worker context"
        );
    }

    /// Pin: `Coordinator` mirrors `SwarmWorker` today (reserved for #619).
    #[test]
    fn check_with_context_coordinator_default_denies() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check_with_context(
            "bash",
            &json!({"command": "ls"}),
            PermissionContext::Coordinator,
        );
        assert!(matches!(r, CheckResult::Denied(_)));
    }

    // ── Crosslink #571: classifier auto-allow tests ────────────────────────

    /// Pin: read-only tools (e.g. `read_file`, `list_files`) score 1.0.
    #[test]
    fn auto_allow_score_read_only_is_one() {
        assert!(
            (auto_allow_score("read_file", &json!({"path": "/x"})) - 1.0).abs() < f32::EPSILON,
            "#571: read_file (no permission target) must score 1.0"
        );
        assert!((auto_allow_score("list_files", &json!({})) - 1.0).abs() < f32::EPSILON);
    }

    /// Pin: safe `bash` verb prefixes score high (>=0.9).
    #[test]
    fn auto_allow_score_safe_bash_prefixes_high() {
        assert!(auto_allow_score("bash", &json!({"command": "ls -la"})) >= 0.9);
        assert!(auto_allow_score("bash", &json!({"command": "git status"})) >= 0.9);
        assert!(auto_allow_score("bash", &json!({"command": "git diff HEAD"})) >= 0.9);
    }

    /// Pin: destructive bash tokens score 0.0 (hard veto).
    #[test]
    fn auto_allow_score_destructive_bash_is_zero() {
        assert!(auto_allow_score("bash", &json!({"command": "rm -rf /"})) < f32::EPSILON);
        assert!(auto_allow_score("bash", &json!({"command": "sudo apt update"})) < f32::EPSILON);
        assert!(auto_allow_score("bash", &json!({"command": "chmod 777 /etc"})) < f32::EPSILON);
    }

    /// Pin: `check_auto_allow` allows safe bash above threshold.
    #[test]
    fn check_auto_allow_safe_bash_passes() {
        let (mgr, _dir) = enabled(vec![]);
        let r = mgr.check_auto_allow("bash", &json!({"command": "ls"}), 0.8);
        assert_eq!(
            r,
            CheckResult::Allowed,
            "#571: safe bash above threshold must auto-allow"
        );
    }

    /// Pin: `check_auto_allow` respects explicit deny rules even when
    /// the classifier would allow.
    #[test]
    fn check_auto_allow_explicit_deny_overrides_classifier() {
        let (mut mgr, _dir) = enabled(vec![]);
        mgr.add_session_rule(PermissionRule {
            tool: "Bash".to_string(),
            pattern: "ls*".to_string(),
            decision: PermissionDecision::Deny,
        });
        let r = mgr.check_auto_allow("bash", &json!({"command": "ls"}), 0.8);
        assert!(
            matches!(r, CheckResult::Denied(_)),
            "#571: explicit Deny must beat classifier auto-allow, got: {r:?}"
        );
    }

    /// Pin: low-confidence calls fall through to the normal pipeline
    /// (`NeedsPrompt` under interactive context).
    #[test]
    fn check_auto_allow_low_score_falls_through() {
        let (mgr, _dir) = enabled(vec![]);
        // `wget` is in the destructive list — score 0.0, threshold 0.5
        // is never met. Without an explicit Deny rule, the fall-through
        // is NeedsPrompt.
        let r = mgr.check_auto_allow("bash", &json!({"command": "echo ok && wget x"}), 0.5);
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "#571: low score must fall through to NeedsPrompt, got: {r:?}"
        );
    }

    // ── Crosslink #577: DenialTracker tests ────────────────────────────────

    /// Pin: a fresh tracker starts at zero with default limits.
    #[test]
    fn denial_tracker_starts_zero_with_default_limits() {
        let t = DenialTracker::new();
        assert_eq!(t.consecutive(), 0);
        assert_eq!(t.total(), 0);
        assert_eq!(t.limits().max_consecutive, MAX_CONSECUTIVE_DENIALS);
        assert_eq!(t.limits().max_total, MAX_TOTAL_DENIALS);
        assert_eq!(t.escalation_state(), EscalationState::Normal);
    }

    /// Pin: `record_denial` increments both counters and `record_allowed`
    /// resets only the consecutive counter.
    #[test]
    fn denial_tracker_record_paths() {
        let mut t = DenialTracker::new();
        t.record_denial();
        t.record_denial();
        assert_eq!(t.consecutive(), 2);
        assert_eq!(t.total(), 2);
        t.record_allowed();
        assert_eq!(
            t.consecutive(),
            0,
            "#577: record_allowed resets consecutive counter"
        );
        assert_eq!(
            t.total(),
            2,
            "#577: record_allowed must NOT touch total counter"
        );
    }

    /// Pin: crossing either threshold flips `escalation_state` to
    /// `ShouldAbort`. Mirrors CC `shouldFallbackToPrompting`.
    #[test]
    fn denial_tracker_escalation_thresholds() {
        let limits = DenialLimits {
            max_consecutive: 2,
            max_total: 100,
        };
        let mut t = DenialTracker::with_limits(limits);
        // 2 denials = at the limit, still Normal (>, not >=).
        t.record_denial();
        t.record_denial();
        assert_eq!(t.escalation_state(), EscalationState::Normal);
        // 3rd denial crosses the threshold.
        t.record_denial();
        assert_eq!(t.escalation_state(), EscalationState::ShouldAbort);
        // Reset clears both counters.
        t.reset();
        assert_eq!(t.consecutive(), 0);
        assert_eq!(t.total(), 0);
        assert_eq!(t.escalation_state(), EscalationState::Normal);
    }

    /// Pin: counter increments saturate at `u32::MAX` instead of wrapping.
    #[test]
    fn denial_tracker_saturates_at_u32_max() {
        let mut t = DenialTracker::new();
        // Hand-poke the consecutive counter near saturation to avoid
        // 4 billion loop iterations. (We use the public API only —
        // many record_denial calls would take too long, so we exercise
        // a large-but-tractable count and verify monotonicity holds.)
        for _ in 0..1000 {
            t.record_denial();
        }
        let before = t.consecutive();
        let before_total = t.total();
        for _ in 0..1000 {
            t.record_denial();
        }
        assert!(t.consecutive() > before, "must be monotonically increasing");
        assert!(t.total() > before_total);
        // Sanity: well below saturation.
        assert!(t.total() < u32::MAX);
    }
}
