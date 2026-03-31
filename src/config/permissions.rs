use serde::Deserialize;

/// Tool permission system configuration.
///
/// Controls whether permission checks are performed before tool execution
/// and provides default allow-list patterns.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct PermissionsConfig {
    /// Enable the permission system (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Glob patterns that are pre-allowed without prompting.
    /// Patterns are matched against the tool's primary argument
    /// (command string for Bash, file_path for Edit/Write).
    #[serde(default)]
    pub default_allow: Vec<String>,
}
