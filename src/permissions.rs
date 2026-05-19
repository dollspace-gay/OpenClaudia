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
use std::collections::HashMap;
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
    pub const fn unrestricted() -> Self {
        // `enabled = false` short-circuits `check()` to `CheckResult::Allowed`.
        Self {
            persisted_rules: Vec::new(),
            session_rules: Vec::new(),
            default_allow: Vec::new(),
            persist_path: PathBuf::new(),
            enabled: false,
            consecutive_denials: 0,
            total_denials: 0,
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

        // SECURITY: Ignore dangerously_disable_sandbox from tool args.
        // This flag must ONLY be honored from user-level config (AppConfig),
        // never from model-controlled tool call arguments.
        if canonical_tool == "Bash" {
            if let Some(disable) = tool_args.get("dangerously_disable_sandbox") {
                if disable.as_bool().unwrap_or(false) {
                    warn!(
                        tool = %canonical_tool,
                        target = %target,
                        "Model attempted dangerously_disable_sandbox=true in tool args — IGNORED. \
                         This flag is only honored from user-level configuration."
                    );
                }
            }
        }

        // 1. Check persisted always-allow rules
        for rule in &self.persisted_rules {
            if rule.decision == PermissionDecision::AlwaysAllow
                && Self::rule_matches(rule, &canonical_tool, &target)
            {
                debug!(
                    tool = %canonical_tool,
                    target = %target,
                    pattern = %rule.pattern,
                    "Allowed by persisted always-allow rule"
                );
                return CheckResult::Allowed;
            }
        }

        // 2. Check session rules
        for rule in &self.session_rules {
            if Self::rule_matches(rule, &canonical_tool, &target) {
                match &rule.decision {
                    PermissionDecision::Allow | PermissionDecision::AlwaysAllow => {
                        debug!(
                            tool = %canonical_tool,
                            target = %target,
                            pattern = %rule.pattern,
                            "Allowed by session rule"
                        );
                        return CheckResult::Allowed;
                    }
                    PermissionDecision::Deny => {
                        debug!(
                            tool = %canonical_tool,
                            target = %target,
                            pattern = %rule.pattern,
                            "Denied by session rule"
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
                debug!(
                    tool = %canonical_tool,
                    target = %target,
                    pattern = %pattern,
                    "Allowed by default_allow config pattern"
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
    fn extract_target(
        tool_name: &str,
        tool_args: &serde_json::Value,
    ) -> Option<Result<(String, String), String>> {
        match tool_name {
            "bash" => {
                let cmd = tool_args.get("command").and_then(|v| v.as_str());
                match (cmd, tool_args.get("command")) {
                    (Some(s), _) => Some(Ok(("Bash".to_string(), s.to_string()))),
                    (None, Some(_)) => Some(Err("Bash".to_string())), // key present but not a string
                    (None, None) => Some(Ok(("Bash".to_string(), String::new()))), // key absent
                }
            }
            "edit_file" => {
                let path = tool_args.get("path").and_then(|v| v.as_str());
                match (path, tool_args.get("path")) {
                    (Some(s), _) => Some(Ok(("Edit".to_string(), s.to_string()))),
                    (None, Some(_)) => Some(Err("Edit".to_string())),
                    (None, None) => Some(Ok(("Edit".to_string(), String::new()))),
                }
            }
            "write_file" => {
                let path = tool_args.get("path").and_then(|v| v.as_str());
                match (path, tool_args.get("path")) {
                    (Some(s), _) => Some(Ok(("Write".to_string(), s.to_string()))),
                    (None, Some(_)) => Some(Err("Write".to_string())),
                    (None, None) => Some(Ok(("Write".to_string(), String::new()))),
                }
            }
            // Read-only tools, task tools, and memory tools don't need permission checks
            _ => None,
        }
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
    fn glob_to_regex_cached(pattern: &str) -> Option<Regex> {
        let cache = GLOB_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(re) = cache.get(pattern) {
            return Some(re.clone());
        }
        let regex_str = Self::glob_to_regex(pattern);
        let result = Regex::new(&regex_str);
        drop(cache);
        match result {
            Ok(re) => {
                GLOB_CACHE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(pattern.to_string(), re.clone());
                Some(re)
            }
            Err(e) => {
                warn!(pattern = %pattern, error = %e, "Invalid glob pattern");
                None
            }
        }
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
    fn test_dangerously_disable_sandbox_in_tool_args_is_ignored() {
        let (mgr, _dir) = make_manager(true, vec![]);
        // Model-supplied dangerously_disable_sandbox must NOT bypass permission checks
        let result = mgr.check(
            "bash",
            &json!({"command": "rm -rf /", "dangerously_disable_sandbox": true}),
        );
        // Should require a prompt, NOT be auto-allowed
        assert!(
            matches!(result, CheckResult::NeedsPrompt { .. }),
            "dangerously_disable_sandbox in tool args must not bypass permissions"
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

    /// B2-deny-2: empty-string glob matches only empty target (bash with no command).
    #[test]
    fn b2_empty_glob_matches_only_empty_target() {
        let (mgr, _dir) = enabled(vec![""]);

        // Non-empty bash command must NOT be allowed by the empty-string pattern.
        let r = mgr.check("bash", &json!({"command": "ls"}));
        assert!(
            matches!(r, CheckResult::NeedsPrompt { .. }),
            "B2: empty glob must not match a non-empty bash command"
        );

        // Bash with absent command key → target is "" → the empty glob matches.
        let r_empty = mgr.check("bash", &json!({}));
        assert_eq!(
            r_empty,
            CheckResult::Allowed,
            "B2: empty glob must match an empty (absent) command target"
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
}
