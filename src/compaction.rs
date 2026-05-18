//! Context Compaction - Manages context window limits for long-running sessions.
//!
//! Features:
//! - Token estimation for messages
//! - Context window limit detection
//! - `PreCompact` hook triggering
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
const GPT5_CONTEXT: usize = 400_000;
const GPT4_CONTEXT: usize = 128_000;
const GPT4O_CONTEXT: usize = 128_000;
const GPT41_CONTEXT: usize = 1_000_000;
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
    #[must_use]
    pub fn for_model(model: &str) -> Self {
        let max_context_tokens = get_context_window(model);
        Self {
            max_context_tokens,
            ..Default::default()
        }
    }
}

/// Sentinel prefix that marks a system message as a compact-boundary
/// divider. Callers detect one via [`is_compact_boundary_message`]
/// rather than matching the raw string. Kept stable across releases
/// because it lives in on-disk JSONL transcripts.
pub const COMPACT_BOUNDARY_MARKER: &str = "[openclaudia:compact_boundary]";

/// Metadata carried inside a compact-boundary message, immediately
/// after [`COMPACT_BOUNDARY_MARKER`] as a one-line JSON object.
/// Mirrors Claude Code's `compactMetadata` shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactBoundaryMetadata {
    /// Whether the compaction fired from an automatic threshold
    /// trigger or from an explicit user action (`/compact`).
    pub trigger: String,
    /// Token count immediately before the compaction.
    pub pre_tokens: usize,
    /// How many messages the summary replaced.
    pub messages_summarized: usize,
}

/// Build a compact-boundary system message. Format:
/// `<marker> <json>\n<human-readable content>`. The JSON line allows
/// transcript readers to recover the metadata without parsing the
/// whole summary; the human-readable suffix keeps inline rendering
/// legible when no reader is present.
#[must_use]
pub fn build_compact_boundary_message(
    pre_tokens: usize,
    messages_summarized: usize,
) -> ChatMessage {
    let metadata = CompactBoundaryMetadata {
        trigger: "auto".to_string(),
        pre_tokens,
        messages_summarized,
    };
    let metadata_json = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".to_string());
    let content = format!(
        "{COMPACT_BOUNDARY_MARKER} {metadata_json}\nConversation compacted — {messages_summarized} earlier message(s) summarized to free context."
    );
    ChatMessage {
        role: "system".to_string(),
        content: MessageContent::Text(content),
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

/// True when `msg` is a compact-boundary marker emitted by
/// [`build_compact_boundary_message`]. Checks the raw text prefix so
/// the predicate works against both in-memory [`ChatMessage`]s and
/// [`crate::transcript::SerializedMessage`] envelopes round-tripped
/// through JSONL.
#[must_use]
pub fn is_compact_boundary_message(msg: &ChatMessage) -> bool {
    if msg.role != "system" {
        return false;
    }
    match &msg.content {
        MessageContent::Text(t) => t.starts_with(COMPACT_BOUNDARY_MARKER),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .any(|t| t.starts_with(COMPACT_BOUNDARY_MARKER)),
    }
}

/// Parse the JSON metadata out of a compact-boundary message. Returns
/// `None` for non-boundary messages or when the metadata line is
/// malformed (reader should treat this as "we know compaction
/// happened but not the details" rather than an error).
#[must_use]
pub fn extract_compact_boundary_metadata(msg: &ChatMessage) -> Option<CompactBoundaryMetadata> {
    if !is_compact_boundary_message(msg) {
        return None;
    }
    let text = match &msg.content {
        MessageContent::Text(t) => t.as_str(),
        MessageContent::Parts(parts) => parts.iter().filter_map(|p| p.text.as_deref()).next()?,
    };
    // First line after the marker is the JSON blob.
    let first_line = text.lines().next()?;
    let after_marker = first_line.strip_prefix(COMPACT_BOUNDARY_MARKER)?;
    serde_json::from_str::<CompactBoundaryMetadata>(after_marker.trim()).ok()
}

/// Get context window size for a model
#[must_use]
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
    } else if model_lower.contains("gpt-5") {
        GPT5_CONTEXT
    } else if model_lower.contains("gpt-4.1") {
        GPT41_CONTEXT
    } else if model_lower.contains("gpt-4o") {
        GPT4O_CONTEXT
    } else if model_lower.contains("gpt-4") {
        GPT4_CONTEXT
    } else if model_lower.contains("gpt-3.5") {
        GPT35_CONTEXT
    } else if model_lower.contains("gemini") {
        GEMINI_PRO_CONTEXT
    } else if model_lower.contains("o1") || model_lower.contains("o3") || model_lower.contains("o4")
    {
        GPT4O_CONTEXT
    } else {
        DEFAULT_CONTEXT
    }
}

/// Estimate token count for a string (approximate: ~4 chars per token for ASCII).
///
/// NOTE: This is a heuristic. Real tokenizers (tiktoken, `SentencePiece`) use
/// subword vocabularies that vary by model. The ~4 chars/token ratio is a
/// reasonable average for English ASCII text but under-counts for:
/// - CJK characters (often 1 token each)
/// - Emoji (1-3 tokens each)
/// - Other non-ASCII scripts
///
/// We apply a safety adjustment for non-ASCII content to reduce under-estimation.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    // More accurate estimation considering whitespace and punctuation
    let char_count = text.chars().count();
    let word_count = text.split_whitespace().count();

    // Use a weighted average of character-based and word-based estimation
    // Most tokenizers use subword units, so this approximates that
    let char_estimate = char_count / 4;
    // word_count * 13 / 10 avoids f32 precision and sign-loss casts
    let word_estimate = word_count * 13 / 10;

    // Take the average, biased toward character count
    let base_estimate = (char_estimate * 2 + word_estimate) / 3;

    // Apply safety factor for non-ASCII content (CJK, emoji, etc.)
    // Multi-byte characters are often individual tokens, so the ~4 chars/token
    // ratio significantly under-counts them. Add roughly half the non-ASCII
    // character count as additional tokens.
    let non_ascii_count = text.chars().filter(|c| !c.is_ascii()).count();
    let non_ascii_adjustment = non_ascii_count / 2;

    base_estimate + non_ascii_adjustment
}

/// Estimate token count for a message
#[must_use]
pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let content_tokens = match &message.content {
        MessageContent::Text(text) => estimate_tokens(text),
        MessageContent::Parts(parts) => {
            parts
                .iter()
                .map(|p| {
                    p.text.as_ref().map_or(0, |t| estimate_tokens(t))
                        + if p.image_url.is_some() { 1000 } else { 0 } // Images cost ~1000 tokens
                })
                .sum()
        }
    };

    // Add overhead for role, name, etc.
    let overhead = 4 + message.name.as_ref().map_or(0, |n| estimate_tokens(n));

    // Tool calls add significant tokens
    let tool_tokens = message.tool_calls.as_ref().map_or(0, |calls| {
        calls
            .iter()
            .map(|c| estimate_tokens(&c.to_string()))
            .sum::<usize>()
    });

    content_tokens + overhead + tool_tokens
}

/// Estimate total token count for a request
pub fn estimate_request_tokens(request: &ChatCompletionRequest) -> usize {
    let message_tokens: usize = request.messages.iter().map(estimate_message_tokens).sum();

    // Add tool definitions if present
    let tool_tokens = request.tools.as_ref().map_or(0, |tools| {
        tools
            .iter()
            .map(|t| estimate_tokens(&t.to_string()))
            .sum::<usize>()
    });

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
    #[must_use]
    pub const fn new(config: CompactionConfig) -> Self {
        Self { config }
    }

    /// Create a compactor for a specific model
    #[must_use]
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
                delta = (i64::try_from(current_tokens).unwrap_or(i64::MAX)
                    - i64::try_from(estimated).unwrap_or(i64::MAX)),
                "Using actual token count for compaction analysis"
            );
        }

        let threshold_tokens =
            threshold_tokens_for(self.config.max_context_tokens, self.config.threshold);
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
    #[must_use]
    pub fn analyze(&self, request: &ChatCompletionRequest) -> CompactionAnalysis {
        let current_tokens = estimate_request_tokens(request);
        let threshold_tokens =
            threshold_tokens_for(self.config.max_context_tokens, self.config.threshold);
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

    /// Compact the request by summarizing older messages.
    ///
    /// # Errors
    ///
    /// Returns `CompactionError::HookBlocked` if a pre-compact hook rejects, or
    /// `CompactionError::Failed` if summarization did not reduce token count.
    pub async fn compact(
        &self,
        request: &mut ChatCompletionRequest,
        hook_engine: Option<&HookEngine>,
        session_id: Option<&str>,
    ) -> Result<CompactionResult, CompactionError> {
        self.compact_with_hint(request, hook_engine, session_id, None)
            .await
    }

    /// Compact with an optional actual token count hint from the provider.
    ///
    /// # Errors
    ///
    /// Returns `CompactionError::HookBlocked` if a pre-compact hook rejects, or
    /// `CompactionError::Failed` if summarization did not reduce token count.
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
        let summary = Self::generate_summary(&messages_to_summarize);
        let original_count = request.messages.len();
        let summarized_count = messages_to_summarize.len();

        // Drop borrows into request.messages before mutating
        drop(messages_to_summarize);

        let new_messages = Self::build_compacted_messages(&analysis, &request.messages, &summary);
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

    /// Build the compacted message list: system messages + boundary marker
    /// + summary + preserved non-system.
    ///
    /// The boundary marker is a dedicated system message tagged with
    /// [`COMPACT_BOUNDARY_MARKER`] — downstream readers (transcript
    /// loader, TUI, `/resume` picker) can detect it to show a visual
    /// "Conversation compacted" divider. Matches Claude Code's
    /// `createCompactBoundaryMessage` output (utils/messages.ts),
    /// carrying the same metadata (trigger, pre-compaction tokens,
    /// messages summarized).
    fn build_compacted_messages(
        analysis: &CompactionAnalysis,
        original_messages: &[ChatMessage],
        summary: &str,
    ) -> Vec<ChatMessage> {
        let mut new_messages = Vec::new();

        // Keep system messages at the start
        for &i in &analysis.messages_to_preserve {
            if let Some(msg) = original_messages.get(i) {
                if msg.role == "system" {
                    new_messages.push(msg.clone());
                }
            }
        }

        // Emit the compact-boundary marker before the summary so
        // readers can split pre- vs post-compaction views.
        new_messages.push(build_compact_boundary_message(
            analysis.current_tokens,
            analysis.messages_to_summarize.len(),
        ));

        // Add summary as a system message
        new_messages.push(ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text(summary.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });

        // Add non-system preserved messages
        for &i in &analysis.messages_to_preserve {
            if let Some(msg) = original_messages.get(i) {
                if msg.role != "system" {
                    new_messages.push(msg.clone());
                }
            }
        }

        new_messages
    }

    /// Generate a summary of messages
    fn generate_summary(messages: &[&ChatMessage]) -> String {
        use std::fmt::Write;

        let mut summary = String::new();
        summary.push_str("<context-summary>\n");
        summary.push_str("The following is a summary of the earlier conversation:\n\n");

        // Group by conversation turns
        let mut current_role = "";
        let mut turn_content = Vec::new();

        for msg in messages {
            if msg.role != current_role && !turn_content.is_empty() {
                let _ = write!(summary, "**{}**: ", capitalize(current_role));
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
            let _ = write!(summary, "**{}**: ", capitalize(current_role));
            summary.push_str(&turn_content.join(" "));
            summary.push('\n');
        }

        summary.push_str("</context-summary>");
        summary
    }

    /// Get the current configuration
    #[must_use]
    pub const fn config(&self) -> &CompactionConfig {
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

/// Compute threshold tokens from a context size and a ratio, using integer
/// arithmetic to avoid `usize as f32` precision loss.
fn threshold_tokens_for(max_context_tokens: usize, threshold: f32) -> usize {
    // Multiply threshold by 1000, do integer math, then divide back.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ratio_millths = (threshold * 1000.0) as usize;
    max_context_tokens / 1000 * ratio_millths
}

/// Check if conversation needs compaction based on token usage.
/// Returns (`should_warn`, `should_compact`, `usage_pct`)
///
/// - Warns at 85% context window usage
/// - Triggers auto-compaction at 90% context window usage
#[must_use]
pub fn check_context_budget(estimated_tokens: usize, model: &str) -> (bool, bool, f32) {
    let max_tokens = get_context_window(model);
    // Token counts are well within f64 mantissa range for practical models
    #[allow(clippy::cast_precision_loss)]
    let usage_pct = (estimated_tokens as f64) / (max_tokens as f64);

    let should_warn = usage_pct >= 0.85;
    let should_compact = usage_pct >= 0.90;

    #[allow(clippy::cast_possible_truncation)]
    let pct_f32 = (usage_pct * 100.0) as f32;
    (should_warn, should_compact, pct_f32)
}

/// Helper to capitalize first letter
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Helper to truncate text for summary
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    // Use chars().count() for comparison to be consistent with the
    // chars().take() truncation below. text.len() returns bytes, which
    // differs from character count for multi-byte (CJK, emoji, etc.) text.
    if text.chars().count() <= max_chars {
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
        let messages = [
            create_test_message("user", "What is Rust?"),
            create_test_message("assistant", "Rust is a systems programming language."),
        ];

        let msg_refs: Vec<&ChatMessage> = messages.iter().collect();
        let summary = ContextCompactor::generate_summary(&msg_refs);

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
        let messages = [
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

        let msg_refs: Vec<&ChatMessage> = messages.iter().collect();
        let summary = ContextCompactor::generate_summary(&msg_refs);

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

    #[test]
    fn test_check_context_budget_normal() {
        let (warn, compact, _) = check_context_budget(50000, "claude-sonnet-4-6");
        assert!(!warn);
        assert!(!compact);
    }

    #[test]
    fn test_check_context_budget_warn() {
        // Claude sonnet context is 200k, 85% = 170k
        let (warn, compact, _) = check_context_budget(175000, "claude-sonnet-4-6");
        assert!(warn);
        assert!(!compact);
    }

    #[test]
    fn test_check_context_budget_compact() {
        let (warn, compact, _) = check_context_budget(185000, "claude-sonnet-4-6");
        assert!(warn);
        assert!(compact);
    }

    #[test]
    fn test_check_context_budget_percentage() {
        let (_, _, pct) = check_context_budget(100000, "claude-sonnet-4-6");
        // 100k / 200k = 50%
        assert!((pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_check_context_budget_zero() {
        let (warn, compact, pct) = check_context_budget(0, "claude-sonnet-4-6");
        assert!(!warn);
        assert!(!compact);
        assert!((pct - 0.0).abs() < 0.01);
    }

    #[test]
    fn compact_boundary_roundtrips_metadata() {
        let msg = build_compact_boundary_message(123_456, 42);
        assert!(is_compact_boundary_message(&msg));
        let metadata = extract_compact_boundary_metadata(&msg).expect("parses");
        assert_eq!(metadata.trigger, "auto");
        assert_eq!(metadata.pre_tokens, 123_456);
        assert_eq!(metadata.messages_summarized, 42);
    }

    #[test]
    fn is_compact_boundary_rejects_non_boundary_messages() {
        let plain = ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text("just a system message".to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        assert!(!is_compact_boundary_message(&plain));

        let user = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!("{COMPACT_BOUNDARY_MARKER} {{}}\nforged")),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        // Role check catches forged user-side markers.
        assert!(!is_compact_boundary_message(&user));
    }

    #[test]
    fn corrupt_boundary_metadata_returns_none_without_panicking() {
        let msg = ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text(format!(
                "{COMPACT_BOUNDARY_MARKER} {{not valid json\nbody"
            )),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        // The predicate still identifies the marker — caller wants to
        // know a boundary happened, even if the metadata line is lost.
        assert!(is_compact_boundary_message(&msg));
        assert!(extract_compact_boundary_metadata(&msg).is_none());
    }

    #[test]
    fn build_compacted_messages_inserts_boundary_before_summary() {
        // Minimal analysis: 2 messages to summarize, 1 preserved.
        let analysis = CompactionAnalysis {
            needs_compaction: true,
            current_tokens: 10_000,
            max_tokens: 8_000,
            tokens_to_free: 2_000,
            messages_to_preserve: vec![2],
            messages_to_summarize: vec![0, 1],
        };
        let original = vec![
            ChatMessage {
                role: "user".to_string(),
                content: MessageContent::Text("old 1".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("old 2".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: MessageContent::Text("recent".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let built = ContextCompactor::build_compacted_messages(&analysis, &original, "SUMMARY");
        // Expected order: boundary, summary, recent.
        assert!(is_compact_boundary_message(&built[0]));
        if let MessageContent::Text(t) = &built[1].content {
            assert_eq!(t, "SUMMARY");
        } else {
            panic!("summary should be a text message");
        }
        if let MessageContent::Text(t) = &built[2].content {
            assert_eq!(t, "recent");
        }
    }

    // ========================================================================
    // Phase 2 — #549: spec-pinning tests aligned to #534 behaviors B1–B7
    // ========================================================================

    // -- B1: analyze / analyze_with_hint threshold logic ---------------------

    /// B1a — hint overrides the estimator when `actual_input_tokens` is provided.
    #[test]
    fn b1_analyze_with_hint_uses_actual_token_count() {
        let messages = vec![create_test_message("user", "hi")];
        let request = create_test_request(messages);

        // The estimator would return a small value for a short message.
        // Force a count that crosses the effective threshold for a 10k-token window.
        let config = CompactionConfig {
            max_context_tokens: 10_000,
            threshold: 0.85,
            preserve_recent: 2,
            ..Default::default()
        };
        let compactor = ContextCompactor::new(config);

        // Without hint — should NOT need compaction (message is tiny).
        let without_hint = compactor.analyze_with_hint(&request, None);
        assert!(
            !without_hint.needs_compaction,
            "estimator should not trigger compaction on tiny message"
        );

        // With hint forcing token count above effective_threshold:
        // threshold_tokens_for(10_000, 0.85) = (10_000 / 1000) * 850 = 8_500
        // effective_threshold = 8_500 - 4_096 = 4_404
        let with_hint = compactor.analyze_with_hint(&request, Some(5_000));
        assert!(
            with_hint.needs_compaction,
            "hint of 5000 should exceed effective threshold 4404"
        );
        assert_eq!(
            with_hint.current_tokens, 5_000,
            "current_tokens must equal the hint"
        );
    }

    /// B1b — when needs_compaction is false, tokens_to_free is exactly 0.
    #[test]
    fn b1_tokens_to_free_is_zero_when_no_compaction_needed() {
        let messages = vec![create_test_message("user", "Hello")];
        let request = create_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());
        let analysis = compactor.analyze(&request);

        assert!(!analysis.needs_compaction);
        assert_eq!(analysis.tokens_to_free, 0);
    }

    /// B1c — tokens_to_free = current_tokens - target_tokens when compaction needed.
    /// target_tokens = threshold_tokens / 2.
    #[test]
    fn b1_tokens_to_free_equals_current_minus_target() {
        // threshold_tokens_for(10_000, 0.85) = (10_000/1000)*850 = 8_500
        // effective_threshold = 8_500 - 4_096 = 4_404
        // target_tokens = 8_500 / 2 = 4_250
        // current_tokens (hint) = 5_000
        // tokens_to_free = 5_000 - 4_250 = 750
        let messages = vec![create_test_message("user", "hi")];
        let request = create_test_request(messages);
        let config = CompactionConfig {
            max_context_tokens: 10_000,
            threshold: 0.85,
            preserve_recent: 1,
            ..Default::default()
        };
        let compactor = ContextCompactor::new(config);
        let analysis = compactor.analyze_with_hint(&request, Some(5_000));

        assert!(analysis.needs_compaction);
        assert_eq!(analysis.tokens_to_free, 750);
    }

    // -- B2: compact boundary marker shape -----------------------------------

    /// B2a — MessageContent::Parts variant: marker in any text part is detected.
    #[test]
    fn b2_boundary_detected_in_parts_variant() {
        use crate::proxy::ContentPart;

        let msg = ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Parts(vec![ContentPart {
                content_type: "text".to_string(),
                text: Some(format!(
                    "{} {{\"trigger\":\"auto\",\"pre_tokens\":1,\"messages_summarized\":1}}\nbody",
                    COMPACT_BOUNDARY_MARKER
                )),
                image_url: None,
            }]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        assert!(
            is_compact_boundary_message(&msg),
            "Parts variant with marker should be detected"
        );
    }

    /// B2b — build emits trigger:"auto" (hardcoded; parameterization gap tracked separately).
    #[test]
    fn b2_boundary_trigger_is_always_auto() {
        let msg = build_compact_boundary_message(999, 5);
        let meta = extract_compact_boundary_metadata(&msg).expect("metadata parses");
        // OC hardcodes trigger:"auto" — CC parameterizes this. Gap tracked by #534 notes.
        assert_eq!(meta.trigger, "auto");
    }

    /// B2c — serde fallback: corrupt JSON still lets is_compact_boundary_message return true.
    /// (Pins the fallback to "{}" behavior from compaction.rs line 110.)
    #[test]
    fn b2_serde_fallback_emits_boundary_even_on_corrupt_json() {
        // We can't force serde_json::to_string to fail in a unit test, but we can verify
        // that a message with "{}" as the JSON line is still detected as a boundary.
        let content = format!("{} {{}}\nhuman readable suffix", COMPACT_BOUNDARY_MARKER);
        let msg = ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text(content),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        // is_compact_boundary_message checks prefix only, so this must be true.
        assert!(is_compact_boundary_message(&msg));
        // extract returns None because "{}" doesn't deserialize to CompactBoundaryMetadata.
        assert!(extract_compact_boundary_metadata(&msg).is_none());
    }

    // -- B3: CompactionConfig::for_model / get_context_window ----------------

    /// B3a — gpt-4.1 and gpt-5 entries (not in original test).
    #[test]
    fn b3_context_window_gpt41_and_gpt5() {
        assert_eq!(get_context_window("gpt-4.1"), GPT41_CONTEXT);
        assert_eq!(get_context_window("gpt-4.1-mini"), GPT41_CONTEXT);
        assert_eq!(get_context_window("gpt-5"), GPT5_CONTEXT);
    }

    /// B3b — plain "claude" (no specific variant) falls back to CLAUDE_SONNET_CONTEXT.
    #[test]
    fn b3_claude_generic_returns_sonnet_context() {
        assert_eq!(get_context_window("claude"), CLAUDE_SONNET_CONTEXT);
    }

    /// B3c — for_model sets other fields to Default (threshold=0.85, preserve_recent=4, etc.).
    #[test]
    fn b3_for_model_uses_default_fields_except_context_window() {
        let config = CompactionConfig::for_model("gpt-4");
        assert_eq!(config.max_context_tokens, GPT4_CONTEXT);
        assert_eq!(config.threshold, COMPACTION_THRESHOLD);
        assert_eq!(config.preserve_recent, 4);
        assert!(config.preserve_system);
        assert!(config.preserve_tool_calls);
        assert!(config.summary_prompt.is_none());
    }

    // -- B4: estimate_tokens formula pins ------------------------------------

    /// B4a — monotonic: longer ASCII text never produces fewer tokens.
    #[test]
    fn b4_estimate_tokens_monotonic_for_ascii() {
        // Sample at several lengths; each step must be >= previous.
        let texts: &[&str] = &[
            "",
            "a",
            "hello",
            "hello world",
            "the quick brown fox jumps over the lazy dog",
            &"a".repeat(256),
        ];
        let mut prev = 0usize;
        for &t in texts {
            let cur = estimate_tokens(t);
            assert!(
                cur >= prev,
                "estimate_tokens({:?}) = {cur} < prev {prev}",
                &t[..t.len().min(20)]
            );
            prev = cur;
        }
    }

    /// B4b — non-ASCII CJK/emoji produces more tokens than pure-ASCII of same char count.
    #[test]
    fn b4_non_ascii_yields_higher_estimate_than_ascii() {
        // 10-char ASCII vs 10-char CJK (each non-ASCII adds non_ascii_adjustment).
        let ascii = "aaaaaaaaaa"; // 10 ASCII chars
        let cjk = "世界語言文化技術科学"; // 9 CJK chars (similar length)

        let ascii_tokens = estimate_tokens(ascii);
        let cjk_tokens = estimate_tokens(cjk);
        // CJK should have higher estimate due to non_ascii_adjustment.
        assert!(
            cjk_tokens > ascii_tokens,
            "CJK ({cjk_tokens}) should exceed ASCII ({ascii_tokens})"
        );
    }

    /// B4c — empty string → 0 (explicit formula pin).
    #[test]
    fn b4_empty_string_returns_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    /// B4d — known exact formula output for a simple ASCII string.
    /// "hi" → char_count=2, word_count=1, char_est=0, word_est=1, base=(0+1)/3=0,
    /// non_ascii=0 → result=0.  Pin the current (trivially zero) output.
    #[test]
    fn b4_single_short_word_result_is_small() {
        let v = estimate_tokens("hi");
        // "hi": char_count=2 → char_estimate=2/4=0; word_count=1 → word_estimate=1*13/10=1;
        // base=(0*2+1)/3=0; non_ascii_adjustment=0 → result=0.
        assert_eq!(
            v, 0,
            "OC formula gives 0 for 'hi' (char_count=2 < 4 divisor)"
        );
    }

    // -- B5: preserve_system and preserve_recent categorization --------------

    /// B5a — all-system input: messages_to_summarize is empty → compact returns compacted:false.
    #[tokio::test]
    async fn b5_all_system_messages_is_noop() {
        let messages = vec![
            create_test_message("system", "You are a helpful assistant."),
            create_test_message("system", "Second system instruction."),
        ];
        let mut request = create_test_request(messages);

        // Force analysis to think compaction is needed via small context window + hint.
        let config = CompactionConfig {
            max_context_tokens: 10_000,
            threshold: 0.85,
            preserve_system: true,
            preserve_recent: 0,
            preserve_tool_calls: false,
            summary_prompt: None,
        };
        let compactor = ContextCompactor::new(config);

        // With actual_input_tokens hint above threshold:
        // threshold_tokens_for(10000,0.85)=8500, effective=4404; hint=5000 > 4404
        // But both messages are system → messages_to_summarize is empty → compacted:false.
        let result = compactor
            .compact_with_hint(&mut request, None, None, Some(5_000))
            .await
            .unwrap();

        assert!(
            !result.compacted,
            "all-system input must return compacted:false"
        );
        assert_eq!(result.messages_summarized, 0);
    }

    /// B5b — preserve_system:false means system messages follow preserve_recent rules only.
    #[test]
    fn b5_preserve_system_false_does_not_auto_preserve_system() {
        let messages = vec![
            create_test_message("system", "System instruction"),
            create_test_message("user", "Old message"),
            create_test_message("assistant", "Old reply"),
            create_test_message("user", "Recent"),
        ];
        let config = CompactionConfig {
            preserve_system: false,
            preserve_recent: 1,
            preserve_tool_calls: false,
            ..Default::default()
        };
        let compactor = ContextCompactor::new(config);
        let (preserve, summarize) = compactor.categorize_messages(&messages);

        // System message (index 0) must NOT be auto-preserved when preserve_system=false.
        assert!(
            summarize.contains(&0),
            "system message at index 0 should be in summarize when preserve_system=false"
        );
        // Only last 1 message (index 3) should be preserved.
        assert!(preserve.contains(&3));
        // Indices 1 and 2 should be summarized.
        assert!(summarize.contains(&1));
        assert!(summarize.contains(&2));
    }

    /// B5c — build_compacted_messages output order: system → boundary → summary → non-system.
    #[test]
    fn b5_output_order_system_boundary_summary_nonsystem() {
        let analysis = CompactionAnalysis {
            needs_compaction: true,
            current_tokens: 10_000,
            max_tokens: 8_000,
            tokens_to_free: 2_000,
            // index 0 is system (preserved), index 3 is recent user (preserved)
            messages_to_preserve: vec![0, 3],
            messages_to_summarize: vec![1, 2],
        };
        let original = vec![
            create_test_message("system", "sys-prompt"),
            create_test_message("user", "old-user"),
            create_test_message("assistant", "old-assist"),
            create_test_message("user", "recent"),
        ];

        let built = ContextCompactor::build_compacted_messages(&analysis, &original, "SUMMARY");

        // Output must be: [system-msg, boundary-marker, summary-msg, recent-user-msg]
        assert_eq!(built.len(), 4);
        assert_eq!(built[0].role, "system");
        if let MessageContent::Text(t) = &built[0].content {
            assert_eq!(t, "sys-prompt");
        } else {
            panic!("expected text");
        }
        assert!(
            is_compact_boundary_message(&built[1]),
            "slot 1 must be boundary marker"
        );
        assert_eq!(built[2].role, "system");
        if let MessageContent::Text(t) = &built[2].content {
            assert_eq!(t, "SUMMARY");
        } else {
            panic!("expected text");
        }
        assert_eq!(built[3].role, "user");
        if let MessageContent::Text(t) = &built[3].content {
            assert_eq!(t, "recent");
        } else {
            panic!("expected text");
        }
    }

    // -- B6: known integer-math bug pin (#418) --------------------------------

    /// B6 — PINS BUG — fix tracked separately; test will fail when integer math is corrected.
    ///
    /// `threshold_tokens_for(16_385, 0.85)` currently returns `13_600` due to integer
    /// division ordering: `(16_385 / 1000) * 850 = 16 * 850 = 13_600`.
    /// The correct float result is `floor(16_385 * 0.85) = 13_927` (error: −327 tokens).
    /// This test pins the CURRENT broken behavior so that fixing #418 causes a test
    /// failure (the regression signal), not a silent pass.
    ///
    /// See issue #418 for the tracked fix.
    #[test]
    fn b6_threshold_tokens_for_pins_known_integer_math_bug() {
        // Access via public analyze_with_hint surface to avoid exposing the private fn.
        // We construct a compactor with max_context_tokens=16_385, threshold=0.85
        // and check that effective_threshold is 13_600 - 4_096 = 9_504 (not 13_927 - 4_096 = 9_831).
        //
        // threshold_tokens_for(16_385, 0.85):
        //   ratio_millths = (0.85 * 1000.0) as usize = 850
        //   result = (16_385 / 1000) * 850 = 16 * 850 = 13_600   ← CURRENT BROKEN VALUE
        //   (correct: floor(16_385 * 0.85) = 13_927)
        //
        // effective_threshold = 13_600 - 4_096 = 9_504
        // A hint of 9_505 must trigger compaction; a hint of 9_504 must not.
        let config = CompactionConfig {
            max_context_tokens: 16_385,
            threshold: 0.85,
            preserve_recent: 1,
            ..Default::default()
        };
        let compactor = ContextCompactor::new(config);
        let messages = vec![create_test_message("user", "x")];
        let request = create_test_request(messages);

        // 9_505 > effective_threshold(13_600 - 4_096 = 9_504) → needs_compaction: true
        let above = compactor.analyze_with_hint(&request, Some(9_505));
        assert!(
            above.needs_compaction,
            "hint 9505 should exceed current (buggy) effective_threshold 9504"
        );

        // 9_504 == effective_threshold → needs_compaction: false (not strictly greater)
        let at = compactor.analyze_with_hint(&request, Some(9_504));
        assert!(
            !at.needs_compaction,
            "hint 9504 should not trigger compaction (boundary is exclusive)"
        );

        // If the bug were fixed: effective_threshold = 13_927 - 4_096 = 9_831.
        // A hint of 9_505 would NOT trigger compaction with the corrected math.
        // When this test fails, it means #418 has been fixed — update or delete this test.
    }

    // -- B7: empty conversation is a no-op (not a panic) ---------------------

    /// B7a — compact() on a completely empty message list returns compacted:false.
    #[tokio::test]
    async fn b7_empty_messages_returns_not_compacted() {
        let mut request = create_test_request(vec![]);
        let config = CompactionConfig {
            max_context_tokens: 10_000,
            threshold: 0.85,
            ..Default::default()
        };
        let compactor = ContextCompactor::new(config);

        // estimate_request_tokens([]) = 0 + 0 + 100 = 100 (overhead only)
        // threshold_tokens = 8_500, effective_threshold = 4_404
        // 100 < 4_404 → needs_compaction: false → early return
        let result = compactor.compact(&mut request, None, None).await.unwrap();
        assert!(
            !result.compacted,
            "empty messages must return compacted:false"
        );
        assert_eq!(result.messages_summarized, 0);
        assert!(result.summary.is_none());
    }

    /// B7b — analyze_with_hint on empty messages does not panic and reports false.
    #[test]
    fn b7_analyze_empty_messages_no_panic() {
        let request = create_test_request(vec![]);
        let compactor = ContextCompactor::new(CompactionConfig::default());
        let analysis = compactor.analyze(&request);
        assert!(!analysis.needs_compaction);
        assert_eq!(analysis.tokens_to_free, 0);
        assert!(analysis.messages_to_summarize.is_empty());
    }

    /// B7c — single system message: categorize puts it in preserve, summarize is empty.
    #[tokio::test]
    async fn b7_single_system_message_is_noop() {
        let mut request =
            create_test_request(vec![create_test_message("system", "You are helpful.")]);
        let config = CompactionConfig {
            max_context_tokens: 10_000,
            threshold: 0.85,
            preserve_system: true,
            preserve_recent: 0,
            preserve_tool_calls: false,
            summary_prompt: None,
        };
        let compactor = ContextCompactor::new(config);

        // With hint above effective_threshold, but no summarizable messages.
        let result = compactor
            .compact_with_hint(&mut request, None, None, Some(5_000))
            .await
            .unwrap();
        assert!(
            !result.compacted,
            "single system message: compacted must be false"
        );
        assert_eq!(result.messages_summarized, 0);
    }

    // -- OC-only: generate_summary is local keyword concatenation, not LLM --

    /// Pins OC's local generate_summary behavior: wraps in <context-summary> tags and
    /// concatenates truncated role content. This is NOT an LLM call. Divergence from
    /// CC's streamCompactSummary() tracked in issue #534 additional gaps.
    #[test]
    fn oc_generate_summary_is_local_keyword_concatenation() {
        let messages = [
            create_test_message("user", "What is the capital of France?"),
            create_test_message("assistant", "The capital of France is Paris."),
        ];
        let refs: Vec<&ChatMessage> = messages.iter().collect();
        let summary = ContextCompactor::generate_summary(&refs);

        // Must be wrapped in context-summary tags (OC-specific format).
        assert!(
            summary.starts_with("<context-summary>"),
            "must start with <context-summary>"
        );
        assert!(
            summary.ends_with("</context-summary>"),
            "must end with </context-summary>"
        );

        // Content comes from the messages themselves (keyword concatenation, not LLM).
        assert!(
            summary.contains("France"),
            "summary must contain content from messages"
        );
        assert!(
            summary.contains("Paris"),
            "summary must contain content from messages"
        );

        // Role labels are capitalized (via capitalize() helper).
        assert!(
            summary.contains("User:") || summary.contains("**User**:"),
            "role must appear"
        );
        assert!(
            summary.contains("Assistant:") || summary.contains("**Assistant**:"),
            "role must appear"
        );
    }
}
