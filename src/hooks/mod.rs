//! Hook Engine - Executes hooks at key moments in the agent lifecycle.
//!
//! Supports 12 event types and two hook mechanisms:
//! - Command hooks: Execute shell commands with JSON stdin/stdout
//! - Prompt hooks: Inject prompts into the conversation
//!
//! Also supports loading hooks from Claude Code's .claude/settings.json
//! for compatibility with existing Claude Code hook configurations.
//!
//! Exit codes:
//! - 0: Success (allow)
//! - 2: Block the action

pub mod claude_compat;
pub mod merge;

// Re-export everything public from submodules
pub use claude_compat::{
    load_claude_code_hooks, load_claude_code_hooks_layered, load_claude_settings,
    ClaudeCodeHook, ClaudeCodeHookEntry, ClaudeCodeSettings, LayeredSettings,
};
pub use merge::merge_hooks_config;

use crate::config::{Hook, HookEntry, HooksConfig};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use std::process::Stdio;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// All hook event types supported by OpenClaudia
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Fired when a new session starts
    SessionStart,
    /// Fired when a session ends
    SessionEnd,
    /// Fired before a tool is executed
    PreToolUse,
    /// Fired after a tool executes successfully
    PostToolUse,
    /// Fired after a tool execution fails
    PostToolUseFailure,
    /// Fired when user submits a prompt
    UserPromptSubmit,
    /// Fired when the agent stops
    Stop,
    /// Fired when a subagent starts
    SubagentStart,
    /// Fired when a subagent stops
    SubagentStop,
    /// Fired before context compaction
    PreCompact,
    /// Fired when a permission is requested
    PermissionRequest,
    /// Fired for notifications
    Notification,
    /// Fired before sending builder output to adversary (VDD)
    PreAdversaryReview,
    /// Fired after adversary returns review (VDD)
    PostAdversaryReview,
    /// Fired when adversary finds genuine issues (VDD)
    VddConflict,
    /// Fired when adversary reaches confabulation threshold (VDD)
    VddConverged,
}

impl HookEvent {
    /// Get the config field name for this event
    pub fn config_key(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::PostToolUseFailure => "post_tool_use_failure",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::Stop => "stop",
            HookEvent::SubagentStart => "subagent_start",
            HookEvent::SubagentStop => "subagent_stop",
            HookEvent::PreCompact => "pre_compact",
            HookEvent::PermissionRequest => "permission_request",
            HookEvent::Notification => "notification",
            HookEvent::PreAdversaryReview => "pre_adversary_review",
            HookEvent::PostAdversaryReview => "post_adversary_review",
            HookEvent::VddConflict => "vdd_conflict",
            HookEvent::VddConverged => "vdd_converged",
        }
    }

    /// Parse from Claude Code's PascalCase event name
    pub fn from_claude_code_name(name: &str) -> Option<Self> {
        match name {
            "PreToolUse" => Some(HookEvent::PreToolUse),
            "PostToolUse" => Some(HookEvent::PostToolUse),
            "PostToolUseFailure" => Some(HookEvent::PostToolUseFailure),
            "UserPromptSubmit" => Some(HookEvent::UserPromptSubmit),
            "Stop" => Some(HookEvent::Stop),
            "SubagentStart" => Some(HookEvent::SubagentStart),
            "SubagentStop" => Some(HookEvent::SubagentStop),
            "PreCompact" => Some(HookEvent::PreCompact),
            "Notification" => Some(HookEvent::Notification),
            // Claude Code doesn't have these but we support them
            "SessionStart" => Some(HookEvent::SessionStart),
            "SessionEnd" => Some(HookEvent::SessionEnd),
            "PermissionRequest" => Some(HookEvent::PermissionRequest),
            "PreAdversaryReview" => Some(HookEvent::PreAdversaryReview),
            "PostAdversaryReview" => Some(HookEvent::PostAdversaryReview),
            "VddConflict" => Some(HookEvent::VddConflict),
            "VddConverged" => Some(HookEvent::VddConverged),
            _ => None,
        }
    }
}

impl HooksConfig {
    /// Check if the hooks config is empty (no hooks defined)
    pub fn is_empty(&self) -> bool {
        self.session_start.is_empty()
            && self.session_end.is_empty()
            && self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.user_prompt_submit.is_empty()
            && self.stop.is_empty()
            && self.pre_adversary_review.is_empty()
            && self.post_adversary_review.is_empty()
            && self.vdd_conflict.is_empty()
            && self.vdd_converged.is_empty()
    }
}

/// Input provided to hooks via stdin
#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    /// The event type that triggered this hook
    pub event: HookEvent,
    /// Current working directory
    pub cwd: String,
    /// Session ID if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Tool name for tool-related events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool input for tool-related events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    /// User prompt for UserPromptSubmit event
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Additional context data
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl HookInput {
    pub fn new(event: HookEvent) -> Self {
        Self {
            event,
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            session_id: None,
            tool_name: None,
            tool_input: None,
            prompt: None,
            extra: HashMap::new(),
        }
    }

    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    pub fn with_tool(mut self, name: impl Into<String>, input: Value) -> Self {
        self.tool_name = Some(name.into());
        self.tool_input = Some(input);
        self
    }

    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }
}

/// Output from a hook execution
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HookOutput {
    /// Decision: "allow", "deny", or "ask"
    pub decision: Option<String>,
    /// Reason for the decision
    pub reason: Option<String>,
    /// System message to inject
    #[serde(rename = "systemMessage")]
    pub system_message: Option<String>,
    /// Modified prompt (for UserPromptSubmit)
    pub prompt: Option<String>,
    /// Additional context from hook (plain text output or hookSpecificOutput.additionalContext)
    #[serde(rename = "additionalContext")]
    pub additional_context: Option<String>,
    /// Additional data from the hook
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Result of running hooks
#[derive(Debug, Clone)]
pub struct HookResult {
    /// Whether the action should be allowed
    pub allowed: bool,
    /// Combined outputs from all hooks
    pub outputs: Vec<HookOutput>,
    /// Any errors that occurred
    pub errors: Vec<HookError>,
}

impl HookResult {
    pub fn allowed() -> Self {
        Self {
            allowed: true,
            outputs: vec![],
            errors: vec![],
        }
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            outputs: vec![HookOutput {
                decision: Some("deny".to_string()),
                reason: Some(reason.into()),
                ..Default::default()
            }],
            errors: vec![],
        }
    }

    /// Get all system messages from hook outputs
    pub fn system_messages(&self) -> Vec<&str> {
        self.outputs
            .iter()
            .filter_map(|o| o.system_message.as_deref())
            .collect()
    }

    /// Get modified prompt if any hook provided one
    pub fn modified_prompt(&self) -> Option<&str> {
        self.outputs.iter().find_map(|o| o.prompt.as_deref())
    }
}

/// Errors that can occur during hook execution
#[derive(Error, Debug, Clone)]
pub enum HookError {
    #[error("Hook timed out after {0} seconds")]
    Timeout(u64),

    #[error("Hook command failed: {0}")]
    CommandFailed(String),

    #[error("Hook output parse error: {0}")]
    ParseError(String),

    #[error("Hook blocked action: {0}")]
    Blocked(String),

    #[error("Invalid matcher regex: {0}")]
    InvalidMatcher(String),
}

/// Callback for executing model hooks via a provider adapter.
/// This avoids a direct dependency from hooks.rs on providers.rs.
pub type ModelHookCallback = Box<
    dyn Fn(
            String,
            String,
            Option<String>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// The hook engine that executes hooks
#[derive(Clone)]
pub struct HookEngine {
    config: HooksConfig,
    /// Optional callback for executing model hooks.
    /// Takes (prompt, model, provider) and returns the model's response text.
    model_hook_callback: Option<Arc<ModelHookCallback>>,
}

impl HookEngine {
    pub fn new(config: HooksConfig) -> Self {
        Self {
            config,
            model_hook_callback: None,
        }
    }

    /// Set a callback for executing model hooks through a provider adapter
    pub fn with_model_callback(mut self, callback: ModelHookCallback) -> Self {
        self.model_hook_callback = Some(Arc::new(callback));
        self
    }

    /// Run all matching hooks for an event
    pub async fn run(&self, event: HookEvent, input: &HookInput) -> HookResult {
        let entries = self.get_entries_for_event(event);

        if entries.is_empty() {
            return HookResult::allowed();
        }

        let matcher_context = self.get_matcher_context(input);

        // Filter entries by matcher
        let matching_entries: Vec<&HookEntry> = entries
            .iter()
            .filter(|entry| self.matches_entry(entry, &matcher_context))
            .collect();

        if matching_entries.is_empty() {
            return HookResult::allowed();
        }

        info!(
            event = ?event,
            count = matching_entries.len(),
            "Running hooks"
        );

        // Collect all hooks to run
        let mut hooks_to_run: Vec<(&Hook, u64)> = Vec::new();
        for entry in &matching_entries {
            for hook in &entry.hooks {
                let timeout_secs = match hook {
                    Hook::Command { timeout, .. } => *timeout,
                    Hook::Prompt { timeout, .. } => *timeout,
                    Hook::Model { timeout, .. } => *timeout,
                };
                hooks_to_run.push((hook, timeout_secs));
            }
        }

        // Run hooks in parallel
        let input_json = serde_json::to_string(input).unwrap_or_default();
        let futures: Vec<_> = hooks_to_run
            .iter()
            .map(|(hook, timeout_secs)| self.run_hook(hook, &input_json, *timeout_secs))
            .collect();

        let results = futures::future::join_all(futures).await;

        // Combine results
        let mut hook_result = HookResult::allowed();
        for result in results {
            match result {
                Ok((output, exit_code)) => {
                    // Exit code 2 means block
                    if exit_code == 2 {
                        hook_result.allowed = false;
                        let reason = output
                            .reason
                            .clone()
                            .unwrap_or_else(|| "Hook blocked action".to_string());
                        warn!(reason = %reason, "Hook blocked action");
                    }
                    // Check decision field
                    if let Some(decision) = &output.decision {
                        if decision == "deny" || decision == "block" {
                            hook_result.allowed = false;
                        }
                    }
                    hook_result.outputs.push(output);
                }
                Err(e) => {
                    error!(error = %e, "Hook execution failed");
                    hook_result.errors.push(e);
                }
            }
        }

        hook_result
    }

    /// Get hook entries for a specific event
    fn get_entries_for_event(&self, event: HookEvent) -> &[HookEntry] {
        match event {
            HookEvent::SessionStart => &self.config.session_start,
            HookEvent::SessionEnd => &self.config.session_end,
            HookEvent::PreToolUse => &self.config.pre_tool_use,
            HookEvent::PostToolUse => &self.config.post_tool_use,
            HookEvent::UserPromptSubmit => &self.config.user_prompt_submit,
            HookEvent::Stop => &self.config.stop,
            // Events not yet in config (return empty)
            HookEvent::PostToolUseFailure
            | HookEvent::SubagentStart
            | HookEvent::SubagentStop
            | HookEvent::PreCompact
            | HookEvent::PermissionRequest
            | HookEvent::Notification => &[],
            // VDD events
            HookEvent::PreAdversaryReview => &self.config.pre_adversary_review,
            HookEvent::PostAdversaryReview => &self.config.post_adversary_review,
            HookEvent::VddConflict => &self.config.vdd_conflict,
            HookEvent::VddConverged => &self.config.vdd_converged,
        }
    }

    /// Get the string to match against for this input
    fn get_matcher_context(&self, input: &HookInput) -> String {
        // For tool events, match against tool name
        if let Some(tool_name) = &input.tool_name {
            return tool_name.clone();
        }
        // For other events, match against prompt or event name
        if let Some(prompt) = &input.prompt {
            return prompt.clone();
        }
        input.event.config_key().to_string()
    }

    /// Check if a hook entry matches the current context
    fn matches_entry(&self, entry: &HookEntry, context: &str) -> bool {
        match &entry.matcher {
            None => true, // No matcher means always match
            Some(pattern) => match self.validate_and_match(pattern, context) {
                Ok(matched) => matched,
                Err(e) => {
                    warn!(pattern = %pattern, error = %e, "Matcher validation failed");
                    false
                }
            },
        }
    }

    /// Validate regex pattern and check for match
    fn validate_and_match(&self, pattern: &str, context: &str) -> Result<bool, HookError> {
        // Check for invalid patterns
        if pattern.is_empty() {
            return Err(HookError::InvalidMatcher("Empty pattern".to_string()));
        }

        match Regex::new(pattern) {
            Ok(re) => Ok(re.is_match(context)),
            Err(e) => Err(HookError::InvalidMatcher(e.to_string())),
        }
    }

    /// Parse hook output — matches Claude Code behavior:
    /// - Empty output → default
    /// - Starts with '{' → try JSON parse, fall back to plain text on failure
    /// - Anything else → treat as plain text (additionalContext / system-reminder)
    fn parse_hook_output(stdout: &str) -> Result<HookOutput, HookError> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(HookOutput::default());
        }

        // Only try JSON parse if it looks like JSON (starts with '{')
        if trimmed.starts_with('{') {
            match serde_json::from_str(trimmed) {
                Ok(output) => return Ok(output),
                Err(_) => {
                    // Invalid JSON that starts with { — treat as plain text
                    debug!("Hook output starts with '{{' but is not valid JSON, treating as plain text");
                }
            }
        }

        // Plain text output — wrap as additionalContext (like Claude Code does)
        Ok(HookOutput {
            additional_context: Some(trimmed.to_string()),
            ..Default::default()
        })
    }

    /// Check if an action should be blocked based on hook result
    pub fn check_blocked(result: &HookResult) -> Result<(), HookError> {
        if !result.allowed {
            let reasons: Vec<String> = result
                .outputs
                .iter()
                .filter_map(|o| o.reason.clone())
                .collect();
            let reason = if reasons.is_empty() {
                "Action blocked by hook".to_string()
            } else {
                reasons.join("; ")
            };
            Err(HookError::Blocked(reason))
        } else {
            Ok(())
        }
    }

    /// Fire a notification event with type and data
    pub async fn fire_notification(&self, notification_type: &str, data: Value) -> Vec<HookResult> {
        let extra = json!({
            "notification_type": notification_type,
            "data": data,
        });

        let mut input = HookInput::new(HookEvent::Notification);
        for (k, v) in extra.as_object().unwrap() {
            input = input.with_extra(k.clone(), v.clone());
        }

        debug!(
            notification_type = %notification_type,
            "Firing notification hook"
        );

        vec![self.run(HookEvent::Notification, &input).await]
    }

    /// Run a single hook
    async fn run_hook(
        &self,
        hook: &Hook,
        input_json: &str,
        timeout_secs: u64,
    ) -> Result<(HookOutput, i32), HookError> {
        match hook {
            Hook::Command { command, .. } => {
                self.run_command_hook(command, input_json, timeout_secs)
                    .await
            }
            Hook::Prompt { prompt, .. } => {
                // Prompt hooks just return the prompt as system message
                Ok((
                    HookOutput {
                        system_message: Some(prompt.clone()),
                        ..Default::default()
                    },
                    0,
                ))
            }
            Hook::Model {
                prompt,
                model,
                provider,
                ..
            } => {
                self.run_model_hook(prompt, model, provider.as_deref(), timeout_secs)
                    .await
            }
        }
    }

    /// Execute a command hook
    async fn run_command_hook(
        &self,
        command: &str,
        input_json: &str,
        timeout_secs: u64,
    ) -> Result<(HookOutput, i32), HookError> {
        debug!(command = %command, "Running command hook");

        // Determine shell based on platform
        let (shell, shell_arg) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };

        let mut child = Command::new(shell)
            .arg(shell_arg)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(
                "CLAUDE_PROJECT_DIR",
                std::env::current_dir().unwrap_or_default(),
            )
            .spawn()
            .map_err(|e| HookError::CommandFailed(e.to_string()))?;

        // Write input to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(input_json.as_bytes()).await {
                warn!("Failed to write hook input to stdin: {}", e);
            }
        }

        // Wait for completion with timeout
        let result = timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !stderr.is_empty() {
                    debug!(stderr = %stderr, "Hook stderr");
                }

                // Parse JSON output if present
                let hook_output = match Self::parse_hook_output(&stdout) {
                    Ok(output) => output,
                    Err(e) => {
                        debug!(error = %e, "Hook output is not JSON (treating as plain text)");
                        HookOutput::default()
                    }
                };

                Ok((hook_output, exit_code))
            }
            Ok(Err(e)) => Err(HookError::CommandFailed(e.to_string())),
            Err(_) => Err(HookError::Timeout(timeout_secs)),
        }
    }

    /// Execute a model hook by sending a prompt to a specified model/provider
    async fn run_model_hook(
        &self,
        prompt: &str,
        model: &str,
        provider: Option<&str>,
        timeout_secs: u64,
    ) -> Result<(HookOutput, i32), HookError> {
        debug!(
            model = %model,
            provider = ?provider,
            "Running model hook"
        );

        let callback = self.model_hook_callback.as_ref().ok_or_else(|| {
            HookError::CommandFailed(
                "Model hook requires a model callback to be configured on the HookEngine"
                    .to_string(),
            )
        })?;

        let future = callback(
            prompt.to_string(),
            model.to_string(),
            provider.map(String::from),
        );

        match timeout(Duration::from_secs(timeout_secs), future).await {
            Ok(Ok(response)) => Ok((
                HookOutput {
                    system_message: Some(response),
                    ..Default::default()
                },
                0,
            )),
            Ok(Err(e)) => Err(HookError::CommandFailed(format!("Model hook error: {}", e))),
            Err(_) => Err(HookError::Timeout(timeout_secs)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HooksConfig;
    use merge::merge_claude_hooks;

    #[test]
    fn test_hook_event_config_keys() {
        assert_eq!(HookEvent::SessionStart.config_key(), "session_start");
        assert_eq!(HookEvent::PreToolUse.config_key(), "pre_tool_use");
        assert_eq!(
            HookEvent::UserPromptSubmit.config_key(),
            "user_prompt_submit"
        );
    }

    #[test]
    fn test_hook_input_builder() {
        let input = HookInput::new(HookEvent::PreToolUse)
            .with_session_id("test-session")
            .with_tool("Write", serde_json::json!({"path": "/tmp/test"}));

        assert_eq!(input.event, HookEvent::PreToolUse);
        assert_eq!(input.session_id, Some("test-session".to_string()));
        assert_eq!(input.tool_name, Some("Write".to_string()));
    }

    #[test]
    fn test_hook_result_system_messages() {
        let result = HookResult {
            allowed: true,
            outputs: vec![
                HookOutput {
                    system_message: Some("Message 1".to_string()),
                    ..Default::default()
                },
                HookOutput {
                    system_message: Some("Message 2".to_string()),
                    ..Default::default()
                },
            ],
            errors: vec![],
        };

        let messages = result.system_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], "Message 1");
        assert_eq!(messages[1], "Message 2");
    }

    #[tokio::test]
    async fn test_empty_hooks_config() {
        let engine = HookEngine::new(HooksConfig::default());
        let input = HookInput::new(HookEvent::SessionStart);
        let result = engine.run(HookEvent::SessionStart, &input).await;

        assert!(result.allowed);
        assert!(result.outputs.is_empty());
    }

    // ========================================================================
    // Claude Code Compatibility Tests
    // ========================================================================

    #[test]
    fn test_hook_event_from_claude_code_name() {
        // Test all Claude Code event names
        assert_eq!(
            HookEvent::from_claude_code_name("PreToolUse"),
            Some(HookEvent::PreToolUse)
        );
        assert_eq!(
            HookEvent::from_claude_code_name("PostToolUse"),
            Some(HookEvent::PostToolUse)
        );
        assert_eq!(
            HookEvent::from_claude_code_name("UserPromptSubmit"),
            Some(HookEvent::UserPromptSubmit)
        );
        assert_eq!(
            HookEvent::from_claude_code_name("PreCompact"),
            Some(HookEvent::PreCompact)
        );
        assert_eq!(
            HookEvent::from_claude_code_name("Stop"),
            Some(HookEvent::Stop)
        );
        assert_eq!(
            HookEvent::from_claude_code_name("SubagentStart"),
            Some(HookEvent::SubagentStart)
        );

        // Unknown events should return None
        assert_eq!(HookEvent::from_claude_code_name("UnknownEvent"), None);
        assert_eq!(HookEvent::from_claude_code_name(""), None);
    }

    #[test]
    fn test_parse_claude_code_settings() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Write|Edit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python validate.py"
                            }
                        ]
                    }
                ],
                "PreCompact": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "bd prime",
                                "timeout": 30
                            }
                        ]
                    }
                ]
            }
        }"#;

        let settings: ClaudeCodeSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.hooks.len(), 2);
        assert!(settings.hooks.contains_key("PreToolUse"));
        assert!(settings.hooks.contains_key("PreCompact"));

        // Check PreToolUse entry
        let pre_tool = &settings.hooks["PreToolUse"][0];
        assert_eq!(pre_tool.matcher, Some("Write|Edit".to_string()));
        assert_eq!(pre_tool.hooks.len(), 1);

        // Check PreCompact entry has no matcher (empty string is treated as None)
        let pre_compact = &settings.hooks["PreCompact"][0];
        assert!(pre_compact.matcher.is_none() || pre_compact.matcher.as_deref() == Some(""));
    }

    #[test]
    fn test_merge_claude_hooks() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Write",
                        "hooks": [
                            {"type": "command", "command": "echo test"}
                        ]
                    }
                ],
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            {"type": "command", "command": "python guard.py"}
                        ]
                    }
                ]
            }
        }"#;

        let settings: ClaudeCodeSettings = serde_json::from_str(json).unwrap();
        let mut config = HooksConfig::default();
        merge_claude_hooks(&mut config, &settings);

        // Should have hooks in the appropriate event lists
        assert_eq!(config.pre_tool_use.len(), 1);
        assert_eq!(config.user_prompt_submit.len(), 1);

        // Check the converted hook
        let entry = &config.pre_tool_use[0];
        assert_eq!(entry.matcher, Some("Write".to_string()));
        assert_eq!(entry.hooks.len(), 1);

        match &entry.hooks[0] {
            Hook::Command { command, timeout } => {
                assert_eq!(command, "echo test");
                assert_eq!(*timeout, 60); // default timeout
            }
            _ => panic!("Expected Command hook"),
        }
    }

    #[test]
    fn test_hooks_config_is_empty() {
        let empty = HooksConfig::default();
        assert!(empty.is_empty());

        let mut non_empty = HooksConfig::default();
        non_empty.pre_tool_use.push(HookEntry {
            matcher: None,
            hooks: vec![],
        });
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_merge_hooks_config() {
        let mut base = HooksConfig::default();
        base.pre_tool_use.push(HookEntry {
            matcher: Some("Read".to_string()),
            hooks: vec![],
        });

        let mut other = HooksConfig::default();
        other.pre_tool_use.push(HookEntry {
            matcher: Some("Write".to_string()),
            hooks: vec![],
        });
        other.user_prompt_submit.push(HookEntry {
            matcher: None,
            hooks: vec![],
        });

        let merged = merge_hooks_config(base, other);

        // Should have entries from both configs
        assert_eq!(merged.pre_tool_use.len(), 2);
        assert_eq!(merged.user_prompt_submit.len(), 1);
    }

    #[test]
    fn test_empty_matcher_filtered() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "",
                        "hooks": [
                            {"type": "command", "command": "echo test"}
                        ]
                    }
                ]
            }
        }"#;

        let settings: ClaudeCodeSettings = serde_json::from_str(json).unwrap();
        let mut config = HooksConfig::default();
        merge_claude_hooks(&mut config, &settings);

        // Empty matcher should be converted to None (matches all)
        assert_eq!(config.pre_tool_use[0].matcher, None);
    }

    // ========================================================================
    // Extended HookInput Tests
    // ========================================================================

    #[test]
    fn test_hook_input_with_prompt() {
        let input =
            HookInput::new(HookEvent::UserPromptSubmit).with_prompt("How do I fix this bug?");

        assert_eq!(input.event, HookEvent::UserPromptSubmit);
        assert_eq!(input.prompt, Some("How do I fix this bug?".to_string()));
    }

    #[test]
    fn test_hook_input_with_extra() {
        let input = HookInput::new(HookEvent::PreCompact)
            .with_extra("current_tokens", serde_json::json!(50000))
            .with_extra("max_tokens", serde_json::json!(100000));

        assert_eq!(
            input.extra.get("current_tokens"),
            Some(&serde_json::json!(50000))
        );
        assert_eq!(
            input.extra.get("max_tokens"),
            Some(&serde_json::json!(100000))
        );
    }

    #[test]
    fn test_hook_input_cwd_populated() {
        let input = HookInput::new(HookEvent::SessionStart);

        // CWD should be populated from env
        assert!(!input.cwd.is_empty());
    }

    #[test]
    fn test_hook_input_serialization() {
        let input = HookInput::new(HookEvent::PreToolUse)
            .with_session_id("session-123")
            .with_tool("bash", serde_json::json!({"command": "ls"}));

        let json = serde_json::to_string(&input).unwrap();

        assert!(json.contains("\"event\":\"pre_tool_use\""));
        assert!(json.contains("\"session_id\":\"session-123\""));
        assert!(json.contains("\"tool_name\":\"bash\""));
    }

    // ========================================================================
    // Extended HookResult Tests
    // ========================================================================

    #[test]
    fn test_hook_result_denied() {
        let result = HookResult::denied("Action not allowed");

        assert!(!result.allowed);
        assert_eq!(result.outputs.len(), 1);
        assert_eq!(result.outputs[0].decision, Some("deny".to_string()));
        assert_eq!(
            result.outputs[0].reason,
            Some("Action not allowed".to_string())
        );
    }

    #[test]
    fn test_hook_result_modified_prompt() {
        let result = HookResult {
            allowed: true,
            outputs: vec![HookOutput {
                prompt: Some("Modified user prompt".to_string()),
                ..Default::default()
            }],
            errors: vec![],
        };

        assert_eq!(result.modified_prompt(), Some("Modified user prompt"));
    }

    #[test]
    fn test_hook_result_no_modified_prompt() {
        let result = HookResult::allowed();
        assert_eq!(result.modified_prompt(), None);
    }

    #[test]
    fn test_hook_result_multiple_system_messages() {
        let result = HookResult {
            allowed: true,
            outputs: vec![
                HookOutput {
                    system_message: Some("Security warning".to_string()),
                    ..Default::default()
                },
                HookOutput::default(), // No message
                HookOutput {
                    system_message: Some("Style guide reminder".to_string()),
                    ..Default::default()
                },
            ],
            errors: vec![],
        };

        let messages = result.system_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], "Security warning");
        assert_eq!(messages[1], "Style guide reminder");
    }

    // ========================================================================
    // HookError Tests
    // ========================================================================

    #[test]
    fn test_hook_error_display() {
        let timeout_err = HookError::Timeout(30);
        assert_eq!(
            format!("{}", timeout_err),
            "Hook timed out after 30 seconds"
        );

        let cmd_err = HookError::CommandFailed("Process exited with code 1".to_string());
        assert_eq!(
            format!("{}", cmd_err),
            "Hook command failed: Process exited with code 1"
        );

        let parse_err = HookError::ParseError("Invalid JSON".to_string());
        assert_eq!(
            format!("{}", parse_err),
            "Hook output parse error: Invalid JSON"
        );

        let blocked_err = HookError::Blocked("File write not allowed".to_string());
        assert_eq!(
            format!("{}", blocked_err),
            "Hook blocked action: File write not allowed"
        );

        let matcher_err = HookError::InvalidMatcher("(unclosed".to_string());
        assert_eq!(
            format!("{}", matcher_err),
            "Invalid matcher regex: (unclosed"
        );
    }

    // ========================================================================
    // HookEngine Matcher Tests
    // ========================================================================

    #[test]
    fn test_hook_engine_matcher_regex() {
        let engine = HookEngine::new(HooksConfig::default());

        // Valid pattern match
        let result = engine.validate_and_match("Write|Edit", "Write");
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Valid pattern no match
        let result = engine.validate_and_match("Write|Edit", "Read");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_hook_engine_matcher_invalid_regex() {
        let engine = HookEngine::new(HooksConfig::default());

        // Invalid regex pattern
        let result = engine.validate_and_match("(unclosed", "test");
        assert!(result.is_err());
        assert!(matches!(result, Err(HookError::InvalidMatcher(_))));
    }

    #[test]
    fn test_hook_engine_matcher_empty_pattern() {
        let engine = HookEngine::new(HooksConfig::default());

        // Empty pattern is invalid
        let result = engine.validate_and_match("", "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_hook_engine_matcher_complex_patterns() {
        let engine = HookEngine::new(HooksConfig::default());

        // Case sensitive by default
        let result = engine.validate_and_match("Write", "write");
        assert!(result.is_ok());
        assert!(!result.unwrap()); // Should not match (case sensitive)

        // Dot matches any char
        let result = engine.validate_and_match(".*file.*", "read_file_content");
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Character class
        let result = engine.validate_and_match("^(read|write)_.*", "read_file");
        assert!(result.is_ok());
        assert!(result.unwrap());

        let result = engine.validate_and_match("^(read|write)_.*", "delete_file");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    // ========================================================================
    // HookEngine Check Blocked Tests
    // ========================================================================

    #[test]
    fn test_check_blocked_allowed() {
        let result = HookResult::allowed();
        assert!(HookEngine::check_blocked(&result).is_ok());
    }

    #[test]
    fn test_check_blocked_denied() {
        let result = HookResult::denied("Not permitted");
        let err = HookEngine::check_blocked(&result);
        assert!(err.is_err());

        match err {
            Err(HookError::Blocked(reason)) => {
                assert_eq!(reason, "Not permitted");
            }
            _ => panic!("Expected Blocked error"),
        }
    }

    #[test]
    fn test_check_blocked_denied_default_reason() {
        let result = HookResult {
            allowed: false,
            outputs: vec![], // No outputs with reason
            errors: vec![],
        };

        let err = HookEngine::check_blocked(&result);
        assert!(err.is_err());

        match err {
            Err(HookError::Blocked(reason)) => {
                assert_eq!(reason, "Action blocked by hook");
            }
            _ => panic!("Expected Blocked error"),
        }
    }

    // ========================================================================
    // HookOutput Tests
    // ========================================================================

    #[test]
    fn test_hook_output_default() {
        let output = HookOutput::default();
        assert!(output.decision.is_none());
        assert!(output.reason.is_none());
        assert!(output.system_message.is_none());
        assert!(output.prompt.is_none());
        assert!(output.extra.is_empty());
    }

    #[test]
    fn test_hook_output_from_json() {
        let json = r#"{
            "decision": "allow",
            "reason": "Validation passed",
            "systemMessage": "Remember to test",
            "prompt": "Modified prompt",
            "customField": "custom value"
        }"#;

        let output: HookOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.decision, Some("allow".to_string()));
        assert_eq!(output.reason, Some("Validation passed".to_string()));
        assert_eq!(output.system_message, Some("Remember to test".to_string()));
        assert_eq!(output.prompt, Some("Modified prompt".to_string()));
        assert_eq!(
            output.extra.get("customField"),
            Some(&serde_json::json!("custom value"))
        );
    }

    #[test]
    fn test_hook_output_partial_json() {
        let json = r#"{"decision": "deny"}"#;

        let output: HookOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.decision, Some("deny".to_string()));
        assert!(output.reason.is_none());
        assert!(output.system_message.is_none());
    }

    // ========================================================================
    // Parse Hook Output Tests
    // ========================================================================

    #[test]
    fn test_parse_hook_output_empty() {
        let result = HookEngine::parse_hook_output("");
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.decision.is_none());
    }

    #[test]
    fn test_parse_hook_output_whitespace() {
        let result = HookEngine::parse_hook_output("   \n\t  ");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_hook_output_valid_json() {
        let result = HookEngine::parse_hook_output(r#"{"decision": "allow"}"#);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decision, Some("allow".to_string()));
    }

    #[test]
    fn test_parse_hook_output_plain_text() {
        // Plain text (not starting with '{') is treated as additionalContext, not an error
        let result = HookEngine::parse_hook_output("not valid json {").unwrap();
        assert_eq!(result.additional_context, Some("not valid json {".to_string()));
    }

    #[test]
    fn test_parse_hook_output_invalid_json_starting_with_brace() {
        // Starts with '{' but invalid JSON — still treated as plain text
        let result = HookEngine::parse_hook_output("{not valid}").unwrap();
        assert_eq!(result.additional_context, Some("{not valid}".to_string()));
    }

    // ========================================================================
    // All Hook Events Test
    // ========================================================================

    #[test]
    fn test_all_hook_events_have_config_keys() {
        // Verify all events return valid config keys
        let events = vec![
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::PostToolUseFailure,
            HookEvent::UserPromptSubmit,
            HookEvent::Stop,
            HookEvent::SubagentStart,
            HookEvent::SubagentStop,
            HookEvent::PreCompact,
            HookEvent::PermissionRequest,
            HookEvent::Notification,
        ];

        for event in events {
            let key = event.config_key();
            assert!(
                !key.is_empty(),
                "Event {:?} should have non-empty config key",
                event
            );
            assert!(
                key.chars().all(|c| c.is_lowercase() || c == '_'),
                "Config key '{}' should be snake_case",
                key
            );
        }
    }

    // ========================================================================
    // Async Hook Tests
    // ========================================================================

    #[tokio::test]
    async fn test_run_with_matching_entry() {
        let mut config = HooksConfig::default();
        config.pre_tool_use.push(crate::config::HookEntry {
            matcher: Some("Write".to_string()),
            hooks: vec![crate::config::Hook::Prompt {
                prompt: "Remember to backup".to_string(),
                timeout: 30,
            }],
        });

        let engine = HookEngine::new(config);
        let input = HookInput::new(HookEvent::PreToolUse)
            .with_tool("Write", serde_json::json!({"path": "/tmp/test"}));

        let result = engine.run(HookEvent::PreToolUse, &input).await;

        assert!(result.allowed);
        assert_eq!(result.outputs.len(), 1);
        assert_eq!(
            result.outputs[0].system_message,
            Some("Remember to backup".to_string())
        );
    }

    #[tokio::test]
    async fn test_run_with_non_matching_entry() {
        let mut config = HooksConfig::default();
        config.pre_tool_use.push(crate::config::HookEntry {
            matcher: Some("Write".to_string()),
            hooks: vec![crate::config::Hook::Prompt {
                prompt: "Should not appear".to_string(),
                timeout: 30,
            }],
        });

        let engine = HookEngine::new(config);
        let input = HookInput::new(HookEvent::PreToolUse)
            .with_tool("Read", serde_json::json!({"path": "/tmp/test"})); // Different tool

        let result = engine.run(HookEvent::PreToolUse, &input).await;

        assert!(result.allowed);
        assert!(result.outputs.is_empty()); // No matching hooks ran
    }

    #[tokio::test]
    async fn test_run_multiple_hooks() {
        let mut config = HooksConfig::default();
        config.pre_tool_use.push(crate::config::HookEntry {
            matcher: None, // Matches all
            hooks: vec![
                crate::config::Hook::Prompt {
                    prompt: "First instruction".to_string(),
                    timeout: 30,
                },
                crate::config::Hook::Prompt {
                    prompt: "Second instruction".to_string(),
                    timeout: 30,
                },
            ],
        });

        let engine = HookEngine::new(config);
        let input = HookInput::new(HookEvent::PreToolUse).with_tool("bash", serde_json::json!({}));

        let result = engine.run(HookEvent::PreToolUse, &input).await;

        assert!(result.allowed);
        assert_eq!(result.outputs.len(), 2);
    }
}
