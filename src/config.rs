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
    /// Maximum agentic turns (API round-trips with tool execution) per user message.
    /// 0 means unlimited (like Claude Code). Default: 0 (unlimited).
    #[serde(default)]
    pub max_turns: u32,
    /// Token tracking configuration
    #[serde(default)]
    pub token_tracking: TokenTrackingConfig,
}

/// Token tracking and budget configuration
#[derive(Debug, Deserialize, Clone)]
pub struct TokenTrackingConfig {
    /// Enable per-turn token tracking (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Log token usage at info level each turn (default: true)
    #[serde(default = "default_true")]
    pub log_usage: bool,
    /// Warn when estimated input exceeds this percentage of context window (0.0-1.0)
    /// Default: 0.75 (warn at 75% of context window)
    #[serde(default = "default_warn_threshold")]
    pub warn_threshold: f32,
    /// Maximum output tokens per response (0 = provider default)
    #[serde(default)]
    pub max_output_tokens: u32,
}

fn default_true() -> bool {
    true
}

fn default_warn_threshold() -> f32 {
    0.75
}

impl Default for TokenTrackingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_usage: true,
            warn_threshold: 0.75,
            max_output_tokens: 0,
        }
    }
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
            max_turns: 0,
            token_tracking: TokenTrackingConfig::default(),
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

    // ========================================================================
    // ProxyConfig Tests
    // ========================================================================

    #[test]
    fn test_proxy_config_default_values() {
        let config = ProxyConfig::default();
        assert_eq!(config.port, default_port());
        assert_eq!(config.host, default_host());
        assert_eq!(config.target, default_target());
    }

    // ========================================================================
    // ThinkingConfig Tests
    // ========================================================================

    #[test]
    fn test_thinking_config_default() {
        // Note: #[derive(Default)] uses bool::default() = false
        // The serde default only applies during deserialization
        let config = ThinkingConfig::default();
        assert!(!config.enabled); // derive(Default) uses bool default = false
        assert!(config.budget_tokens.is_none());
        assert!(!config.preserve_across_turns);
        assert!(config.reasoning_effort.is_none());
    }

    #[test]
    fn test_thinking_config_serde_default() {
        // When deserializing, the serde default function is used
        let config: ThinkingConfig = serde_json::from_str("{}").unwrap();
        assert!(config.enabled); // serde uses default_thinking_enabled() = true
        assert!(config.budget_tokens.is_none());
    }

    #[test]
    fn test_thinking_config_with_budget() {
        let json = r#"{
            "enabled": true,
            "budget_tokens": 10000,
            "preserve_across_turns": true,
            "reasoning_effort": "high"
        }"#;

        let config: ThinkingConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.budget_tokens, Some(10000));
        assert!(config.preserve_across_turns);
        assert_eq!(config.reasoning_effort, Some("high".to_string()));
    }

    // ========================================================================
    // SessionConfig Tests
    // ========================================================================

    #[test]
    fn test_session_config_default() {
        let config = SessionConfig::default();
        assert_eq!(config.timeout_minutes, 30);
        assert_eq!(config.persist_path, PathBuf::from(".openclaudia/session"));
    }

    #[test]
    fn test_session_config_from_json() {
        let json = r#"{
            "timeout_minutes": 60,
            "persist_path": "/custom/path"
        }"#;

        let config: SessionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeout_minutes, 60);
        assert_eq!(config.persist_path, PathBuf::from("/custom/path"));
    }

    // ========================================================================
    // KeyAction Tests
    // ========================================================================

    #[test]
    fn test_key_action_serialization() {
        let action = KeyAction::NewSession;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"new_session\"");

        let action2: KeyAction = serde_json::from_str("\"toggle_mode\"").unwrap();
        assert_eq!(action2, KeyAction::ToggleMode);
    }

    #[test]
    fn test_key_action_all_variants() {
        // Ensure all variants can be serialized/deserialized
        let actions = vec![
            ("\"new_session\"", KeyAction::NewSession),
            ("\"list_sessions\"", KeyAction::ListSessions),
            ("\"export\"", KeyAction::Export),
            ("\"copy_response\"", KeyAction::CopyResponse),
            ("\"editor\"", KeyAction::Editor),
            ("\"models\"", KeyAction::Models),
            ("\"toggle_mode\"", KeyAction::ToggleMode),
            ("\"cancel\"", KeyAction::Cancel),
            ("\"status\"", KeyAction::Status),
            ("\"help\"", KeyAction::Help),
            ("\"clear\"", KeyAction::Clear),
            ("\"exit\"", KeyAction::Exit),
            ("\"undo\"", KeyAction::Undo),
            ("\"redo\"", KeyAction::Redo),
            ("\"compact\"", KeyAction::Compact),
            ("\"none\"", KeyAction::None),
        ];

        for (json, expected) in actions {
            let parsed: KeyAction = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    // ========================================================================
    // KeybindingsConfig Tests
    // ========================================================================

    #[test]
    fn test_keybindings_config_default() {
        let config = KeybindingsConfig::default();

        // Verify default bindings exist
        assert_eq!(
            config.bindings.get("ctrl-x n"),
            Some(&KeyAction::NewSession)
        );
        assert_eq!(
            config.bindings.get("ctrl-x l"),
            Some(&KeyAction::ListSessions)
        );
        assert_eq!(config.bindings.get("ctrl-x x"), Some(&KeyAction::Export));
        assert_eq!(config.bindings.get("f2"), Some(&KeyAction::Models));
        assert_eq!(config.bindings.get("tab"), Some(&KeyAction::ToggleMode));
        assert_eq!(config.bindings.get("escape"), Some(&KeyAction::Cancel));
    }

    #[test]
    fn test_keybindings_get_action() {
        let config = KeybindingsConfig::default();

        // Case-insensitive lookup
        assert_eq!(config.get_action("ctrl-x n"), Some(&KeyAction::NewSession));
        assert_eq!(config.get_action("CTRL-X N"), Some(&KeyAction::NewSession));

        // Unknown key returns None
        assert_eq!(config.get_action("unknown-key"), None);
    }

    #[test]
    fn test_keybindings_is_bound() {
        let mut config = KeybindingsConfig::default();

        // Regular binding is bound
        assert!(config.is_bound("ctrl-x n"));

        // Unknown key is not bound
        assert!(!config.is_bound("unknown-key"));

        // Explicitly disabled key (set to None) is not bound
        config
            .bindings
            .insert("disabled-key".to_string(), KeyAction::None);
        assert!(!config.is_bound("disabled-key"));
    }

    #[test]
    fn test_keybindings_get_keys_for_action() {
        let config = KeybindingsConfig::default();

        // Models has two bindings in default config
        let model_keys = config.get_keys_for_action(&KeyAction::Models);
        assert!(model_keys.contains(&&"ctrl-x m".to_string()));
        assert!(model_keys.contains(&&"f2".to_string()));
    }

    #[test]
    fn test_keybindings_get_action_or_default() {
        let config = KeybindingsConfig::default();

        // Known key returns its action
        assert_eq!(
            config.get_action_or_default("ctrl-x n"),
            KeyAction::NewSession
        );

        // Unknown key returns None action
        assert_eq!(config.get_action_or_default("unknown"), KeyAction::None);
    }

    // ========================================================================
    // HooksConfig Tests
    // ========================================================================

    #[test]
    fn test_hooks_config_default() {
        let config = HooksConfig::default();
        assert!(config.session_start.is_empty());
        assert!(config.session_end.is_empty());
        assert!(config.pre_tool_use.is_empty());
        assert!(config.post_tool_use.is_empty());
        assert!(config.user_prompt_submit.is_empty());
        assert!(config.stop.is_empty());
    }

    #[test]
    fn test_hook_entry_with_matcher() {
        let json = r#"{
            "matcher": "Write|Edit",
            "hooks": []
        }"#;

        let entry: HookEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.matcher, Some("Write|Edit".to_string()));
    }

    #[test]
    fn test_hook_command_type() {
        let json = r#"{
            "type": "command",
            "command": "echo test",
            "timeout": 30
        }"#;

        let hook: Hook = serde_json::from_str(json).unwrap();
        match hook {
            Hook::Command { command, timeout } => {
                assert_eq!(command, "echo test");
                assert_eq!(timeout, 30);
            }
            _ => panic!("Expected Command hook"),
        }
    }

    #[test]
    fn test_hook_prompt_type() {
        let json = r#"{
            "type": "prompt",
            "prompt": "Always be helpful",
            "timeout": 10
        }"#;

        let hook: Hook = serde_json::from_str(json).unwrap();
        match hook {
            Hook::Prompt { prompt, timeout } => {
                assert_eq!(prompt, "Always be helpful");
                assert_eq!(timeout, 10);
            }
            _ => panic!("Expected Prompt hook"),
        }
    }

    #[test]
    fn test_hook_default_timeouts() {
        // Command hook default timeout
        let cmd_json = r#"{"type": "command", "command": "test"}"#;
        let cmd_hook: Hook = serde_json::from_str(cmd_json).unwrap();
        match cmd_hook {
            Hook::Command { timeout, .. } => assert_eq!(timeout, 60), // default
            _ => panic!("Expected Command"),
        }

        // Prompt hook default timeout
        let prompt_json = r#"{"type": "prompt", "prompt": "test"}"#;
        let prompt_hook: Hook = serde_json::from_str(prompt_json).unwrap();
        match prompt_hook {
            Hook::Prompt { timeout, .. } => assert_eq!(timeout, 30), // default
            _ => panic!("Expected Prompt"),
        }
    }

    // ========================================================================
    // ProviderConfig Tests
    // ========================================================================

    #[test]
    fn test_provider_config_minimal() {
        let json = r#"{
            "base_url": "https://api.example.com"
        }"#;

        let config: ProviderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.base_url, "https://api.example.com");
        assert!(config.api_key.is_none());
        assert!(config.model.is_none());
        assert!(config.headers.is_empty());
    }

    #[test]
    fn test_provider_config_full() {
        let json = r#"{
            "base_url": "https://api.example.com",
            "api_key": "sk-test123",
            "model": "gpt-4",
            "headers": {"X-Custom": "value"},
            "thinking": {
                "enabled": true,
                "budget_tokens": 5000
            }
        }"#;

        let config: ProviderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.base_url, "https://api.example.com");
        assert_eq!(config.api_key, Some("sk-test123".to_string()));
        assert_eq!(config.model, Some("gpt-4".to_string()));
        assert_eq!(config.headers.get("X-Custom"), Some(&"value".to_string()));
        assert!(config.thinking.enabled);
        assert_eq!(config.thinking.budget_tokens, Some(5000));
    }

    // ========================================================================
    // AppConfig Tests
    // ========================================================================

    #[test]
    fn test_app_config_active_provider() {
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                api_key: Some("key".to_string()),
                base_url: "https://api.anthropic.com".to_string(),
                model: None,
                headers: HashMap::new(),
                thinking: ThinkingConfig::default(),
            },
        );

        let config = AppConfig {
            proxy: ProxyConfig {
                target: "anthropic".to_string(),
                ..Default::default()
            },
            providers,
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
        };

        let active = config.active_provider();
        assert!(active.is_some());
        assert_eq!(active.unwrap().api_key, Some("key".to_string()));
    }

    #[test]
    fn test_app_config_get_provider() {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                api_key: Some("openai-key".to_string()),
                base_url: "https://api.openai.com".to_string(),
                model: None,
                headers: HashMap::new(),
                thinking: ThinkingConfig::default(),
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                api_key: Some("anthropic-key".to_string()),
                base_url: "https://api.anthropic.com".to_string(),
                model: None,
                headers: HashMap::new(),
                thinking: ThinkingConfig::default(),
            },
        );

        let config = AppConfig {
            proxy: ProxyConfig::default(),
            providers,
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
        };

        assert!(config.get_provider("openai").is_some());
        assert!(config.get_provider("anthropic").is_some());
        assert!(config.get_provider("nonexistent").is_none());
    }

    #[test]
    fn test_app_config_active_provider_not_found() {
        let config = AppConfig {
            proxy: ProxyConfig {
                target: "nonexistent".to_string(),
                ..Default::default()
            },
            providers: HashMap::new(),
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
        };

        assert!(config.active_provider().is_none());
    }
}
