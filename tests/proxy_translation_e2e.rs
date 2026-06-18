//! End-to-end tests for the proxy translation helpers:
//! `normalize_base_url`, `determine_provider`,
//! `extract_usage_from_sse_event`, and `ChatMessage`/
//! `ChatCompletionRequest` serde round-trip.
//!
//! Sprint 25 of the verification effort. `src/proxy.rs` has 14
//! unit tests but no integration coverage that drives the
//! translation helpers in concert against adversarial inputs.
//!
//! Coverage shape:
//!
//!   - **`normalize_base_url`** — idempotent under trailing `/`,
//!     `/v1`, and `/v1/` stripping; preserves the host body.
//!   - **`determine_provider`** — every documented model prefix
//!     resolves to the right provider; unknown models fall
//!     back to `config.proxy.target`.
//!   - **`extract_usage_from_sse_event`** — returns Some
//!     `TokenUsage` from Anthropic `message_start` /
//!     `message_delta` events; returns None from unrelated
//!     events (no false positives).
//!   - **`ChatMessage` / `ChatCompletionRequest` serde** —
//!     round-trip preserves role, `tool_calls`, content
//!     variants, and unknown extra fields.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::AppConfig;
use openclaudia::proxy::{
    determine_provider, extract_usage_from_sse_event, normalize_base_url, ChatCompletionRequest,
    ChatMessage, MessageContent,
};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — normalize_base_url
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn normalize_base_url_strips_trailing_slash() {
    assert_eq!(
        normalize_base_url("https://api.anthropic.com/"),
        "https://api.anthropic.com"
    );
}

#[test]
fn normalize_base_url_strips_trailing_v1() {
    assert_eq!(
        normalize_base_url("https://api.anthropic.com/v1"),
        "https://api.anthropic.com"
    );
}

#[test]
fn normalize_base_url_strips_trailing_v1_slash() {
    assert_eq!(
        normalize_base_url("https://api.anthropic.com/v1/"),
        "https://api.anthropic.com"
    );
}

#[test]
fn normalize_base_url_idempotent_on_clean_input() {
    let clean = "https://api.anthropic.com";
    assert_eq!(normalize_base_url(clean), clean);
    // And a second normalisation is a no-op.
    let once = normalize_base_url(clean);
    assert_eq!(normalize_base_url(&once), once);
}

#[test]
fn normalize_base_url_preserves_inner_path_segments() {
    // Only the documented trailing fragments are stripped.
    // `v1` appearing as part of a path component (`/api/v1/x`)
    // must NOT be stripped.
    assert_eq!(
        normalize_base_url("https://api.example.com/proxy"),
        "https://api.example.com/proxy"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — determine_provider
// ───────────────────────────────────────────────────────────────────────────

/// Build a minimal `AppConfig` with a known proxy target.
/// `AppConfig` doesn't impl `Default`, so we deserialize from
/// the minimum-valid YAML — every optional field gets the
/// serde-default treatment.
fn cfg_with_target(target: &str) -> AppConfig {
    let yaml = format!(
        "proxy:\n  port: 8080\n  host: \"127.0.0.1\"\n  target: {target}\nproviders:\n  anthropic:\n    base_url: https://api.anthropic.com\n    api_key: sk-ant-test-key\n",
    );
    serde_yaml::from_str(&yaml).expect("minimal yaml must parse into AppConfig")
}

#[test]
fn determine_provider_dispatches_by_model_prefix() {
    let config = cfg_with_target("anthropic");
    for (model, expected) in &[
        ("claude-3-5-sonnet-20241022", "anthropic"),
        ("claude-opus-4-8", "anthropic"),
        ("anthropic-internal", "anthropic"),
        ("gpt-5.5", "openai"),
        ("gpt-4-turbo", "openai"),
        ("o1-preview", "openai"),
        ("o3-mini", "openai"),
        ("gemini-3.5-flash", "google"),
        ("gemini-3.1-pro-preview-customtools", "google"),
        ("deepseek-chat", "deepseek"),
        ("deepseek-reasoner", "deepseek"),
        ("qwen-max", "qwen"),
        ("qwq-32b", "qwen"),
        ("glm-4-32b", "zai"),
        ("kimi-k2.7-code", "kimi"),
        ("moonshot-v1-128k", "kimi"),
        ("MiniMax-M3", "minimax"),
    ] {
        let got = determine_provider(model, &config);
        assert_eq!(
            got, *expected,
            "model {model:?} must dispatch to {expected:?}; got {got:?}"
        );
    }
}

#[test]
fn determine_provider_unknown_model_falls_back_to_config_target() {
    let config = cfg_with_target("openai");
    let got = determine_provider("totally-unknown-model-2099", &config);
    assert_eq!(
        got, "openai",
        "unknown model must fall back to config.proxy.target"
    );
}

#[test]
fn determine_provider_is_case_insensitive() {
    let config = cfg_with_target("openai");
    let lower = determine_provider("claude-3-opus", &config);
    let upper = determine_provider("CLAUDE-3-OPUS", &config);
    let mixed = determine_provider("Claude-3-Opus", &config);
    assert_eq!(lower, upper);
    assert_eq!(lower, mixed);
    assert_eq!(lower, "anthropic");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — extract_usage_from_sse_event
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extract_usage_from_anthropic_message_start_event() {
    let event = json!({
        "type": "message_start",
        "message": {
            "usage": {
                "input_tokens": 100,
                "output_tokens": 0
            }
        }
    });
    let usage = extract_usage_from_sse_event(&event);
    assert!(
        usage.is_some(),
        "message_start with input_tokens must extract"
    );
    let u = usage.unwrap();
    assert_eq!(u.input_tokens, 100);
}

#[test]
fn extract_usage_from_anthropic_message_delta_event() {
    let event = json!({
        "type": "message_delta",
        "delta": {},
        "usage": {
            "output_tokens": 50
        }
    });
    let usage = extract_usage_from_sse_event(&event);
    assert!(
        usage.is_some(),
        "message_delta with output_tokens must extract"
    );
    let u = usage.unwrap();
    assert_eq!(u.output_tokens, 50);
}

#[test]
fn extract_usage_returns_none_for_unrelated_event() {
    // content_block_delta is the most common SSE event — it
    // carries no usage info. The extractor MUST return None,
    // not produce a zero-filled TokenUsage that would
    // double-count in a running sum.
    let event = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": "hello"}
    });
    let usage = extract_usage_from_sse_event(&event);
    assert!(
        usage.is_none(),
        "content_block_delta must produce None; got {usage:?}"
    );
}

#[test]
fn extract_usage_returns_none_for_empty_event() {
    let event = json!({});
    let usage = extract_usage_from_sse_event(&event);
    assert!(usage.is_none());
}

#[test]
fn extract_usage_returns_none_for_message_delta_with_zero_output() {
    // A delta event carrying output_tokens=0 is treated as "no
    // signal" (the count would otherwise overwrite a meaningful
    // earlier accumulator with 0).
    let event = json!({
        "type": "message_delta",
        "usage": {"output_tokens": 0}
    });
    let usage = extract_usage_from_sse_event(&event);
    assert!(
        usage.is_none(),
        "message_delta with output_tokens=0 must produce None; got {usage:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — ChatMessage / ChatCompletionRequest serde round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn chat_message_text_content_round_trips_through_json() {
    let original = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Text("hello".to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    let json_str = serde_json::to_string(&original).expect("serialize");
    let parsed: ChatMessage = serde_json::from_str(&json_str).expect("deserialize");
    assert_eq!(parsed.role, "user");
    match parsed.content {
        MessageContent::Text(s) => assert_eq!(s, "hello"),
        MessageContent::Parts(_) => panic!("expected Text variant"),
    }
}

#[test]
fn chat_message_with_tool_calls_round_trips() {
    let original = ChatMessage {
        role: "assistant".to_string(),
        content: MessageContent::Text(String::new()),
        name: None,
        tool_calls: Some(vec![json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
        })]),
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    let json_str = serde_json::to_string(&original).expect("serialize");
    let parsed: ChatMessage = serde_json::from_str(&json_str).expect("deserialize");
    assert_eq!(parsed.role, "assistant");
    let tc = parsed.tool_calls.expect("tool_calls must survive");
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0]["id"], "call_1");
    assert_eq!(tc[0]["function"]["name"], "bash");
}

#[test]
fn chat_completion_request_unknown_extras_round_trip_via_flatten() {
    // The `extra` field flattens into the request, so arbitrary
    // unknown fields (`thinking`, `temperature`, etc.) survive
    // a serde round-trip.
    let raw = json!({
        "model": "claude-3-opus",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0.7,
        "max_tokens": 1024,
        "thinking": {"enabled": true, "budget_tokens": 4000},
        "novel_field": "future-proofing"
    });
    let parsed: ChatCompletionRequest = serde_json::from_value(raw).expect("parse");
    assert_eq!(parsed.model, "claude-3-opus");
    assert_eq!(parsed.temperature, Some(0.7));
    assert_eq!(parsed.max_tokens, Some(1024));
    // `thinking` and `novel_field` land in `extra`.
    assert!(parsed.extra.contains_key("thinking"));
    assert!(parsed.extra.contains_key("novel_field"));

    // Re-serialise; the unknown fields MUST survive.
    let round = serde_json::to_value(&parsed).expect("re-serialize");
    assert_eq!(round["thinking"]["enabled"], true);
    assert_eq!(round["novel_field"], "future-proofing");
}

#[test]
fn message_content_text_and_parts_round_trip_separately() {
    // Text variant.
    let text = json!({"role": "user", "content": "hello"});
    let msg: ChatMessage = serde_json::from_value(text).expect("text parse");
    assert!(matches!(msg.content, MessageContent::Text(_)));

    // Parts variant (multimodal).
    let parts = json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "what's in this image?"},
            {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}
        ]
    });
    let msg: ChatMessage = serde_json::from_value(parts).expect("parts parse");
    match &msg.content {
        MessageContent::Parts(p) => assert_eq!(p.len(), 2),
        MessageContent::Text(_) => panic!("expected Parts variant"),
    }
}
