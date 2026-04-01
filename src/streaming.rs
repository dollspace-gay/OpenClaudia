//! Structured streaming events for provider-agnostic response handling.
//!
//! This module provides a unified [`StreamEvent`] enum that standardizes
//! streaming output across all providers (Anthropic, `OpenAI`, Gemini).
//! Each provider's SSE format is parsed into the same event types,
//! enabling provider-agnostic downstream handling.

use serde::{Deserialize, Serialize};

/// Unified streaming event enum across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    /// API request started
    RequestStart { request_id: Option<String> },

    /// Text content delta
    TextDelta { text: String },

    /// Thinking/reasoning delta (extended thinking, o1 reasoning, etc.)
    ThinkingDelta { text: String },

    /// Thinking block completed
    ThinkingComplete { duration_ms: Option<u64> },

    /// Tool use block started
    ToolUseStart { id: String, name: String },

    /// Partial JSON input for tool use
    ToolUseInputDelta { partial_json: String },

    /// Tool use block completed with full input
    ToolUseComplete {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// Tool execution result
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },

    /// Token usage update
    Usage(TokenUsage),

    /// Stream completed
    Done { stop_reason: StopReason },

    /// Error during streaming
    Error { message: String, retryable: bool },
}

/// Why the stream stopped
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StopReason {
    /// Model finished naturally
    EndTurn,
    /// Model wants to use tools
    ToolUse,
    /// Hit max tokens
    MaxTokens,
    /// User cancelled
    UserCancelled,
    /// Content filter triggered
    ContentFilter,
    /// Unknown/other reason
    Other(String),
}

/// Token usage from a streaming response
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
}

/// Parse provider-specific SSE data into [`StreamEvent`]s.
///
/// Each method accepts the parsed JSON from an SSE `data:` line and returns
/// zero or more [`StreamEvent`]s. Missing or malformed fields are handled
/// gracefully -- the parser never panics, it simply returns an empty vec.
pub struct StreamParser;

impl StreamParser {
    /// Parse an Anthropic SSE event into [`StreamEvent`]s.
    ///
    /// `event_type` is the Anthropic event type string (e.g. `"content_block_delta"`,
    /// `"message_start"`, `"message_delta"`). `data` is the parsed JSON payload.
    pub fn parse_anthropic(event_type: &str, data: &serde_json::Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        match event_type {
            "message_start" => {
                let request_id = data
                    .pointer("/message/id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                events.push(StreamEvent::RequestStart { request_id });
            }
            "content_block_start" => {
                if let Some(block) = data.get("content_block") {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("tool_use") => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            events.push(StreamEvent::ToolUseStart { id, name });
                        }
                        Some("thinking") => {
                            // Emit an empty thinking delta to signal the start of a thinking block
                            events.push(StreamEvent::ThinkingDelta {
                                text: String::new(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = data.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::TextDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::ThinkingDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                        "input_json_delta" => {
                            if let Some(json) = delta.get("partial_json").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::ToolUseInputDelta {
                                    partial_json: json.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = data.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                        let stop_reason = match reason {
                            "end_turn" => StopReason::EndTurn,
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::Other(reason.to_string()),
                        };
                        events.push(StreamEvent::Done { stop_reason });
                    }
                }
                if let Some(usage) = data.get("usage") {
                    events.push(StreamEvent::Usage(TokenUsage {
                        input_tokens: usage
                            .get("input_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        output_tokens: usage
                            .get("output_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        cache_creation_tokens: usage
                            .get("cache_creation_input_tokens")
                            .and_then(serde_json::Value::as_u64),
                        cache_read_tokens: usage
                            .get("cache_read_input_tokens")
                            .and_then(serde_json::Value::as_u64),
                    }));
                }
            }
            _ => {}
        }
        events
    }

    /// Parse an `OpenAI` SSE chunk into [`StreamEvent`]s.
    ///
    /// `data` is the parsed JSON from an `OpenAI` streaming `data:` line
    /// (the `choices[].delta` format used by chat completions).
    #[must_use]
    pub fn parse_openai(data: &serde_json::Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(choices) = data.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    // Text content
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            events.push(StreamEvent::TextDelta {
                                text: content.to_string(),
                            });
                        }
                    }
                    // Reasoning/thinking content (o1, o3)
                    if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str())
                    {
                        if !reasoning.is_empty() {
                            events.push(StreamEvent::ThinkingDelta {
                                text: reasoning.to_string(),
                            });
                        }
                    }
                    // Tool calls
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            if let Some(function) = tc.get("function") {
                                if let Some(name) = function.get("name").and_then(|n| n.as_str()) {
                                    let id = tc
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    events.push(StreamEvent::ToolUseStart {
                                        id,
                                        name: name.to_string(),
                                    });
                                }
                                if let Some(args) =
                                    function.get("arguments").and_then(|a| a.as_str())
                                {
                                    if !args.is_empty() {
                                        events.push(StreamEvent::ToolUseInputDelta {
                                            partial_json: args.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                // Finish reason
                if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                    let stop_reason = match reason {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        "content_filter" => StopReason::ContentFilter,
                        _ => StopReason::Other(reason.to_string()),
                    };
                    events.push(StreamEvent::Done { stop_reason });
                }
            }
        }
        // Usage (if present, e.g. with stream_options.include_usage)
        if let Some(usage) = data.get("usage") {
            events.push(StreamEvent::Usage(TokenUsage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_creation_tokens: None,
                cache_read_tokens: None,
            }));
        }
        events
    }

    /// Parse a Gemini SSE chunk into [`StreamEvent`]s.
    ///
    /// `data` is the parsed JSON from a Gemini streaming response
    /// (the `candidates[].content.parts[]` format).
    #[must_use]
    pub fn parse_gemini(data: &serde_json::Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(candidates) = data.get("candidates").and_then(|c| c.as_array()) {
            for candidate in candidates {
                if let Some(content) = candidate.get("content") {
                    if let Some(parts) = content.get("parts").and_then(|p| p.as_array()) {
                        for part in parts {
                            // Text content
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::TextDelta {
                                    text: text.to_string(),
                                });
                            }
                            // Function calls (Gemini sends complete calls, not streamed deltas)
                            if let Some(fc) = part.get("functionCall") {
                                let name = fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let args = fc.get("args").cloned().unwrap_or_else(|| {
                                    serde_json::Value::Object(serde_json::Map::default())
                                });
                                let id = format!("gemini_{name}");
                                events.push(StreamEvent::ToolUseStart {
                                    id: id.clone(),
                                    name: name.clone(),
                                });
                                events.push(StreamEvent::ToolUseComplete {
                                    id,
                                    name,
                                    input: args,
                                });
                            }
                            // Gemini thinking (Gemini 2.5 Flash/Pro)
                            if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::ThinkingDelta {
                                    text: thought.to_string(),
                                });
                            }
                        }
                    }
                }
                // Finish reason
                if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                    let stop_reason = match reason {
                        "STOP" => StopReason::EndTurn,
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        "SAFETY" => StopReason::ContentFilter,
                        _ => StopReason::Other(reason.to_string()),
                    };
                    events.push(StreamEvent::Done { stop_reason });
                }
            }
        }
        // Usage metadata
        if let Some(usage) = data.get("usageMetadata") {
            events.push(StreamEvent::Usage(TokenUsage {
                input_tokens: usage
                    .get("promptTokenCount")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                output_tokens: usage
                    .get("candidatesTokenCount")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_creation_tokens: None,
                cache_read_tokens: None,
            }));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Anthropic parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_anthropic_message_start() {
        let data = json!({"message": {"id": "msg_123"}});
        let events = StreamParser::parse_anthropic("message_start", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::RequestStart { request_id } => {
                assert_eq!(request_id.as_deref(), Some("msg_123"));
            }
            other => panic!("Expected RequestStart, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_text_delta() {
        let data = json!({"delta": {"type": "text_delta", "text": "Hello"}});
        let events = StreamParser::parse_anthropic("content_block_delta", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_thinking_delta() {
        let data = json!({"delta": {"type": "thinking_delta", "thinking": "Let me think..."}});
        let events = StreamParser::parse_anthropic("content_block_delta", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "Let me think..."),
            other => panic!("Expected ThinkingDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_tool_use_start() {
        let data = json!({
            "content_block": {
                "type": "tool_use",
                "id": "toolu_abc",
                "name": "bash"
            }
        });
        let events = StreamParser::parse_anthropic("content_block_start", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUseStart { id, name } => {
                assert_eq!(id, "toolu_abc");
                assert_eq!(name, "bash");
            }
            other => panic!("Expected ToolUseStart, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_input_json_delta() {
        let data = json!({"delta": {"type": "input_json_delta", "partial_json": "{\"cmd\":"}});
        let events = StreamParser::parse_anthropic("content_block_delta", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUseInputDelta { partial_json } => {
                assert_eq!(partial_json, "{\"cmd\":");
            }
            other => panic!("Expected ToolUseInputDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_stop_reason() {
        let data = json!({
            "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });
        let events = StreamParser::parse_anthropic("message_delta", &data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn
            }
        )));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage(_))));
    }

    #[test]
    fn test_parse_anthropic_tool_use_stop_reason() {
        let data = json!({"delta": {"stop_reason": "tool_use"}});
        let events = StreamParser::parse_anthropic("message_delta", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, &StopReason::ToolUse),
            other => panic!("Expected Done with ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_cache_usage() {
        let data = json!({
            "delta": {"stop_reason": "end_turn"},
            "usage": {
                "input_tokens": 200,
                "output_tokens": 100,
                "cache_creation_input_tokens": 50,
                "cache_read_input_tokens": 150
            }
        });
        let events = StreamParser::parse_anthropic("message_delta", &data);
        let usage_event = events.iter().find(|e| matches!(e, StreamEvent::Usage(_)));
        assert!(usage_event.is_some());
        match usage_event.unwrap() {
            StreamEvent::Usage(usage) => {
                assert_eq!(usage.input_tokens, 200);
                assert_eq!(usage.output_tokens, 100);
                assert_eq!(usage.cache_creation_tokens, Some(50));
                assert_eq!(usage.cache_read_tokens, Some(150));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn test_parse_anthropic_thinking_block_start() {
        let data = json!({"content_block": {"type": "thinking"}});
        let events = StreamParser::parse_anthropic("content_block_start", &data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert!(text.is_empty()),
            other => panic!("Expected ThinkingDelta (empty), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_unknown_event() {
        let data = json!({"foo": "bar"});
        let events = StreamParser::parse_anthropic("some_unknown_event", &data);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_anthropic_malformed_data() {
        let data = json!(null);
        let events = StreamParser::parse_anthropic("content_block_delta", &data);
        assert!(events.is_empty());
    }

    // -----------------------------------------------------------------------
    // OpenAI parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_openai_text_delta() {
        let data = json!({
            "choices": [{"delta": {"content": "Hello world"}}]
        });
        let events = StreamParser::parse_openai(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello world"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_openai_empty_content_skipped() {
        let data = json!({
            "choices": [{"delta": {"content": ""}}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_openai_reasoning_content() {
        let data = json!({
            "choices": [{"delta": {"reasoning_content": "Step 1: ..."}}]
        });
        let events = StreamParser::parse_openai(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "Step 1: ..."),
            other => panic!("Expected ThinkingDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_openai_tool_call() {
        let data = json!({
            "choices": [{"delta": {"tool_calls": [
                {"id": "tc1", "function": {"name": "bash", "arguments": ""}}
            ]}}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "bash")));
    }

    #[test]
    fn test_parse_openai_tool_call_with_arguments() {
        let data = json!({
            "choices": [{"delta": {"tool_calls": [
                {"id": "tc1", "function": {"name": "read", "arguments": "{\"path\":"}}
            ]}}]
        });
        let events = StreamParser::parse_openai(&data);
        assert_eq!(events.len(), 2); // ToolUseStart + ToolUseInputDelta
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "read")));
        assert!(events.iter().any(
            |e| matches!(e, StreamEvent::ToolUseInputDelta { partial_json } if partial_json == "{\"path\":")
        ));
    }

    #[test]
    fn test_parse_openai_finish_reason_stop() {
        let data = json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn
            }
        )));
    }

    #[test]
    fn test_parse_openai_finish_reason_tool_calls() {
        let data = json!({
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse
            }
        )));
    }

    #[test]
    fn test_parse_openai_finish_reason_length() {
        let data = json!({
            "choices": [{"delta": {}, "finish_reason": "length"}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::MaxTokens
            }
        )));
    }

    #[test]
    fn test_parse_openai_finish_reason_content_filter() {
        let data = json!({
            "choices": [{"delta": {}, "finish_reason": "content_filter"}]
        });
        let events = StreamParser::parse_openai(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::ContentFilter
            }
        )));
    }

    #[test]
    fn test_parse_openai_usage() {
        let data = json!({
            "choices": [],
            "usage": {"prompt_tokens": 42, "completion_tokens": 17}
        });
        let events = StreamParser::parse_openai(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Usage(usage) => {
                assert_eq!(usage.input_tokens, 42);
                assert_eq!(usage.output_tokens, 17);
                assert!(usage.cache_creation_tokens.is_none());
                assert!(usage.cache_read_tokens.is_none());
            }
            other => panic!("Expected Usage, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_openai_malformed_data() {
        let data = json!({"not_choices": true});
        let events = StreamParser::parse_openai(&data);
        assert!(events.is_empty());
    }

    // -----------------------------------------------------------------------
    // Gemini parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_gemini_text_delta() {
        let data = json!({
            "candidates": [{"content": {"parts": [{"text": "Hello from Gemini"}]}}]
        });
        let events = StreamParser::parse_gemini(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello from Gemini"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_gemini_function_call() {
        let data = json!({
            "candidates": [{"content": {"parts": [
                {"functionCall": {"name": "read_file", "args": {"path": "test.rs"}}}
            ]}}]
        });
        let events = StreamParser::parse_gemini(&data);
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "read_file")));
        assert!(events.iter().any(
            |e| matches!(e, StreamEvent::ToolUseComplete { name, .. } if name == "read_file")
        ));
    }

    #[test]
    fn test_parse_gemini_function_call_no_args() {
        let data = json!({
            "candidates": [{"content": {"parts": [
                {"functionCall": {"name": "list_files"}}
            ]}}]
        });
        let events = StreamParser::parse_gemini(&data);
        // Should still produce ToolUseStart + ToolUseComplete with empty object
        let complete = events.iter().find(
            |e| matches!(e, StreamEvent::ToolUseComplete { name, input, .. } if name == "list_files" && input.is_object()),
        );
        assert!(complete.is_some());
    }

    #[test]
    fn test_parse_gemini_thinking() {
        let data = json!({
            "candidates": [{"content": {"parts": [
                {"thought": "I need to analyze the code..."}
            ]}}]
        });
        let events = StreamParser::parse_gemini(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => {
                assert_eq!(text, "I need to analyze the code...");
            }
            other => panic!("Expected ThinkingDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_gemini_finish_reason_stop() {
        let data = json!({
            "candidates": [{"finishReason": "STOP"}]
        });
        let events = StreamParser::parse_gemini(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn
            }
        )));
    }

    #[test]
    fn test_parse_gemini_finish_reason_safety() {
        let data = json!({
            "candidates": [{"finishReason": "SAFETY"}]
        });
        let events = StreamParser::parse_gemini(&data);
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                stop_reason: StopReason::ContentFilter
            }
        )));
    }

    #[test]
    fn test_parse_gemini_usage_metadata() {
        let data = json!({
            "candidates": [],
            "usageMetadata": {
                "promptTokenCount": 300,
                "candidatesTokenCount": 150,
                "totalTokenCount": 450
            }
        });
        let events = StreamParser::parse_gemini(&data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Usage(usage) => {
                assert_eq!(usage.input_tokens, 300);
                assert_eq!(usage.output_tokens, 150);
            }
            other => panic!("Expected Usage, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_gemini_malformed_data() {
        let data = json!({"not_candidates": true});
        let events = StreamParser::parse_gemini(&data);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_gemini_mixed_parts() {
        let data = json!({
            "candidates": [{"content": {"parts": [
                {"thought": "thinking..."},
                {"text": "Here is the result"},
                {"functionCall": {"name": "bash", "args": {"command": "ls"}}}
            ]}}]
        });
        let events = StreamParser::parse_gemini(&data);
        // Should produce: ThinkingDelta, TextDelta, ToolUseStart, ToolUseComplete
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], StreamEvent::ThinkingDelta { .. }));
        assert!(matches!(&events[1], StreamEvent::TextDelta { .. }));
        assert!(matches!(&events[2], StreamEvent::ToolUseStart { .. }));
        assert!(matches!(&events[3], StreamEvent::ToolUseComplete { .. }));
    }

    // -----------------------------------------------------------------------
    // StopReason / enum tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_stop_reason_variants() {
        assert_eq!(StopReason::EndTurn, StopReason::EndTurn);
        assert_ne!(StopReason::EndTurn, StopReason::ToolUse);
        assert_ne!(StopReason::MaxTokens, StopReason::ContentFilter);
        assert_ne!(StopReason::UserCancelled, StopReason::EndTurn);
        assert_eq!(
            StopReason::Other("custom".into()),
            StopReason::Other("custom".into())
        );
        assert_ne!(StopReason::Other("a".into()), StopReason::Other("b".into()));
    }

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!(usage.cache_creation_tokens.is_none());
        assert!(usage.cache_read_tokens.is_none());
    }

    #[test]
    fn test_stream_event_serialization_roundtrip() {
        let event = StreamEvent::TextDelta {
            text: "hello".into(),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            StreamEvent::TextDelta { text } => assert_eq!(text, "hello"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }
}
