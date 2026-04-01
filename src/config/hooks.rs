use serde::Deserialize;

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
    /// Model hook: sends a prompt to a specific model/provider and returns
    /// the model's response as the hook result.
    #[serde(rename = "model")]
    Model {
        /// The prompt to send to the model
        prompt: String,
        /// The model identifier (e.g., "claude-3-5-haiku-20241022")
        model: String,
        /// Optional provider name (defaults to proxy target)
        #[serde(default)]
        provider: Option<String>,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
}

const fn default_timeout() -> u64 {
    60
}

const fn default_prompt_timeout() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_hook_model_type() {
        let json = r#"{
            "type": "model",
            "prompt": "Review this code",
            "model": "claude-3-5-haiku-20241022",
            "provider": "anthropic",
            "timeout": 45
        }"#;

        let hook: Hook = serde_json::from_str(json).unwrap();
        match hook {
            Hook::Model {
                prompt,
                model,
                provider,
                timeout,
            } => {
                assert_eq!(prompt, "Review this code");
                assert_eq!(model, "claude-3-5-haiku-20241022");
                assert_eq!(provider, Some("anthropic".to_string()));
                assert_eq!(timeout, 45);
            }
            _ => panic!("Expected Model hook"),
        }
    }

    #[test]
    fn test_hook_model_type_defaults() {
        let json = r#"{
            "type": "model",
            "prompt": "Validate",
            "model": "gpt-4o-mini"
        }"#;

        let hook: Hook = serde_json::from_str(json).unwrap();
        match hook {
            Hook::Model {
                provider, timeout, ..
            } => {
                assert!(provider.is_none());
                assert_eq!(timeout, 60); // default_timeout
            }
            _ => panic!("Expected Model hook"),
        }
    }
}
