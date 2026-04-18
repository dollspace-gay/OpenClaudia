//! Response parsing utilities for VDD adversary output.
//!
//! Handles extraction of JSON from various response formats (raw JSON,
//! markdown code blocks, natural language), severity parsing, and
//! token usage extraction from provider responses.

use serde_json::Value;
use tracing::debug;

use crate::session::TokenUsage;

use super::review::AdversaryResponse;

// ==========================================================================
// JSON Extraction
// ==========================================================================

/// Try to extract JSON from a response that may contain markdown code blocks.
///
/// Every slice into `text` goes through [`str::get`] so an offset that
/// somehow lands mid-codepoint returns `None` instead of panicking. The
/// previous implementation used direct `text[a..b]` indexing; today's
/// delimiters are all ASCII so the arithmetic stays on char boundaries,
/// but a single future non-ASCII fence token would turn adversary output
/// into a VDD-loop kill via a single multibyte codepoint.
/// See crosslink #337.
pub(crate) fn extract_json_from_response(text: &str) -> Option<String> {
    // Look for ```json ... ``` blocks
    if let Some(start) = text.find("```json") {
        let json_start = start + "```json".len();
        if let Some(rest) = text.get(json_start..) {
            if let Some(end) = rest.find("```") {
                if let Some(inner) = rest.get(..end) {
                    return Some(inner.trim().to_string());
                }
            }
        }
    }

    // Look for ``` ... ``` blocks
    if let Some(start) = text.find("```") {
        let json_start = start + 3;
        if let Some(after_fence) = text.get(json_start..) {
            // Skip optional language identifier on the same line
            let line_end = after_fence.find('\n').unwrap_or(0);
            if let Some(after_lang) = after_fence.get(line_end..) {
                if let Some(end) = after_lang.find("```") {
                    if let Some(inner) = after_lang.get(..end) {
                        return Some(inner.trim().to_string());
                    }
                }
            }
        }
    }

    // Try to find raw JSON object starting with `{"findings"`
    if let Some(start) = text.find(r#"{"findings""#) {
        let tail = text.get(start..)?;
        let mut depth = 0i32;
        let mut end_rel: Option<usize> = None;
        for (i, c) in tail.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_rel = Some(i + c.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(len) = end_rel {
            if let Some(inner) = tail.get(..len) {
                return Some(inner.to_string());
            }
        }
    }

    // Try to find any raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                // `end + 1` is guaranteed to be a char boundary because `}` is ASCII.
                if let Some(inner) = text.get(start..=end) {
                    return Some(inner.to_string());
                }
            }
        }
    }

    None
}

/// Try to construct a valid `AdversaryResponse` from partial/malformed JSON
pub(crate) fn try_parse_relaxed(text: &str) -> Option<AdversaryResponse> {
    // Check for "NO_FINDINGS" or "no findings" anywhere in response
    let lower = text.to_lowercase();
    if lower.contains("no_findings")
        || lower.contains("no findings")
        || lower.contains("no issues")
        || lower.contains("no vulnerabilities")
        || lower.contains("code looks correct")
        || lower.contains("looks good")
    {
        return Some(AdversaryResponse {
            findings: Some(vec![]),
            assessment: Some("NO_FINDINGS".to_string()),
        });
    }

    None
}

// ==========================================================================
// Severity Parsing
// ==========================================================================

/// Parse a severity string into the Severity enum.
pub(crate) fn parse_severity(s: &str) -> super::finding::Severity {
    use super::finding::Severity;
    match s.to_uppercase().as_str() {
        "CRITICAL" => Severity::Critical,
        "HIGH" => Severity::High,
        "MEDIUM" | "MED" => Severity::Medium,
        "LOW" => Severity::Low,
        _ => Severity::Info,
    }
}

// ==========================================================================
// Response Text Extraction
// ==========================================================================

/// Extract the text content from a chat completion response.
/// Supports `OpenAI`, Anthropic, and Google/Gemini formats.
pub(crate) fn extract_response_text(response: &Value) -> String {
    // OpenAI format: choices[0].message.content
    if let Some(content) = response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return content.to_string();
    }

    // Anthropic format: content[0].text
    if let Some(content) = response
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
        })
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
    {
        return content.to_string();
    }

    // Google/Gemini format: candidates[0].content.parts[0].text
    if let Some(content) = response
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
    {
        return content.to_string();
    }

    // Log what we actually received for debugging
    debug!(
        "VDD: Unknown response format, dumping structure: {:?}",
        response.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );

    String::new()
}

// ==========================================================================
// Token Usage Extraction
// ==========================================================================

/// Extract token usage from a provider response.
pub(crate) fn extract_token_usage(response: &Value) -> TokenUsage {
    // OpenAI/Anthropic format: usage.prompt_tokens / usage.completion_tokens
    if let Some(usage) = response.get("usage") {
        return TokenUsage {
            input_tokens: usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            cache_write_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        };
    }

    // Google/Gemini format: usageMetadata.promptTokenCount / candidatesTokenCount
    if let Some(usage) = response.get("usageMetadata") {
        return TokenUsage {
            input_tokens: usage
                .get("promptTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("candidatesTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cachedContentTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            cache_write_tokens: 0,
        };
    }

    TokenUsage::default()
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // --- Regression tests for crosslink #337 (UTF-8 safety) ---

    #[test]
    fn extract_json_survives_leading_emoji() {
        // 4-byte UTF-8 codepoint immediately before the fence (🔥 = U+1F525).
        let text = "🔥```json\n{\"findings\": []}\n```";
        let json = extract_json_from_response(text).expect("parser should not panic");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["findings"].is_array());
    }

    #[test]
    fn extract_json_survives_cjk_prose() {
        let text = "分析结果如下:\n```json\n{\"assessment\": \"NO_FINDINGS\"}\n```\n";
        let json = extract_json_from_response(text).expect("parser should not panic");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["assessment"], "NO_FINDINGS");
    }

    #[test]
    fn extract_json_survives_smart_quotes_in_prose() {
        let text = "\u{201C}Note:\u{201D} nothing to report.\n```json\n{\"findings\": []}\n```";
        let json = extract_json_from_response(text).expect("parser should not panic");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["findings"].is_array());
    }

    #[test]
    fn extract_json_survives_emoji_inside_json_string() {
        let text = r#"```json
{"findings": [{"desc": "contains 🚀 and 💥"}]}
```"#;
        let json = extract_json_from_response(text).expect("parser should not panic");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["findings"][0]["desc"], "contains 🚀 and 💥");
    }

    #[test]
    fn extract_json_from_raw_findings_object_with_emoji() {
        let text = r#"preamble 🎯 {"findings": [{"desc": "hello"}]} trailing"#;
        let json = extract_json_from_response(text).expect("parser should not panic");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["findings"][0]["desc"], "hello");
    }

    #[test]
    fn extract_json_returns_none_for_empty_input() {
        assert!(extract_json_from_response("").is_none());
        assert!(extract_json_from_response("no braces here").is_none());
    }

    #[test]
    fn extract_json_survives_unclosed_fence() {
        // Adversarial malformed output: opening fence but no closing fence.
        // Must not panic.
        let text = "```json\n{\"findings\": []"; // missing }
        let _ = extract_json_from_response(text);
    }

    #[test]
    fn test_parse_severity() {
        use super::super::finding::Severity;
        assert_eq!(parse_severity("CRITICAL"), Severity::Critical);
        assert_eq!(parse_severity("critical"), Severity::Critical);
        assert_eq!(parse_severity("HIGH"), Severity::High);
        assert_eq!(parse_severity("MEDIUM"), Severity::Medium);
        assert_eq!(parse_severity("MED"), Severity::Medium);
        assert_eq!(parse_severity("LOW"), Severity::Low);
        assert_eq!(parse_severity("INFO"), Severity::Info);
        assert_eq!(parse_severity("unknown"), Severity::Info);
    }

    #[test]
    fn test_extract_json_from_code_block() {
        let text = r#"Here is my analysis:
```json
{"findings": [], "assessment": "NO_FINDINGS"}
```
"#;
        let json = extract_json_from_response(text).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["assessment"], "NO_FINDINGS");
    }

    #[test]
    fn test_extract_json_from_raw() {
        let text = r#"Some preamble text {"findings": [{"severity": "HIGH"}], "assessment": "FINDINGS_PRESENT"} trailing text"#;
        let json = extract_json_from_response(text).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["assessment"], "FINDINGS_PRESENT");
    }

    #[test]
    fn test_extract_response_text_openai_format() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Hello from the model"
                }
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from the model");
    }

    #[test]
    fn test_extract_response_text_anthropic_format() {
        let response = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "Hello from Anthropic"
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from Anthropic");
    }

    #[test]
    fn test_extract_response_text_empty() {
        let response = serde_json::json!({});
        assert_eq!(extract_response_text(&response), "");
    }

    #[test]
    fn test_extract_response_text_google_format() {
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "Hello from Gemini"
                    }]
                }
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from Gemini");
    }

    #[test]
    fn test_extract_token_usage_google_format() {
        let response = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 150,
                "candidatesTokenCount": 80,
                "cachedContentTokenCount": 25
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 25);
    }

    #[test]
    fn test_extract_token_usage_openai() {
        let response = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn test_extract_token_usage_anthropic() {
        let response = serde_json::json!({
            "usage": {
                "input_tokens": 200,
                "output_tokens": 75,
                "cache_read_input_tokens": 50,
                "cache_creation_input_tokens": 10
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.cache_read_tokens, 50);
        assert_eq!(usage.cache_write_tokens, 10);
    }
}
