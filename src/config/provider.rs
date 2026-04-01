use serde::Deserialize;
use std::collections::HashMap;

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
    /// Reasoning effort level for `OpenAI` o1/o3: "low", "medium", "high"
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

const fn default_thinking_enabled() -> bool {
    true
}

/// Provider configuration (Anthropic, `OpenAI`, Google, etc.)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
