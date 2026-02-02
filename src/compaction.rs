//! Context Compaction - Manages context window limits for long-running sessions.
//!
//! Features:
//! - Token estimation for messages
//! - Context window limit detection
//! - PreCompact hook triggering
//! - Conversation summarization
//! - Critical information preservation

use crate::hooks::{HookEngine, HookEvent, HookInput};
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Context window sizes for different models (in tokens)
const CLAUDE_OPUS_CONTEXT: usize = 200_000;
const CLAUDE_SONNET_CONTEXT: usize = 200_000;
const CLAUDE_HAIKU_CONTEXT: usize = 200_000;
const GPT4_CONTEXT: usize = 128_000;
const GPT4O_CONTEXT: usize = 128_000;
const GPT35_CONTEXT: usize = 16_385;
const GEMINI_PRO_CONTEXT: usize = 1_000_000;
const DEFAULT_CONTEXT: usize = 128_000;

/// Safety margin - trigger compaction before hitting the limit
const COMPACTION_THRESHOLD: f32 = 0.85;

/// Minimum tokens to preserve for response
const RESPONSE_RESERVE: usize = 4_096;

/// Configuration for context compaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Maximum context window size (tokens)
    pub max_context_tokens: usize,
    /// Threshold ratio to trigger compaction (0.0-1.0)
    pub threshold: f32,
    /// Minimum number of recent messages to always preserve
    pub preserve_recent: usize,
    /// Whether to always preserve system messages
    pub preserve_system: bool,
    /// Whether to preserve tool call/result pairs
    pub preserve_tool_calls: bool,
    /// Custom summary prompt (if any)
    pub summary_prompt: Option<String>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: DEFAULT_CONTEXT,
            threshold: COMPACTION_THRESHOLD,
            preserve_recent: 4,
            preserve_system: true,
            preserve_tool_calls: true,
            summary_prompt: None,
        }
    }
}

impl CompactionConfig {
    /// Create config for a specific model
    pub fn for_model(model: &str) -> Self {
        let max_context_tokens = get_context_window(model);
        Self {
            max_context_tokens,
            ..Default::default()
        }
    }
}

/// Get context window size for a model
pub fn get_context_window(model: &str) -> usize {
    let model_lower = model.to_lowercase();

    if model_lower.contains("opus") {
        CLAUDE_OPUS_CONTEXT
    } else if model_lower.contains("sonnet") {
        CLAUDE_SONNET_CONTEXT
    } else if model_lower.contains("haiku") {
        CLAUDE_HAIKU_CONTEXT
    } else if model_lower.contains("claude") {
        CLAUDE_SONNET_CONTEXT // Default Claude
    } else if model_lower.contains("gpt-4o") {
        GPT4O_CONTEXT
    } else if model_lower.contains("gpt-4") {
        GPT4_CONTEXT
    } else if model_lower.contains("gpt-3.5") {
        GPT35_CONTEXT
    } else if model_lower.contains("gemini") {
        GEMINI_PRO_CONTEXT
    } else if model_lower.contains("o1") || model_lower.contains("o3") {
        GPT4O_CONTEXT
    } else {
        DEFAULT_CONTEXT
    }
}

/// Estimate token count for a string (approximate: ~4 chars per token)
pub fn estimate_tokens(text: &str) -> usize {
    // More accurate estimation considering whitespace and punctuation
    let char_count = text.chars().count();
    let word_count = text.split_whitespace().count();

    // Use a weighted average of character-based and word-based estimation
    // Most tokenizers use subword units, so this approximates that
    let char_estimate = char_count / 4;
    let word_estimate = (word_count as f32 * 1.3) as usize;

    // Take the average, biased toward character count
    (char_estimate * 2 + word_estimate) / 3
}

/// Estimate token count for a message
pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let content_tokens = match &message.content {
        MessageContent::Text(text) => estimate_tokens(text),
        MessageContent::Parts(parts) => {
            parts
                .iter()
                .map(|p| {
                    p.text.as_ref().map(|t| estimate_tokens(t)).unwrap_or(0)
                        + if p.image_url.is_some() { 1000 } else { 0 } // Images cost ~1000 tokens
                })
                .sum()
        }
    };

    // Add overhead for role, name, etc.
    let overhead = 4 + message
        .name
        .as_ref()
        .map(|n| estimate_tokens(n))
        .unwrap_or(0);

    // Tool calls add significant tokens
    let tool_tokens = message
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|c| estimate_tokens(&c.to_string()))
                .sum::<usize>()
        })
        .unwrap_or(0);

    content_tokens + overhead + tool_tokens
}

/// Estimate total token count for a request
pub fn estimate_request_tokens(request: &ChatCompletionRequest) -> usize {
    let message_tokens: usize = request.messages.iter().map(estimate_message_tokens).sum();

    // Add tool definitions if present
    let tool_tokens = request
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|t| estimate_tokens(&t.to_string()))
                .sum::<usize>()
        })
        .unwrap_or(0);

    // Add some overhead for request structure
    message_tokens + tool_tokens + 100
}

/// Result of compaction analysis
#[derive(Debug, Clone)]
pub struct CompactionAnalysis {
    /// Current estimated token count
    pub current_tokens: usize,
    /// Maximum allowed tokens
    pub max_tokens: usize,
    /// Whether compaction is needed
    pub needs_compaction: bool,
    /// Tokens that need to be freed
    pub tokens_to_free: usize,
    /// Suggested messages to summarize (indices)
    pub messages_to_summarize: Vec<usize>,
    /// Messages to preserve (indices)
    pub messages_to_preserve: Vec<usize>,
}

/// Context compaction engine
#[derive(Clone)]
pub struct ContextCompactor {
    config: CompactionConfig,
}

impl ContextCompactor {
    /// Create a new context compactor
    pub fn new(config: CompactionConfig) -> Self {
        Self { config }
    }

    /// Create a compactor for a specific model
    pub fn for_model(model: &str) -> Self {
        Self::new(CompactionConfig::for_model(model))
    }

    /// Analyze whether compaction is needed.
    /// If `actual_input_tokens` is provided (from a previous turn's provider response),
    /// it will be used instead of the estimator for more accurate decisions.
    pub fn analyze_with_hint(
        &self,
        request: &ChatCompletionRequest,
        actual_input_tokens: Option<usize>,
    ) -> CompactionAnalysis {
        let estimated = estimate_request_tokens(request);
        let current_tokens = actual_input_tokens.unwrap_or(estimated);

        if actual_input_tokens.is_some() {
            debug!(
                estimated = estimated,
                actual = current_tokens,
                delta = (current_tokens as i64 - estimated as i64),
                "Using actual token count for compaction analysis"
            );
        }

        let threshold_tokens =
            (self.config.max_context_tokens as f32 * self.config.threshold) as usize;
        let effective_threshold = threshold_tokens.saturating_sub(RESPONSE_RESERVE);
        let needs_compaction = current_tokens > effective_threshold;

        let target_tokens = threshold_tokens / 2;
        let tokens_to_free = if needs_compaction {
            current_tokens.saturating_sub(target_tokens)
        } else {
            0
        };

        let (preserve, summarize) = self.categorize_messages(&request.messages);

        CompactionAnalysis {
            current_tokens,
            max_tokens: self.config.max_context_tokens,
            needs_compaction,
            tokens_to_free,
            messages_to_summarize: summarize,
            messages_to_preserve: preserve,
        }
    }

    /// Analyze whether compaction is needed
    pub fn analyze(&self, request: &ChatCompletionRequest) -> CompactionAnalysis {
        let current_tokens = estimate_request_tokens(request);
        let threshold_tokens =
            (self.config.max_context_tokens as f32 * self.config.threshold) as usize;
        let effective_threshold = threshold_tokens.saturating_sub(RESPONSE_RESERVE);
        let needs_compaction = current_tokens > effective_threshold;

        let target_tokens = threshold_tokens / 2;
        let tokens_to_free = if needs_compaction {
            current_tokens.saturating_sub(target_tokens)
        } else {
            0
        };

        // Determine which messages to preserve vs summarize
        let (preserve, summarize) = self.categorize_messages(&request.messages);

        CompactionAnalysis {
            current_tokens,
            max_tokens: self.config.max_context_tokens,
            needs_compaction,
            tokens_to_free,
            messages_to_summarize: summarize,
            messages_to_preserve: preserve,
        }
    }

    /// Categorize messages into preserve vs summarize
    fn categorize_messages(&self, messages: &[ChatMessage]) -> (Vec<usize>, Vec<usize>) {
        let mut preserve = Vec::new();
        let mut summarize = Vec::new();
        let msg_count = messages.len();

        for (i, msg) in messages.iter().enumerate() {
            let should_preserve =
                // Always preserve system messages if configured
                (self.config.preserve_system && msg.role == "system")
                // Preserve recent messages
                || i >= msg_count.saturating_sub(self.config.preserve_recent)
                // Preserve tool calls/results if configured
                || (self.config.preserve_tool_calls &&
                    (msg.role == "tool" || msg.tool_calls.is_some() || msg.tool_call_id.is_some()));

            if should_preserve {
                preserve.push(i);
            } else {
                summarize.push(i);
            }
        }

        (preserve, summarize)
    }

    /// Compact the request by summarizing older messages
    pub async fn compact(
        &self,
        request: &mut ChatCompletionRequest,
        hook_engine: Option<&HookEngine>,
        session_id: Option<&str>,
    ) -> Result<CompactionResult, CompactionError> {
        self.compact_with_hint(request, hook_engine, session_id, None)
            .await
    }

    /// Compact with an optional actual token count hint from the provider
    pub async fn compact_with_hint(
        &self,
        request: &mut ChatCompletionRequest,
        hook_engine: Option<&HookEngine>,
        session_id: Option<&str>,
        actual_input_tokens: Option<usize>,
    ) -> Result<CompactionResult, CompactionError> {
        let analysis = self.analyze_with_hint(request, actual_input_tokens);

        if !analysis.needs_compaction {
            return Ok(CompactionResult {
                compacted: false,
                original_tokens: analysis.current_tokens,
                new_tokens: analysis.current_tokens,
                messages_summarized: 0,
                summary: None,
            });
        }

        info!(
            current = analysis.current_tokens,
            max = analysis.max_tokens,
            to_free = analysis.tokens_to_free,
            "Context compaction needed"
        );

        // Run PreCompact hooks if engine provided
        if let Some(engine) = hook_engine {
            let mut hook_input = HookInput::new(HookEvent::PreCompact)
                .with_extra("current_tokens", serde_json::json!(analysis.current_tokens))
                .with_extra("max_tokens", serde_json::json!(analysis.max_tokens));

            if let Some(sid) = session_id {
                hook_input = hook_input.with_session_id(sid);
            }

            let hook_result = engine.run(HookEvent::PreCompact, &hook_input).await;

            if !hook_result.allowed {
                warn!("PreCompact hook blocked compaction");
                return Err(CompactionError::HookBlocked(
                    hook_result
                        .outputs
                        .first()
                        .and_then(|o| o.reason.clone())
                        .unwrap_or_else(|| "Hook blocked compaction".to_string()),
                ));
            }
        }

        // Extract messages to summarize
        let messages_to_summarize: Vec<&ChatMessage> = analysis
            .messages_to_summarize
            .iter()
            .filter_map(|&i| request.messages.get(i))
            .collect();

        if messages_to_summarize.is_empty() {
            debug!("No messages available for summarization");
            return Ok(CompactionResult {
                compacted: false,
                original_tokens: analysis.current_tokens,
                new_tokens: analysis.current_tokens,
                messages_summarized: 0,
                summary: None,
            });
        }

        // Generate summary of old messages
        let summary = self.generate_summary(&messages_to_summarize);

        // Build new message list: system + summary + preserved messages
        let mut new_messages = Vec::new();

        // Keep system messages at the start
        for &i in &analysis.messages_to_preserve {
            if let Some(msg) = request.messages.get(i) {
                if msg.role == "system" {
                    new_messages.push(msg.clone());
                }
            }
        }

        // Add summary as a system message
        new_messages.push(ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text(summary.clone()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });

        // Add non-system preserved messages
        for &i in &analysis.messages_to_preserve {
            if let Some(msg) = request.messages.get(i) {
                if msg.role != "system" {
                    new_messages.push(msg.clone());
                }
            }
        }

        let original_count = request.messages.len();
        let summarized_count = messages_to_summarize.len();
        request.messages = new_messages;

        let new_tokens = estimate_request_tokens(request);

        // Verify compaction actually reduced tokens
        if new_tokens >= analysis.current_tokens {
            warn!(
                original_tokens = analysis.current_tokens,
                new_tokens = new_tokens,
                "Compaction did not reduce token count"
            );
            return Err(CompactionError::Failed(
                "Compaction did not reduce token count".to_string(),
            ));
        }

        info!(
            original_messages = original_count,
            summarized = summarized_count,
            new_messages = request.messages.len(),
            original_tokens = analysis.current_tokens,
            new_tokens = new_tokens,
            saved = analysis.current_tokens.saturating_sub(new_tokens),
            "Context compacted"
        );

        Ok(CompactionResult {
            compacted: true,
            original_tokens: analysis.current_tokens,
            new_tokens,
            messages_summarized: summarized_count,
            summary: Some(summary),
        })
    }

    /// Generate a summary of messages
    fn generate_summary(&self, messages: &[&ChatMessage]) -> String {
        let mut summary = String::new();
        summary.push_str("<context-summary>\n");
        summary.push_str("The following is a summary of the earlier conversation:\n\n");

        // Group by conversation turns
        let mut current_role = "";
        let mut turn_content = Vec::new();

        for msg in messages {
            if msg.role != current_role && !turn_content.is_empty() {
                summary.push_str(&format!("**{}**: ", capitalize(current_role)));
                summary.push_str(&turn_content.join(" "));
                summary.push_str("\n\n");
                turn_content.clear();
            }

            current_role = &msg.role;

            let content = match &msg.content {
                MessageContent::Text(t) => truncate_for_summary(t, 500),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| p.text.as_ref())
                    .map(|t| truncate_for_summary(t, 200))
                    .collect::<Vec<_>>()
                    .join(" "),
            };

            if !content.is_empty() {
                turn_content.push(content);
            }

            // Note tool usage
            if msg.tool_calls.is_some() {
                turn_content.push("[Used tools]".to_string());
            }
            if msg.tool_call_id.is_some() {
                turn_content.push("[Tool result]".to_string());
            }
        }

        // Flush remaining
        if !turn_content.is_empty() {
            summary.push_str(&format!("**{}**: ", capitalize(current_role)));
            summary.push_str(&turn_content.join(" "));
            summary.push('\n');
        }

        summary.push_str("</context-summary>");
        summary
    }

    /// Get the current configuration
    pub fn config(&self) -> &CompactionConfig {
        &self.config
    }

    /// Update configuration
    pub fn set_config(&mut self, config: CompactionConfig) {
        self.config = config;
    }
}

/// Result of a compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Whether compaction was performed
    pub compacted: bool,
    /// Original token count
    pub original_tokens: usize,
    /// New token count after compaction
    pub new_tokens: usize,
    /// Number of messages that were summarized
    pub messages_summarized: usize,
    /// The generated summary (if any)
    pub summary: Option<String>,
}

/// Errors that can occur during compaction
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("PreCompact hook blocked compaction: {0}")]
    HookBlocked(String),

    #[error("Compaction failed: {0}")]
    Failed(String),
}

/// Helper to capitalize first letter
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Helper to truncate text for summary
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{}...", truncated.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn create_test_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: MessageContent::Text(content.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn create_test_request(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages,
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn test_estimate_tokens() {
        // Rough estimation: ~4 chars per token
        assert!(estimate_tokens("hello world") > 0);
        assert!(estimate_tokens("hello world") < 10);

        // Longer text should have more tokens
        let short = estimate_tokens("hi");
        let long =
            estimate_tokens("This is a much longer piece of text that should have more tokens");
        assert!(long > short);
    }

    #[test]
    fn test_get_context_window() {
        assert_eq!(
            get_context_window("claude-3-opus-20240229"),
            CLAUDE_OPUS_CONTEXT
        );
        assert_eq!(
            get_context_window("claude-3-5-sonnet-20241022"),
            CLAUDE_SONNET_CONTEXT
        );
        assert_eq!(get_context_window("gpt-4o"), GPT4O_CONTEXT);
        assert_eq!(get_context_window("gpt-4"), GPT4_CONTEXT);
        assert_eq!(get_context_window("gpt-3.5-turbo"), GPT35_CONTEXT);
        assert_eq!(get_context_window("gemini-pro"), GEMINI_PRO_CONTEXT);
        assert_eq!(get_context_window("unknown-model"), DEFAULT_CONTEXT);
    }

    #[test]
    fn test_analyze_no_compaction_needed() {
        let messages = vec![
            create_test_message("system", "You are helpful."),
            create_test_message("user", "Hello"),
            create_test_message("assistant", "Hi there!"),
        ];

        let request = create_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());
        let analysis = compactor.analyze(&request);

        assert!(!analysis.needs_compaction);
        assert_eq!(analysis.tokens_to_free, 0);
    }

    #[test]
    fn test_analyze_compaction_needed() {
        // Create a request with many long messages
        let long_content = "x".repeat(50000); // ~12500 tokens
        let messages = vec![
            create_test_message("system", "You are helpful."),
            create_test_message("user", &long_content),
            create_test_message("assistant", &long_content),
            create_test_message("user", &long_content),
            create_test_message("assistant", &long_content),
        ];

        let request = create_test_request(messages);

        // Use a small context window to force compaction
        let config = CompactionConfig {
            max_context_tokens: 10000,
            threshold: 0.8,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);
        let analysis = compactor.analyze(&request);

        assert!(analysis.needs_compaction);
        assert!(analysis.tokens_to_free > 0);
    }

    #[test]
    fn test_categorize_messages() {
        let messages = vec![
            create_test_message("system", "System prompt"),
            create_test_message("user", "First question"),
            create_test_message("assistant", "First answer"),
            create_test_message("user", "Second question"),
            create_test_message("assistant", "Second answer"),
            create_test_message("user", "Third question"),
            create_test_message("assistant", "Third answer"),
        ];

        let config = CompactionConfig {
            preserve_recent: 2,
            preserve_system: true,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);
        let (preserve, summarize) = compactor.categorize_messages(&messages);

        // Should preserve: system (index 0) and last 2 messages (indices 5, 6)
        assert!(preserve.contains(&0)); // system
        assert!(preserve.contains(&5)); // recent
        assert!(preserve.contains(&6)); // recent

        // Should summarize: indices 1-4
        assert!(summarize.contains(&1));
        assert!(summarize.contains(&2));
        assert!(summarize.contains(&3));
        assert!(summarize.contains(&4));
    }

    #[test]
    fn test_generate_summary() {
        let messages = vec![
            create_test_message("user", "What is Rust?"),
            create_test_message("assistant", "Rust is a systems programming language."),
        ];

        let compactor = ContextCompactor::new(CompactionConfig::default());
        let msg_refs: Vec<&ChatMessage> = messages.iter().collect();
        let summary = compactor.generate_summary(&msg_refs);

        assert!(summary.contains("<context-summary>"));
        assert!(summary.contains("</context-summary>"));
        assert!(summary.contains("User"));
        assert!(summary.contains("Assistant"));
    }

    #[test]
    fn test_truncate_for_summary() {
        let short = "Hello";
        assert_eq!(truncate_for_summary(short, 100), "Hello");

        let long = "x".repeat(200);
        let truncated = truncate_for_summary(&long, 50);
        assert!(truncated.len() < 60);
        assert!(truncated.ends_with("..."));
    }

    #[tokio::test]
    async fn test_compact_not_needed() {
        let messages = vec![
            create_test_message("system", "You are helpful."),
            create_test_message("user", "Hi"),
        ];

        let mut request = create_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());

        let result = compactor.compact(&mut request, None, None).await.unwrap();

        assert!(!result.compacted);
        assert_eq!(result.messages_summarized, 0);
        assert!(result.summary.is_none());
    }

    #[tokio::test]
    async fn test_compact_performed() {
        // Create request that needs compaction
        let long_content = "x".repeat(10000);
        let messages = vec![
            create_test_message("system", "You are helpful."),
            create_test_message("user", &long_content),
            create_test_message("assistant", &long_content),
            create_test_message("user", &long_content),
            create_test_message("assistant", &long_content),
            create_test_message("user", "Recent message"),
            create_test_message("assistant", "Recent response"),
        ];

        let mut request = create_test_request(messages);

        let config = CompactionConfig {
            max_context_tokens: 5000,
            threshold: 0.8,
            preserve_recent: 2,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);
        let result = compactor.compact(&mut request, None, None).await.unwrap();

        assert!(result.compacted);
        assert!(result.messages_summarized > 0);
        assert!(result.summary.is_some());
        assert!(result.new_tokens < result.original_tokens);
    }

    // ========================================================================
    // Extended Compaction Tests
    // ========================================================================

    #[test]
    fn test_compaction_config_for_model() {
        let config = CompactionConfig::for_model("claude-3-opus");
        assert_eq!(config.max_context_tokens, CLAUDE_OPUS_CONTEXT);

        let config = CompactionConfig::for_model("gpt-4o-mini");
        assert_eq!(config.max_context_tokens, GPT4O_CONTEXT);

        let config = CompactionConfig::for_model("gemini-1.5-pro");
        assert_eq!(config.max_context_tokens, GEMINI_PRO_CONTEXT);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_unicode() {
        // Unicode characters should still be counted
        let unicode = "Hello 世界 🦀";
        let tokens = estimate_tokens(unicode);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_message_tokens_with_name() {
        let mut msg = create_test_message("user", "Hello");
        msg.name = Some("John".to_string());

        let tokens = estimate_message_tokens(&msg);
        let msg_no_name = create_test_message("user", "Hello");
        let tokens_no_name = estimate_message_tokens(&msg_no_name);

        // Message with name should have more tokens
        assert!(tokens > tokens_no_name);
    }

    #[test]
    fn test_estimate_message_tokens_with_parts() {
        use crate::proxy::ContentPart;

        let msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![
                ContentPart {
                    content_type: "text".to_string(),
                    text: Some("Hello world".to_string()),
                    image_url: None,
                },
                ContentPart {
                    content_type: "text".to_string(),
                    text: Some("How are you?".to_string()),
                    image_url: None,
                },
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };

        let tokens = estimate_message_tokens(&msg);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_message_tokens_with_image() {
        use crate::proxy::ContentPart;

        let msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![ContentPart {
                content_type: "image_url".to_string(),
                text: None,
                image_url: Some(serde_json::json!({
                    "url": "data:image/png;base64,iVBORw0..."
                })),
            }]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };

        let tokens = estimate_message_tokens(&msg);
        // Images should cost approximately 1000 tokens
        assert!(tokens >= 1000);
    }

    #[test]
    fn test_estimate_request_tokens_with_tools() {
        let messages = vec![create_test_message("user", "Help me write code")];
        let mut request = create_test_request(messages);

        // Add some tools
        request.tools = Some(vec![
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file from disk",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "write_file",
                    "description": "Write a file to disk",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}
                }
            }),
        ]);

        let tokens_with_tools = estimate_request_tokens(&request);

        request.tools = None;
        let tokens_without_tools = estimate_request_tokens(&request);

        assert!(tokens_with_tools > tokens_without_tools);
    }

    #[test]
    fn test_categorize_preserves_tool_messages() {
        let messages = vec![
            create_test_message("system", "You are helpful"),
            create_test_message("user", "Run a command"),
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("I'll run ls".to_string()),
                name: None,
                tool_calls: Some(vec![
                    serde_json::json!({"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}),
                ]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: MessageContent::Text("file1.txt\nfile2.txt".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
            create_test_message("user", "Recent message"),
        ];

        let config = CompactionConfig {
            preserve_tool_calls: true,
            preserve_recent: 1,
            preserve_system: true,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);
        let (preserve, summarize) = compactor.categorize_messages(&messages);

        // Should preserve system (0), tool calls (2), tool results (3), and recent (4)
        assert!(preserve.contains(&0)); // system
        assert!(preserve.contains(&2)); // tool call
        assert!(preserve.contains(&3)); // tool result
        assert!(preserve.contains(&4)); // recent

        // Should summarize user message (1)
        assert!(summarize.contains(&1));
    }

    #[test]
    fn test_categorize_no_preserve_tool_calls() {
        let messages = vec![
            create_test_message("system", "You are helpful"),
            create_test_message("user", "Old message"),
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("I'll run ls".to_string()),
                name: None,
                tool_calls: Some(vec![serde_json::json!({"id": "call_1"})]),
                tool_call_id: None,
            },
            create_test_message("user", "Recent message"),
        ];

        let config = CompactionConfig {
            preserve_tool_calls: false,
            preserve_recent: 1,
            preserve_system: true,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);
        let (_preserve, summarize) = compactor.categorize_messages(&messages);

        // Tool call message should be in summarize when preserve_tool_calls is false
        assert!(summarize.contains(&2));
    }

    #[test]
    fn test_generate_summary_with_tool_markers() {
        let messages = vec![
            create_test_message("user", "Run ls command"),
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("Running ls".to_string()),
                name: None,
                tool_calls: Some(vec![serde_json::json!({"id": "1"})]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: MessageContent::Text("file.txt".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: Some("1".to_string()),
            },
        ];

        let compactor = ContextCompactor::new(CompactionConfig::default());
        let msg_refs: Vec<&ChatMessage> = messages.iter().collect();
        let summary = compactor.generate_summary(&msg_refs);

        assert!(summary.contains("[Used tools]"));
        assert!(summary.contains("[Tool result]"));
    }

    #[test]
    fn test_truncate_for_summary_edge_cases() {
        // Exactly at limit
        let text = "a".repeat(100);
        let truncated = truncate_for_summary(&text, 100);
        assert_eq!(truncated, text);
        assert!(!truncated.ends_with("..."));

        // One over limit
        let text = "a".repeat(101);
        let truncated = truncate_for_summary(&text, 100);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 103); // 100 + "..."

        // Empty string
        let truncated = truncate_for_summary("", 100);
        assert_eq!(truncated, "");
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("user"), "User");
        assert_eq!(capitalize("assistant"), "Assistant");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("ALREADY"), "ALREADY");
        assert_eq!(capitalize("a"), "A");
    }

    #[test]
    fn test_compaction_config_default() {
        let config = CompactionConfig::default();
        assert_eq!(config.max_context_tokens, DEFAULT_CONTEXT);
        assert_eq!(config.threshold, COMPACTION_THRESHOLD);
        assert_eq!(config.preserve_recent, 4);
        assert!(config.preserve_system);
        assert!(config.preserve_tool_calls);
        assert!(config.summary_prompt.is_none());
    }

    #[test]
    fn test_context_compactor_config_access() {
        let config = CompactionConfig {
            max_context_tokens: 50000,
            ..Default::default()
        };

        let mut compactor = ContextCompactor::new(config.clone());
        assert_eq!(compactor.config().max_context_tokens, 50000);

        // Update config
        let new_config = CompactionConfig {
            max_context_tokens: 100000,
            ..Default::default()
        };
        compactor.set_config(new_config);
        assert_eq!(compactor.config().max_context_tokens, 100000);
    }

    #[test]
    fn test_analysis_structure() {
        let messages = vec![
            create_test_message("system", "System prompt"),
            create_test_message("user", "Hello"),
            create_test_message("assistant", "Hi"),
        ];

        let request = create_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());
        let analysis = compactor.analyze(&request);

        // Analysis should have valid values
        assert!(analysis.current_tokens > 0);
        assert_eq!(analysis.max_tokens, DEFAULT_CONTEXT);
        assert!(!analysis.needs_compaction); // Small request
        assert_eq!(analysis.tokens_to_free, 0);

        // All messages should be in preserve (small request)
        assert!(!analysis.messages_to_preserve.is_empty());
    }

    #[test]
    fn test_get_context_window_edge_cases() {
        // Test model name variations
        assert_eq!(get_context_window("CLAUDE-3-OPUS"), CLAUDE_OPUS_CONTEXT);
        assert_eq!(get_context_window("Claude-Sonnet"), CLAUDE_SONNET_CONTEXT);
        assert_eq!(get_context_window("GPT-4O-2024-05-13"), GPT4O_CONTEXT);
        assert_eq!(get_context_window("gpt-3.5-turbo-16k"), GPT35_CONTEXT);
        assert_eq!(get_context_window("o1-preview"), GPT4O_CONTEXT);
        assert_eq!(get_context_window("o3-mini"), GPT4O_CONTEXT);
    }

    #[test]
    fn test_compaction_result_fields() {
        let result = CompactionResult {
            compacted: true,
            original_tokens: 50000,
            new_tokens: 20000,
            messages_summarized: 10,
            summary: Some("Summary content".to_string()),
        };

        assert!(result.compacted);
        assert_eq!(result.original_tokens, 50000);
        assert_eq!(result.new_tokens, 20000);
        assert_eq!(result.messages_summarized, 10);
        assert!(result.summary.is_some());
    }

    #[test]
    fn test_compaction_error_display() {
        let hook_err = CompactionError::HookBlocked("Hook prevented compaction".to_string());
        assert!(format!("{}", hook_err).contains("Hook prevented compaction"));

        let failed_err = CompactionError::Failed("Insufficient tokens freed".to_string());
        assert!(format!("{}", failed_err).contains("Insufficient tokens freed"));
    }
}
