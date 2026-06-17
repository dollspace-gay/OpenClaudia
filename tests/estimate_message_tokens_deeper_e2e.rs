//! End-to-end tests for `compaction::estimate_message_tokens`
//! plus `compaction::estimate_request_tokens` —
//! per-message overhead (4 + name), `image_url` 1600-token
//! contribution, `tool_calls` accounting, and the request-level
//! 100-token structural overhead + tool-definition addend.
//!
//! Sprint 189 of the verification effort. Sprint 92 had 4
//! basic tests; this file pins the per-field token-cost
//! contributions explicitly.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::compaction::{estimate_message_tokens, estimate_request_tokens, estimate_tokens};
use openclaudia::proxy::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent};
use serde_json::json;
use std::collections::HashMap;

fn user_text(content: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Text(content.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — estimate_message_tokens per-field overhead
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn empty_user_message_costs_at_least_overhead_4_tokens() {
    // PINS DOC: overhead = 4 tokens minimum (role, structure).
    let msg = user_text("");
    let tokens = estimate_message_tokens(&msg);
    assert_eq!(tokens, 4, "empty user message MUST cost exactly 4 tokens");
}

#[test]
fn message_with_4_ascii_chars_adds_1_token_to_overhead() {
    // 4 ASCII chars = 1 token; +4 overhead = 5 total.
    let msg = user_text("abcd");
    assert_eq!(estimate_message_tokens(&msg), 5);
}

#[test]
fn message_with_name_adds_name_estimate_to_overhead() {
    let mut msg = user_text("");
    msg.name = Some("abcd".to_string());
    // PINS DOC: name contributes via estimate_tokens.
    // name "abcd" = 1 token + 4 overhead = 5 total.
    assert_eq!(estimate_message_tokens(&msg), 5);
}

#[test]
fn message_with_long_name_adds_proportional_tokens() {
    let mut msg = user_text("");
    msg.name = Some("a".repeat(100));
    // 100 chars / 4 = 25 tokens + 4 overhead = 29.
    assert_eq!(estimate_message_tokens(&msg), 29);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Parts content with image_url
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parts_with_only_image_url_adds_1600_image_cost() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(vec![ContentPart {
            content_type: "image_url".to_string(),
            text: None,
            image_url: Some(json!({"url": "data:image/png;base64,..."})),
        }]),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    // Image 1600 + overhead 4 = 1604.
    assert_eq!(estimate_message_tokens(&msg), 1604);
}

#[test]
fn parts_with_both_text_and_image_url_sums_them() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(vec![ContentPart {
            content_type: "text".to_string(),
            text: Some("abcd".to_string()),
            image_url: Some(json!({"url": "x"})),
        }]),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    // text "abcd" = 1 + image 1600 + overhead 4 = 1605.
    assert_eq!(estimate_message_tokens(&msg), 1605);
}

#[test]
fn parts_with_2_images_charges_3200_image_tokens() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(vec![
            ContentPart {
                content_type: "image_url".to_string(),
                text: None,
                image_url: Some(json!({"url": "a"})),
            },
            ContentPart {
                content_type: "image_url".to_string(),
                text: None,
                image_url: Some(json!({"url": "b"})),
            },
        ]),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    // 2 * 1600 + 4 overhead = 3204.
    assert_eq!(estimate_message_tokens(&msg), 3204);
}

#[test]
fn parts_with_neither_text_nor_image_url_contributes_zero() {
    // PINS DOC: a Part with both fields None contributes 0
    // (still costs 4 overhead).
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(vec![ContentPart {
            content_type: "video".to_string(),
            text: None,
            image_url: None,
        }]),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    assert_eq!(estimate_message_tokens(&msg), 4);
}

#[test]
fn empty_parts_array_costs_only_overhead() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(Vec::new()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    assert_eq!(estimate_message_tokens(&msg), 4);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — tool_calls contribution
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn message_with_tool_calls_adds_per_call_string_estimate() {
    let mut msg = user_text("");
    let call =
        json!({"id": "c1", "type": "function", "function": {"name": "x", "arguments": "{}"}});
    msg.tool_calls = Some(vec![call.clone()]);
    let call_str_tokens = estimate_tokens(&call.to_string());
    // Total = overhead 4 + call_str_tokens.
    assert_eq!(estimate_message_tokens(&msg), 4 + call_str_tokens);
}

#[test]
fn message_with_multiple_tool_calls_sums_each() {
    let mut msg = user_text("");
    let call1 = json!({"id": "c1", "type": "function", "function": {"name": "a"}});
    let call2 = json!({"id": "c2", "type": "function", "function": {"name": "b"}});
    msg.tool_calls = Some(vec![call1.clone(), call2.clone()]);
    let expected = 4 + estimate_tokens(&call1.to_string()) + estimate_tokens(&call2.to_string());
    assert_eq!(estimate_message_tokens(&msg), expected);
}

#[test]
fn message_with_empty_tool_calls_vec_costs_only_overhead() {
    let mut msg = user_text("");
    msg.tool_calls = Some(Vec::new());
    assert_eq!(estimate_message_tokens(&msg), 4);
}

#[test]
fn message_with_no_tool_calls_field_costs_only_overhead_and_content() {
    let msg = user_text("abcd");
    assert_eq!(estimate_message_tokens(&msg), 5);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — estimate_request_tokens
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn empty_request_costs_at_least_100_overhead() {
    // PINS DOC: request adds 100-token structural overhead.
    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: Vec::new(),
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    };
    assert_eq!(
        estimate_request_tokens(&req),
        100,
        "empty request MUST cost exactly 100 tokens (struct overhead only)"
    );
}

#[test]
fn request_token_count_sums_per_message_estimates_plus_overhead() {
    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: vec![user_text(""), user_text("")],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    };
    // Each empty user msg = 4 tokens; 2 + 100 overhead = 108.
    assert_eq!(estimate_request_tokens(&req), 4 + 4 + 100);
}

#[test]
fn request_with_tools_adds_per_tool_string_estimate() {
    let tool = json!({"type": "function", "function": {"name": "x"}});
    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: Vec::new(),
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: Some(vec![tool.clone()]),
        tool_choice: None,
        extra: HashMap::new(),
    };
    let expected = 100 + estimate_tokens(&tool.to_string());
    assert_eq!(estimate_request_tokens(&req), expected);
}

#[test]
fn request_with_multiple_tools_sums_each() {
    let t1 = json!({"type": "function", "function": {"name": "a"}});
    let t2 = json!({"type": "function", "function": {"name": "b"}});
    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: Vec::new(),
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: Some(vec![t1.clone(), t2.clone()]),
        tool_choice: None,
        extra: HashMap::new(),
    };
    let expected = 100 + estimate_tokens(&t1.to_string()) + estimate_tokens(&t2.to_string());
    assert_eq!(estimate_request_tokens(&req), expected);
}

#[test]
fn request_with_empty_tools_vec_costs_only_messages_and_overhead() {
    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: vec![user_text("")],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: Some(Vec::new()),
        tool_choice: None,
        extra: HashMap::new(),
    };
    assert_eq!(estimate_request_tokens(&req), 4 + 100);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Monotonicity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn adding_a_message_strictly_increases_estimate() {
    let mut req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: vec![user_text("")],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    };
    let before = estimate_request_tokens(&req);
    req.messages.push(user_text(""));
    let after = estimate_request_tokens(&req);
    assert!(after > before);
}

#[test]
fn longer_content_strictly_increases_message_estimate() {
    let short = user_text("a");
    let long = user_text(&"a".repeat(1000));
    assert!(estimate_message_tokens(&long) > estimate_message_tokens(&short));
}

#[test]
fn adding_image_url_increases_message_estimate_by_at_least_1600() {
    let bare = user_text("hello");
    let with_image = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(vec![
            ContentPart {
                content_type: "text".to_string(),
                text: Some("hello".to_string()),
                image_url: None,
            },
            ContentPart {
                content_type: "image_url".to_string(),
                text: None,
                image_url: Some(json!({"url": "x"})),
            },
        ]),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    let delta = estimate_message_tokens(&with_image) - estimate_message_tokens(&bare);
    assert!(
        delta >= 1600,
        "PINS: image adds >= 1600 tokens; got delta {delta}"
    );
}
