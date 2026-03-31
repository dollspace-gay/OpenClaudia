//! Configuration loader with environment variable substitution.
//!
//! Loads configuration from:
//! 1. Default values
//! 2. `.openclaudia/config.yaml` in project directory
//! 3. `~/.openclaudia/config.yaml` in home directory
//! 4. Environment variables with `OPENCLAUDIA_` prefix

mod guardrails;
mod hooks;
mod keybindings;
mod permissions;
mod provider;
mod proxy;
mod session;
mod vdd;

pub use guardrails::{
    BlastRadiusConfig, DiffMonitorConfig, GuardrailAction, GuardrailMode, GuardrailsConfig,
    QualityCheck, QualityGatesConfig, RunAfter,
};
pub use hooks::{Hook, HookEntry, HooksConfig};
pub use keybindings::{KeyAction, KeybindingsConfig};
pub use permissions::PermissionsConfig;
pub use provider::{ProviderConfig, ThinkingConfig};
pub use proxy::ProxyConfig;
pub use session::{SessionConfig, TokenTrackingConfig};
pub use vdd::{
    VddAdversaryConfig, VddConfig, VddMode, VddStaticAnalysis, VddThresholds, VddTracking,
};

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Shared default function used by multiple submodules.
pub(crate) fn default_true() -> bool {
    true
}

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
    #[serde(default)]
    pub permissions: PermissionsConfig,
    /// Path to enterprise managed settings file, if one was loaded.
    /// Managed settings override all user and project settings.
    #[serde(skip)]
    pub managed_settings_path: Option<PathBuf>,
}

// ==========================================================================
// Config Schema Generation (future)
// ==========================================================================
//
// To enable JSON Schema generation for config validation and IDE support,
// add the `schemars` crate to dependencies and derive `JsonSchema` on all
// config structs (AppConfig, ProxyConfig, ProviderConfig, HooksConfig, etc.).
//
// Example:
//   #[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
//   pub struct AppConfig { ... }
//
// Then expose via:
//   pub fn generate_config_schema() -> String {
//       serde_json::to_string_pretty(&schemars::schema_for!(AppConfig)).unwrap()
//   }
//
// This would allow `openclaudia config schema` to output the JSON schema
// for editor integration and config validation.

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
            permissions: PermissionsConfig::default(),
            managed_settings_path: None,
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
            permissions: PermissionsConfig::default(),
            managed_settings_path: None,
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
            permissions: PermissionsConfig::default(),
            managed_settings_path: None,
        };

        assert!(config.active_provider().is_none());
    }
}
