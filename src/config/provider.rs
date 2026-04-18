use serde::Deserialize;
use std::collections::HashMap;

// Re-export redaction/validation helpers from their home next to the
// `ApiKey` newtype in `providers::api_key`. Keeps existing
// `crate::config::provider::redact_api_key` call sites working while the
// newtype itself does the load-bearing work. See crosslink #256.
pub use crate::providers::api_key::{redact_api_key, validate_api_key, ApiKey};

/// Thinking/reasoning mode configuration
#[derive(Debug, Deserialize, Clone)]
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
    /// Reasoning effort level for `OpenAI` o1/o3: "low", "medium", "high"
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

const fn default_thinking_enabled() -> bool {
    true
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            enabled: default_thinking_enabled(),
            budget_tokens: None,
            preserve_across_turns: false,
            reasoning_effort: None,
        }
    }
}

/// Provider configuration (Anthropic, `OpenAI`, Google, etc.)
///
/// `api_key` is an [`ApiKey`] newtype whose own `Debug`/`Display` redact
/// the value and whose `Deserialize` impl validates the structure
/// (rejects empty / CRLF / non-ASCII). We keep the derived `Debug` on
/// this struct because the redaction guarantee is now structural on the
/// field type — one less place to regress. See crosslink #256.
#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: Option<ApiKey>,
    pub base_url: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub thinking: ThinkingConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_config_default() {
        // Default and serde deserialization now both return enabled=true
        let config = ThinkingConfig::default();
        assert!(config.enabled);
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
        assert_eq!(
            config.api_key.as_ref().map(ApiKey::as_str),
            Some("sk-test123")
        );
        assert_eq!(config.model, Some("gpt-4".to_string()));
        assert_eq!(config.headers.get("X-Custom"), Some(&"value".to_string()));
        assert!(config.thinking.enabled);
        assert_eq!(config.thinking.budget_tokens, Some(5000));
    }

    // --- Regression tests for crosslink #256 ---

    #[test]
    fn provider_config_debug_does_not_leak_key() {
        let cfg = ProviderConfig {
            api_key: Some(
                ApiKey::try_from_string("sk-ant-api03-SECRET_VALUE_HERE_XYZ".to_string())
                    .expect("valid test key"),
            ),
            base_url: "https://api.anthropic.com".to_string(),
            model: None,
            headers: HashMap::new(),
            thinking: ThinkingConfig::default(),
        };
        let s = format!("{cfg:?}");
        assert!(!s.contains("SECRET_VALUE_HERE"), "Debug leaked middle: {s}");
        assert!(
            !s.contains("sk-ant-api03-SECRET"),
            "Debug leaked prefix-middle: {s}"
        );
        assert!(
            s.contains("sk-a") || s.contains("…"),
            "no redaction fingerprint: {s}"
        );
    }

    #[test]
    fn provider_config_rejects_crlf_api_key_at_deserialize() {
        let json = r#"{
            "base_url": "https://api.example.com",
            "api_key": "sk-legit\r\nX-Injected: evil"
        }"#;
        let result: Result<ProviderConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "CRLF api_key must fail deserialize");
    }

    #[test]
    fn provider_config_rejects_empty_api_key_at_deserialize() {
        let json = r#"{
            "base_url": "https://api.example.com",
            "api_key": ""
        }"#;
        let result: Result<ProviderConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "empty api_key must fail deserialize");
    }

}
