use serde::Deserialize;
use std::path::PathBuf;

use super::default_true;

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

const fn default_warn_threshold() -> f32 {
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

const fn default_timeout_minutes() -> u64 {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
