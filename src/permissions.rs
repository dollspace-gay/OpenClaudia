//! Granular tool permission system for OpenClaudia.
//!
//! Provides glob-pattern-based permission rules that control tool execution:
//! - Per-tool rules with glob patterns matching commands or file paths
//! - Three decision levels: Allow, Deny, AlwaysAllow (persisted across sessions)
//! - Configurable defaults and persistence to `.openclaudia/permissions.json`
//!
//! Check order: always-allow rules -> session rules -> config default_allow -> Deny (prompt user)

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
    /// For Edit/Write: matched against the file_path.
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

/// Manages permission rules for tool execution.
///
/// Rules are checked in priority order:
/// 1. Persisted always-allow rules (loaded from disk)
/// 2. Session rules (added at runtime via user responses)
/// 3. Config-level default_allow patterns
/// 4. If nothing matches, returns `NeedsPrompt`
pub struct PermissionManager {
    /// Persisted rules (AlwaysAllow) loaded from `.openclaudia/permissions.json`
    persisted_rules: Vec<PermissionRule>,
    /// Transient session rules (Allow/Deny added during this session)
    session_rules: Vec<PermissionRule>,
    /// Default allow patterns from config
    default_allow: Vec<String>,
    /// Path to the persistence file
    persist_path: PathBuf,
    /// Whether the permission system is enabled
    enabled: bool,
}

impl PermissionManager {
    /// Create a new PermissionManager, loading persisted rules from disk.
    pub fn new(
        persist_path: impl Into<PathBuf>,
        enabled: bool,
        default_allow: Vec<String>,
    ) -> Self {
        let persist_path = persist_path.into();
        let persisted_rules = Self::load_persisted_rules(&persist_path);
        Self {
            persisted_rules,
            session_rules: Vec::new(),
            default_allow,
            persist_path,
            enabled,
        }
    }

    /// Check whether a tool invocation is allowed.
    ///
    /// - `tool_name`: e.g. "bash", "edit_file", "write_file"
    /// - `tool_args`: the parsed arguments map from the tool call
    ///
    /// Returns `Allowed`, `Denied`, or `NeedsPrompt`.
    pub fn check(&self, tool_name: &str, tool_args: &serde_json::Value) -> CheckResult {
        if !self.enabled {
            return CheckResult::Allowed;
        }

        // Determine the canonical tool category and the target string to match against
        let (canonical_tool, target) = match Self::extract_target(tool_name, tool_args) {
            Some(pair) => pair,
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
    /// Returns `None` for tools that don't need permission checks (e.g. read-only tools).
    fn extract_target(tool_name: &str, tool_args: &serde_json::Value) -> Option<(String, String)> {
        match tool_name {
            "bash" => {
                let cmd = tool_args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some(("Bash".to_string(), cmd.to_string()))
            }
            "edit_file" => {
                let path = tool_args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                Some(("Edit".to_string(), path.to_string()))
            }
            "write_file" => {
                let path = tool_args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                Some(("Write".to_string(), path.to_string()))
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
        let re = Self::glob_to_regex_cached(pattern);
        match re {
            Some(re) => re.is_match(target),
            None => false,
        }
    }

    /// Return a cached compiled `Regex` for a glob pattern, compiling and caching it on first use.
    fn glob_to_regex_cached(pattern: &str) -> Option<Regex> {
        let mut cache = GLOB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(re) = cache.get(pattern) {
            return Some(re.clone());
        }
        let regex_str = Self::glob_to_regex(pattern);
        match Regex::new(&regex_str) {
            Ok(re) => {
                cache.insert(pattern.to_string(), re.clone());
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
    pub fn persisted_rules(&self) -> &[PermissionRule] {
        &self.persisted_rules
    }

    /// Get all session rules (for inspection/debugging).
    pub fn session_rules(&self) -> &[PermissionRule] {
        &self.session_rules
    }

    /// Check if the permission system is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Clear all session rules (called at session end).
    pub fn clear_session_rules(&mut self) {
        self.session_rules.clear();
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

    #[test]
    fn test_disabled_always_allows() {
        let (mgr, _dir) = make_manager(false, vec![]);
        let result = mgr.check("bash", &json!({"command": "rm -rf /"}));
        assert_eq!(result, CheckResult::Allowed);
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
