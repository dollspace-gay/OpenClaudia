//! End-to-end tests for `ProviderAdapter::extract_response_text`
//! across providers — Anthropic content-array shape, Google
//! candidates[0].content.parts shape, Ollama message.content
//! shape, plus malformed-input None semantics.
//!
//! Sprint 162 of the verification effort. Sprint 17
//! covered the `OpenAI` shape (default trait impl); this
//! file pins each adapter's per-provider response shape.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::get_adapter;
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Anthropic content-array shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_extract_response_text_picks_text_block() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "content": [
            {"type": "text", "text": "the answer"}
        ]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("the answer")
    );
}

#[test]
fn anthropic_extract_response_text_skips_tool_use_blocks() {
    // PINS DOC: only "type":"text" blocks contribute to the
    // text channel; tool_use blocks are extracted elsewhere.
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "content": [
            {"type": "tool_use", "id": "x", "name": "bash", "input": {}},
            {"type": "text", "text": "after tool"}
        ]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("after tool")
    );
}

#[test]
fn anthropic_extract_response_text_returns_first_text_block_only() {
    // .find() picks the FIRST type=text block.
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "content": [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("first"),
        "MUST take FIRST text block (not concat)"
    );
}

#[test]
fn anthropic_extract_response_text_returns_none_on_missing_content() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({"other_field": "value"});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn anthropic_extract_response_text_returns_none_on_empty_content_array() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({"content": []});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn anthropic_extract_response_text_returns_none_on_only_tool_use_blocks() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "content": [
            {"type": "tool_use", "id": "x", "name": "bash", "input": {}}
        ]
    });
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn anthropic_extract_response_text_returns_none_on_content_as_object_not_array() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({"content": {"text": "wrong shape"}});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn anthropic_extract_response_text_preserves_unicode() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "content": [{"type": "text", "text": "日本語 🎉"}]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("日本語 🎉")
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Google candidates shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn google_extract_response_text_picks_candidates_0_content_parts() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {"text": "google answer"}
                ]
            }
        }]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("google answer")
    );
}

#[test]
fn google_extract_response_text_joins_multiple_text_parts() {
    // PINS DOC: parts are JOINED (concat with empty separator).
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {"text": "first "},
                    {"text": "second"}
                ]
            }
        }]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("first second")
    );
}

#[test]
fn google_extract_response_text_filters_non_text_parts() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {"functionCall": {"name": "x"}},
                    {"text": "after function call"}
                ]
            }
        }]
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("after function call")
    );
}

#[test]
fn google_extract_response_text_returns_none_on_no_candidates() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({"other_field": "value"});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn google_extract_response_text_returns_none_on_empty_candidates() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({"candidates": []});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn google_extract_response_text_returns_none_on_no_text_parts_at_all() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "candidates": [{
            "content": {"parts": [{"functionCall": {"name": "x"}}]}
        }]
    });
    // PINS DOC: joined empty string → returned as None.
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn google_extract_response_text_uses_only_first_candidate() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "candidates": [
            {"content": {"parts": [{"text": "first candidate"}]}},
            {"content": {"parts": [{"text": "second candidate"}]}}
        ]
    });
    // Only candidates[0] is consulted.
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("first candidate")
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Ollama message.content shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ollama_extract_response_text_picks_message_content() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({
        "message": {"role": "assistant", "content": "ollama answer"}
    });
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("ollama answer")
    );
}

#[test]
fn ollama_extract_response_text_returns_none_on_no_message() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"other": "x"});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn ollama_extract_response_text_returns_none_on_message_without_content() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"message": {"role": "assistant"}});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn ollama_extract_response_text_returns_none_on_non_string_content() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"message": {"content": 42}});
    assert!(adapter.extract_response_text(&response).is_none());
}

#[test]
fn ollama_extract_response_text_preserves_empty_string_content() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"message": {"content": ""}});
    // PINS: "" is a real string, NOT None.
    assert_eq!(
        adapter.extract_response_text(&response).as_deref(),
        Some("")
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Cross-provider distinctness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn each_provider_returns_none_for_other_providers_response_shape() {
    // PINS SHAPE ISOLATION: an Anthropic response handed to
    // the Google adapter MUST return None (no false match
    // across providers).
    let anthropic_resp = json!({"content": [{"type": "text", "text": "x"}]});
    let google_resp = json!({
        "candidates": [{"content": {"parts": [{"text": "x"}]}}]
    });
    let ollama_resp = json!({"message": {"content": "x"}});

    let google = get_adapter("google").unwrap();
    let ollama = get_adapter("ollama").unwrap();
    let anth = get_adapter("anthropic").unwrap();

    assert!(google.extract_response_text(&anthropic_resp).is_none());
    assert!(google.extract_response_text(&ollama_resp).is_none());

    assert!(ollama.extract_response_text(&anthropic_resp).is_none());
    assert!(ollama.extract_response_text(&google_resp).is_none());

    assert!(anth.extract_response_text(&google_resp).is_none());
    assert!(anth.extract_response_text(&ollama_resp).is_none());
}

#[test]
fn each_provider_returns_none_on_empty_object() {
    let empty = json!({});
    for name in &["anthropic", "openai", "google", "kimi", "minimax", "ollama"] {
        let adapter = get_adapter(name).unwrap();
        assert!(
            adapter.extract_response_text(&empty).is_none(),
            "{name} MUST return None on empty object"
        );
    }
}

#[test]
fn each_provider_returns_none_on_null_value() {
    let nul = serde_json::Value::Null;
    for name in &["anthropic", "openai", "google", "kimi", "minimax", "ollama"] {
        let adapter = get_adapter(name).unwrap();
        assert!(
            adapter.extract_response_text(&nul).is_none(),
            "{name} MUST return None on JSON null"
        );
    }
}

#[test]
fn each_provider_returns_none_on_garbled_array_at_root() {
    let arr = json!(["not", "an", "object"]);
    for name in &["anthropic", "openai", "google", "kimi", "minimax", "ollama"] {
        let adapter = get_adapter(name).unwrap();
        assert!(
            adapter.extract_response_text(&arr).is_none(),
            "{name} MUST return None on root-array garbage"
        );
    }
}
