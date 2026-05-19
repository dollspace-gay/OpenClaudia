//! Response parsing utilities for VDD adversary output.
//!
//! Handles extraction of JSON from the adversary text (raw JSON, markdown
//! code blocks, natural-language assessments) and severity parsing.
//!
//! Response-text and token-usage extraction used to live here as free
//! functions but duplicated logic already owned by the
//! [`crate::providers::ProviderAdapter`] trait — see crosslink #479. The
//! free functions are gone; call `adapter.extract_response_text(..)` /
//! `adapter.extract_token_usage(..)` instead.

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

// Response-text and token-usage extraction moved to the
// `ProviderAdapter` trait — see crosslink #479. The free functions that
// used to live here duplicated logic owned by the provider adapters and
// silently returned defaults for any shape they did not hardcode.

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

    // Response-text and token-usage tests moved with the functions —
    // see `src/providers/{mod,anthropic,google,ollama,openai}.rs`
    // (crosslink #479).
}
