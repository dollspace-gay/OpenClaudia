use serde::Deserialize;

/// Tool permission system configuration.
///
/// Controls whether permission checks are performed before tool execution
/// and provides default allow-list patterns.
///
/// # Default posture
///
/// `enabled` defaults to `true` (deny-by-default, matching Claude Code's
/// always-on permission pipeline). A fresh installation with no
/// `permissions:` block in `config.yaml` will **prompt before every
/// destructive tool call**.
///
/// To opt out of the permission system entirely, set `enabled: false` in
/// your config. This is **not recommended** for production use; it is
/// equivalent to Claude Code's `bypassPermissions` mode and removes all
/// audit trails.
///
/// # Deprecation note
///
/// The `enabled` field is scheduled for removal. The long-term plan is to
/// make permissions always-on and replace opt-out with an explicit
/// `dangerously_disable_permissions: true`. See crosslink #282.
#[derive(Debug, Deserialize, Clone)]
pub struct PermissionsConfig {
    /// Enable the permission system.
    ///
    /// Defaults to `true` (deny-by-default). Set to `false` only to
    /// replicate the old allow-all behaviour; note that doing so also
    /// silences all persisted Deny rules.
    ///
    /// **Deprecated**: prefer leaving this unset (the default `true`)
    /// and use `dangerously_disable_permissions` when an explicit bypass
    /// is required. See crosslink #282.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Glob patterns that are pre-allowed without prompting.
    /// Patterns are matched against the tool's primary argument
    /// (command string for Bash, `file_path` for Edit/Write).
    #[serde(default)]
    pub default_allow: Vec<String>,
}

/// Returns the default value for `PermissionsConfig::enabled`.
///
/// `true` — permissions are on by default (deny-by-default posture).
/// Fixes crosslink #282: the previous `#[serde(default)]` on a `bool`
/// field silently defaulted to `false`, making a fresh install allow-all.
const fn default_enabled() -> bool {
    true
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            default_allow: Vec::new(),
        }
    }
}
