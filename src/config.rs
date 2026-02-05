//! Configuration loader with environment variable substitution.
//!
//! Loads configuration from:
//! 1. Default values
//! 2. `.openclaudia/config.yaml` in project directory
//! 3. `~/.openclaudia/config.yaml` in home directory
//! 4. Environment variables with `OPENCLAUDIA_` prefix

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
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
    #[serde(default)]
    pub vdd: VddConfig,
    #[serde(default)]
    pub guardrails: GuardrailsConfig,
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

// ==========================================================================
// VDD (Verification-Driven Development) Configuration
// ==========================================================================

/// VDD operating mode
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VddMode {
    /// Adversary findings injected as context for next turn
    #[default]
    Advisory,
    /// Response held until adversary passes or loop converges
    Blocking,
}

impl fmt::Display for VddMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VddMode::Advisory => write!(f, "advisory"),
            VddMode::Blocking => write!(f, "blocking"),
        }
    }
}

/// Top-level VDD configuration
#[derive(Debug, Deserialize, Clone)]
pub struct VddConfig {
    /// Enable VDD adversarial loop (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Operating mode: advisory or blocking
    #[serde(default)]
    pub mode: VddMode,
    /// Adversary model configuration (must be different provider than builder)
    #[serde(default)]
    pub adversary: VddAdversaryConfig,
    /// Convergence thresholds
    #[serde(default)]
    pub thresholds: VddThresholds,
    /// Static analysis commands to run as part of the loop
    #[serde(default)]
    pub static_analysis: VddStaticAnalysis,
    /// Persistence and logging
    #[serde(default)]
    pub tracking: VddTracking,
}

impl Default for VddConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: VddMode::Advisory,
            adversary: VddAdversaryConfig::default(),
            thresholds: VddThresholds::default(),
            static_analysis: VddStaticAnalysis::default(),
            tracking: VddTracking::default(),
        }
    }
}

/// Adversary model configuration
#[derive(Debug, Deserialize, Clone)]
pub struct VddAdversaryConfig {
    /// Provider name (must differ from proxy.target)
    #[serde(default = "default_adversary_provider")]
    pub provider: String,
    /// Model override for adversary (uses provider default if None)
    #[serde(default)]
    pub model: Option<String>,
    /// Separate API key for adversary (falls back to provider's key if None)
    #[serde(default)]
    pub api_key: Option<String>,
    /// Temperature for adversary responses (lower = more deterministic critique)
    #[serde(default = "default_adversary_temperature")]
    pub temperature: f32,
    /// Max output tokens for adversary responses
    #[serde(default = "default_adversary_max_tokens")]
    pub max_tokens: u32,
}

fn default_adversary_provider() -> String {
    "google".to_string()
}

fn default_adversary_temperature() -> f32 {
    0.3
}

fn default_adversary_max_tokens() -> u32 {
    4096
}

impl Default for VddAdversaryConfig {
    fn default() -> Self {
        Self {
            provider: default_adversary_provider(),
            model: None,
            api_key: None,
            temperature: default_adversary_temperature(),
            max_tokens: default_adversary_max_tokens(),
        }
    }
}

/// Convergence and termination thresholds
#[derive(Debug, Deserialize, Clone)]
pub struct VddThresholds {
    /// Maximum adversarial loop iterations before forced termination
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// False positive rate threshold for confabulation detection (0.0-1.0)
    #[serde(default = "default_fp_threshold")]
    pub false_positive_rate: f32,
    /// Minimum iterations before checking confabulation threshold
    #[serde(default = "default_min_iterations")]
    pub min_iterations: u32,
}

fn default_max_iterations() -> u32 {
    5
}

fn default_fp_threshold() -> f32 {
    0.75
}

fn default_min_iterations() -> u32 {
    2
}

impl Default for VddThresholds {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            false_positive_rate: default_fp_threshold(),
            min_iterations: default_min_iterations(),
        }
    }
}

/// Static analysis commands run as part of the adversarial loop
#[derive(Debug, Deserialize, Clone)]
pub struct VddStaticAnalysis {
    /// Enable static analysis in the loop
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Auto-detect project language and use default analysis commands
    /// Only used when `commands` is empty. Default: true.
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    /// Shell commands to run (exit code 0 = pass)
    /// If empty and auto_detect is true, commands are auto-detected from project type.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Timeout per command in seconds
    #[serde(default = "default_analysis_timeout")]
    pub timeout_seconds: u64,
}

fn default_analysis_timeout() -> u64 {
    120
}

impl Default for VddStaticAnalysis {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_detect: true,
            commands: Vec::new(),
            timeout_seconds: default_analysis_timeout(),
        }
    }
}

/// VDD session persistence and logging
#[derive(Debug, Deserialize, Clone)]
pub struct VddTracking {
    /// Persist VDD session data to disk
    #[serde(default = "default_true")]
    pub persist: bool,
    /// Directory for VDD session data
    #[serde(default = "default_vdd_path")]
    pub path: PathBuf,
    /// Log full adversary responses (verbose)
    #[serde(default = "default_true")]
    pub log_adversary_responses: bool,
}

fn default_vdd_path() -> PathBuf {
    PathBuf::from(".openclaudia/vdd")
}

impl Default for VddTracking {
    fn default() -> Self {
        Self {
            persist: true,
            path: default_vdd_path(),
            log_adversary_responses: true,
        }
    }
}

impl VddConfig {
    /// Validate VDD configuration. Returns error message if invalid.
    pub fn validate(&self, builder_provider: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        // Adversary must use different provider than builder
        if self.adversary.provider.to_lowercase() == builder_provider.to_lowercase() {
            return Err(format!(
                "VDD adversary provider '{}' must differ from builder provider '{}'. \
                 Using the same model to review its own output defeats the purpose of adversarial review.",
                self.adversary.provider, builder_provider
            ));
        }

        // Threshold validation
        if self.thresholds.false_positive_rate < 0.0 || self.thresholds.false_positive_rate > 1.0 {
            return Err(format!(
                "VDD false_positive_rate must be between 0.0 and 1.0, got {}",
                self.thresholds.false_positive_rate
            ));
        }

        if self.thresholds.min_iterations > self.thresholds.max_iterations {
            return Err(format!(
                "VDD min_iterations ({}) cannot exceed max_iterations ({})",
                self.thresholds.min_iterations, self.thresholds.max_iterations
            ));
        }

        if self.thresholds.max_iterations == 0 {
            return Err("VDD max_iterations must be at least 1".to_string());
        }

        // Temperature validation
        if self.adversary.temperature < 0.0 || self.adversary.temperature > 2.0 {
            return Err(format!(
                "VDD adversary temperature must be between 0.0 and 2.0, got {}",
                self.adversary.temperature
            ));
        }

        Ok(())
    }
}

// ==========================================================================
// Guardrails Configuration
// ==========================================================================

/// Top-level guardrails configuration
#[derive(Debug, Deserialize, Clone, Default)]
pub struct GuardrailsConfig {
    /// Blast radius limiting: constrain file access per request
    #[serde(default)]
    pub blast_radius: Option<BlastRadiusConfig>,
    /// Diff size monitoring: flag oversized changes
    #[serde(default)]
    pub diff_monitor: Option<DiffMonitorConfig>,
    /// Automated code quality gates
    #[serde(default)]
    pub quality_gates: Option<QualityGatesConfig>,
}

/// Guardrail enforcement mode
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GuardrailMode {
    /// Block operations that violate the guardrail
    Strict,
    /// Warn but allow operations
    #[default]
    Advisory,
}

impl fmt::Display for GuardrailMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Strict => write!(f, "strict"),
            Self::Advisory => write!(f, "advisory"),
        }
    }
}

/// Action to take when a guardrail threshold is exceeded
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailAction {
    /// Log a warning
    #[default]
    Warn,
    /// Block the operation
    Block,
    /// Inject findings into context for the model to address
    InjectFindings,
}

impl fmt::Display for GuardrailAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Warn => write!(f, "warn"),
            Self::Block => write!(f, "block"),
            Self::InjectFindings => write!(f, "inject_findings"),
        }
    }
}

/// When to run quality gate checks
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunAfter {
    /// After every file edit
    EveryEdit,
    /// After every tool-use turn
    #[default]
    EveryTurn,
    /// Only before git commit
    OnCommit,
}

impl fmt::Display for RunAfter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EveryEdit => write!(f, "every_edit"),
            Self::EveryTurn => write!(f, "every_turn"),
            Self::OnCommit => write!(f, "on_commit"),
        }
    }
}

/// Blast radius limiting configuration
#[derive(Debug, Deserialize, Clone)]
pub struct BlastRadiusConfig {
    /// Enable blast radius limiting
    #[serde(default)]
    pub enabled: bool,
    /// Enforcement mode: strict (block) or advisory (warn)
    #[serde(default)]
    pub mode: GuardrailMode,
    /// Glob patterns for allowed file paths (empty = all allowed)
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Glob patterns for denied file paths (takes priority over allowed)
    #[serde(default)]
    pub denied_paths: Vec<String>,
    /// Maximum files the agent can modify per turn (0 = unlimited)
    #[serde(default)]
    pub max_files_per_turn: u32,
}

impl Default for BlastRadiusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: GuardrailMode::Advisory,
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            max_files_per_turn: 0,
        }
    }
}

/// Diff size monitoring configuration
#[derive(Debug, Deserialize, Clone)]
pub struct DiffMonitorConfig {
    /// Enable diff monitoring
    #[serde(default)]
    pub enabled: bool,
    /// Maximum total lines changed before warning (0 = unlimited)
    #[serde(default = "default_max_lines")]
    pub max_lines_changed: u32,
    /// Maximum files changed before warning (0 = unlimited)
    #[serde(default = "default_max_files")]
    pub max_files_changed: u32,
    /// Action when thresholds exceeded
    #[serde(default)]
    pub action: GuardrailAction,
}

fn default_max_lines() -> u32 {
    500
}

fn default_max_files() -> u32 {
    10
}

impl Default for DiffMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_lines_changed: default_max_lines(),
            max_files_changed: default_max_files(),
            action: GuardrailAction::Warn,
        }
    }
}

/// Quality gates configuration
#[derive(Debug, Deserialize, Clone)]
pub struct QualityGatesConfig {
    /// Enable quality gates
    #[serde(default)]
    pub enabled: bool,
    /// When to run checks
    #[serde(default)]
    pub run_after: RunAfter,
    /// Action when required checks fail
    #[serde(default)]
    pub fail_action: GuardrailAction,
    /// List of quality checks to run
    #[serde(default)]
    pub checks: Vec<QualityCheck>,
    /// Timeout per command in seconds
    #[serde(default = "default_quality_timeout")]
    pub timeout_seconds: u64,
}

fn default_quality_timeout() -> u64 {
    120
}

impl Default for QualityGatesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            run_after: RunAfter::EveryTurn,
            fail_action: GuardrailAction::Warn,
            checks: Vec::new(),
            timeout_seconds: default_quality_timeout(),
        }
    }
}

/// A single quality check definition
#[derive(Debug, Deserialize, Clone)]
pub struct QualityCheck {
    /// Human-readable name for the check
    pub name: String,
    /// Shell command to run (exit code 0 = pass)
    pub command: String,
    /// Whether failure should be treated as an error
    #[serde(default = "default_true")]
    pub required: bool,
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
    #[serde(default)]
    pub pre_adversary_review: Vec<HookEntry>,
    #[serde(default)]
    pub post_adversary_review: Vec<HookEntry>,
    #[serde(default)]
    pub vdd_conflict: Vec<HookEntry>,
    #[serde(default)]
    pub vdd_converged: Vec<HookEntry>,
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
            vdd: VddConfig::default(),
            guardrails: GuardrailsConfig::default(),
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
            vdd: VddConfig::default(),
            guardrails: GuardrailsConfig::default(),
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
            vdd: VddConfig::default(),
            guardrails: GuardrailsConfig::default(),
        };

        assert!(config.active_provider().is_none());
    }

    // ========================================================================
    // VDD Configuration Tests
    // ========================================================================

    #[test]
    fn test_vdd_config_default_disabled() {
        let config = VddConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.mode, VddMode::Advisory);
        assert_eq!(config.adversary.provider, "google");
        assert!(config.adversary.model.is_none());
        assert_eq!(config.adversary.temperature, 0.3);
        assert_eq!(config.adversary.max_tokens, 4096);
        assert_eq!(config.thresholds.max_iterations, 5);
        assert_eq!(config.thresholds.false_positive_rate, 0.75);
        assert_eq!(config.thresholds.min_iterations, 2);
        assert!(config.static_analysis.enabled);
        assert!(config.static_analysis.commands.is_empty());
        assert_eq!(config.static_analysis.timeout_seconds, 120);
        assert!(config.tracking.persist);
        assert_eq!(config.tracking.path, PathBuf::from(".openclaudia/vdd"));
    }

    #[test]
    fn test_vdd_config_serde_full() {
        let json = r#"{
            "enabled": true,
            "mode": "blocking",
            "adversary": {
                "provider": "google",
                "model": "gemini-2.5-pro",
                "temperature": 0.2,
                "max_tokens": 8192
            },
            "thresholds": {
                "max_iterations": 8,
                "false_positive_rate": 0.80,
                "min_iterations": 3
            },
            "static_analysis": {
                "enabled": true,
                "commands": ["cargo clippy -- -D warnings", "cargo test"],
                "timeout_seconds": 180
            },
            "tracking": {
                "persist": true,
                "path": "/custom/vdd",
                "log_adversary_responses": false
            }
        }"#;

        let config: VddConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.mode, VddMode::Blocking);
        assert_eq!(config.adversary.provider, "google");
        assert_eq!(config.adversary.model, Some("gemini-2.5-pro".to_string()));
        assert_eq!(config.adversary.temperature, 0.2);
        assert_eq!(config.adversary.max_tokens, 8192);
        assert_eq!(config.thresholds.max_iterations, 8);
        assert_eq!(config.thresholds.false_positive_rate, 0.80);
        assert_eq!(config.thresholds.min_iterations, 3);
        assert_eq!(config.static_analysis.commands.len(), 2);
        assert_eq!(config.static_analysis.timeout_seconds, 180);
        assert!(!config.tracking.log_adversary_responses);
    }

    #[test]
    fn test_vdd_mode_display() {
        assert_eq!(format!("{}", VddMode::Advisory), "advisory");
        assert_eq!(format!("{}", VddMode::Blocking), "blocking");
    }

    #[test]
    fn test_vdd_validate_same_provider_rejected() {
        let config = VddConfig {
            enabled: true,
            adversary: VddAdversaryConfig {
                provider: "anthropic".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must differ"));
    }

    #[test]
    fn test_vdd_validate_same_provider_case_insensitive() {
        let config = VddConfig {
            enabled: true,
            adversary: VddAdversaryConfig {
                provider: "Anthropic".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
    }

    #[test]
    fn test_vdd_validate_different_provider_ok() {
        let config = VddConfig {
            enabled: true,
            adversary: VddAdversaryConfig {
                provider: "google".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate("anthropic").is_ok());
    }

    #[test]
    fn test_vdd_validate_disabled_skips_checks() {
        // Even with same provider, disabled VDD passes validation
        let config = VddConfig {
            enabled: false,
            adversary: VddAdversaryConfig {
                provider: "anthropic".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate("anthropic").is_ok());
    }

    #[test]
    fn test_vdd_validate_bad_fp_rate() {
        let config = VddConfig {
            enabled: true,
            thresholds: VddThresholds {
                false_positive_rate: 1.5,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("between 0.0 and 1.0"));
    }

    #[test]
    fn test_vdd_validate_min_exceeds_max() {
        let config = VddConfig {
            enabled: true,
            thresholds: VddThresholds {
                min_iterations: 10,
                max_iterations: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot exceed"));
    }

    #[test]
    fn test_vdd_validate_zero_max_iterations() {
        let config = VddConfig {
            enabled: true,
            thresholds: VddThresholds {
                max_iterations: 0,
                min_iterations: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least 1"));
    }

    #[test]
    fn test_vdd_validate_bad_temperature() {
        let config = VddConfig {
            enabled: true,
            adversary: VddAdversaryConfig {
                temperature: 3.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate("anthropic");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("temperature"));
    }
}
