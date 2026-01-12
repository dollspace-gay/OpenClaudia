//! Configuration loader with environment variable substitution.
//!
//! Loads configuration from:
//! 1. Default values
//! 2. `.openclaudia/config.yaml` in project directory
//! 3. `~/.openclaudia/config.yaml` in home directory
//! 4. Environment variables with `OPENCLAUDIA_` prefix

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Main configuration structure
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub proxy: ProxyConfig,
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub keybindings: KeybindingsConfig,
}

/// Proxy server configuration
#[derive(Debug, Deserialize, Clone)]
pub struct ProxyConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_target")]
    pub target: String,
}

fn default_port() -> u16 {
    8080
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_target() -> String {
    "anthropic".to_string()
}

/// Thinking/reasoning mode configuration
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ThinkingConfig {
    /// Enable thinking mode (default: true for supported providers)
    #[serde(default = "default_thinking_enabled")]
    pub enabled: bool,
    /// Token budget for thinking (provider-specific)
    /// - Anthropic: min 1024, no max
    /// - Google Gemini 2.5: 128-32768
    /// - Z.AI/GLM: no explicit budget
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    /// Preserve thinking across turns (Z.AI/GLM specific)
    #[serde(default)]
    pub preserve_across_turns: bool,
    /// Reasoning effort level for OpenAI o1/o3: "low", "medium", "high"
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

fn default_thinking_enabled() -> bool {
    true
}

/// Provider configuration (Anthropic, OpenAI, Google, etc.)
#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub thinking: ThinkingConfig,
}

/// Hooks configuration
#[derive(Debug, Deserialize, Clone, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub session_start: Vec<HookEntry>,
    #[serde(default)]
    pub session_end: Vec<HookEntry>,
    #[serde(default)]
    pub pre_tool_use: Vec<HookEntry>,
    #[serde(default)]
    pub post_tool_use: Vec<HookEntry>,
    #[serde(default)]
    pub user_prompt_submit: Vec<HookEntry>,
    #[serde(default)]
    pub stop: Vec<HookEntry>,
}

/// Individual hook entry
#[derive(Debug, Deserialize, Clone)]
pub struct HookEntry {
    #[serde(default)]
    pub matcher: Option<String>,
    pub hooks: Vec<Hook>,
}

/// Hook definition
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum Hook {
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    #[serde(rename = "prompt")]
    Prompt {
        prompt: String,
        #[serde(default = "default_prompt_timeout")]
        timeout: u64,
    },
}

fn default_timeout() -> u64 {
    60
}

fn default_prompt_timeout() -> u64 {
    30
}

/// Session configuration
#[derive(Debug, Deserialize, Clone)]
pub struct SessionConfig {
    #[serde(default = "default_timeout_minutes")]
    pub timeout_minutes: u64,
    #[serde(default = "default_persist_path")]
    pub persist_path: PathBuf,
}

/// Keybinding action names
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    /// Start a new session
    NewSession,
    /// List saved sessions
    ListSessions,
    /// Export conversation to markdown
    Export,
    /// Copy last response to clipboard
    CopyResponse,
    /// Open external editor
    Editor,
    /// Show/switch models
    Models,
    /// Toggle Build/Plan mode
    ToggleMode,
    /// Cancel in-progress response
    Cancel,
    /// Show session status
    Status,
    /// Show help
    Help,
    /// Clear/new conversation
    Clear,
    /// Exit the application
    Exit,
    /// Undo last exchange
    Undo,
    /// Redo last undone exchange
    Redo,
    /// Compact conversation
    Compact,
    /// No action (disabled keybinding)
    None,
}

/// Keybindings configuration
/// Maps key combinations to actions. Use "none" to disable a keybinding.
#[derive(Debug, Deserialize, Clone)]
pub struct KeybindingsConfig {
    /// Map of key combination strings to action names
    /// Example: { "ctrl-x n": "new_session", "f2": "models", "tab": "none" }
    #[serde(flatten)]
    pub bindings: HashMap<String, KeyAction>,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        let mut bindings = HashMap::new();
        // Default keybindings (Ctrl+X leader key pattern)
        bindings.insert("ctrl-x n".to_string(), KeyAction::NewSession);
        bindings.insert("ctrl-x l".to_string(), KeyAction::ListSessions);
        bindings.insert("ctrl-x x".to_string(), KeyAction::Export);
        bindings.insert("ctrl-x y".to_string(), KeyAction::CopyResponse);
        bindings.insert("ctrl-x e".to_string(), KeyAction::Editor);
        bindings.insert("ctrl-x m".to_string(), KeyAction::Models);
        bindings.insert("ctrl-x s".to_string(), KeyAction::Status);
        bindings.insert("ctrl-x h".to_string(), KeyAction::Help);
        bindings.insert("f2".to_string(), KeyAction::Models);
        bindings.insert("tab".to_string(), KeyAction::ToggleMode);
        bindings.insert("escape".to_string(), KeyAction::Cancel);
        Self { bindings }
    }
}

impl KeybindingsConfig {
    /// Get the action for a key combination
    pub fn get_action(&self, key: &str) -> Option<&KeyAction> {
        self.bindings.get(&key.to_lowercase())
    }

    /// Check if a key is bound (returns None for disabled or unbound keys)
    pub fn is_bound(&self, key: &str) -> bool {
        matches!(self.get_action(key), Some(action) if *action != KeyAction::None)
    }

    /// Get all bindings for a specific action
    pub fn get_keys_for_action(&self, action: &KeyAction) -> Vec<&String> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(k, _)| k)
            .collect()
    }

    /// Get the action for a key, with default fallback
    /// Returns the configured action or the default action for that key
    pub fn get_action_or_default(&self, key: &str) -> KeyAction {
        self.get_action(key).cloned().unwrap_or(KeyAction::None)
    }
}

fn default_timeout_minutes() -> u64 {
    30
}

fn default_persist_path() -> PathBuf {
    PathBuf::from(".openclaudia/session")
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            timeout_minutes: default_timeout_minutes(),
            persist_path: default_persist_path(),
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            target: default_target(),
        }
    }
}

/// Load configuration from all sources
pub fn load_config() -> Result<AppConfig, ConfigError> {
    let mut builder = Config::builder();

    // Set defaults
    builder = builder
        .set_default("proxy.port", 8080)?
        .set_default("proxy.host", "127.0.0.1")?
        .set_default("proxy.target", "anthropic")?
        .set_default("session.timeout_minutes", 30)?
        .set_default("session.persist_path", ".openclaudia/session")?;

    // Add default providers
    builder = builder
        .set_default("providers.anthropic.base_url", "https://api.anthropic.com")?
        .set_default("providers.openai.base_url", "https://api.openai.com")?
        .set_default(
            "providers.google.base_url",
            "https://generativelanguage.googleapis.com",
        )?
        // Z.AI/GLM (OpenAI-compatible)
        .set_default(
            "providers.zai.base_url",
            "https://api.z.ai/api/coding/paas/v4",
        )?
        // DeepSeek (OpenAI-compatible)
        .set_default("providers.deepseek.base_url", "https://api.deepseek.com")?
        // Qwen/Alibaba (OpenAI-compatible)
        .set_default(
            "providers.qwen.base_url",
            "https://dashscope.aliyuncs.com/compatible-mode",
        )?;

    // Load from project config file
    let project_config = PathBuf::from(".openclaudia/config.yaml");
    if project_config.exists() {
        builder = builder.add_source(File::from(project_config).required(false));
    }

    // Load from home directory config file
    if let Some(home) = dirs::home_dir() {
        let home_config: PathBuf = home.join(".openclaudia/config.yaml");
        if home_config.exists() {
            builder = builder.add_source(File::from(home_config).required(false));
        }
    }

    // Load from environment variables with OPENCLAUDIA_ prefix
    // e.g., OPENCLAUDIA_PROXY_PORT=9090, OPENCLAUDIA_PROVIDERS_ANTHROPIC_API_KEY=sk-...
    builder = builder.add_source(
        Environment::with_prefix("OPENCLAUDIA")
            .separator("_")
            .try_parsing(true),
    );

    // Also check for provider API keys from standard env vars
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        builder = builder.set_override("providers.anthropic.api_key", key)?;
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        builder = builder.set_override("providers.openai.api_key", key)?;
    }
    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        builder = builder.set_override("providers.google.api_key", key)?;
    }
    if let Ok(key) = std::env::var("ZAI_API_KEY") {
        builder = builder.set_override("providers.zai.api_key", key)?;
    }
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        builder = builder.set_override("providers.deepseek.api_key", key)?;
    }
    if let Ok(key) = std::env::var("QWEN_API_KEY") {
        builder = builder.set_override("providers.qwen.api_key", key)?;
    }

    builder.build()?.try_deserialize()
}

/// Get the active provider configuration
impl AppConfig {
    pub fn active_provider(&self) -> Option<&ProviderConfig> {
        self.providers.get(&self.proxy.target)
    }

    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        // This test verifies defaults work without any config files
        let config = ProxyConfig::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.target, "anthropic");
    }
}
