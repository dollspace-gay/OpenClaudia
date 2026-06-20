//! End-to-end tests for `tools::AnthropicContentBlock` enum
//! shape + `AnthropicToolAccumulator::to_openai_tool_calls_json`
//! cross-format converter + `clear()` reset semantics +
//! `has_tool_use` edge cases.
//!
//! Sprint 95 of the verification effort. Sprint 45 covered
//! the streaming event-processing happy paths through
//! `process_event`; this file pins the cross-format
//! conversion (`to_openai_tool_calls_json` — what proxy
//! writes into the chat session for round-trip via
//! `convert_messages_to_anthropic`) plus the documented
//! reset / mixed-state semantics.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{AnthropicContentBlock, AnthropicToolAccumulator};
use serde_json::{json, Value};

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Drive an event stream through the accumulator.
fn drive(events: &[Value]) -> AnthropicToolAccumulator {
    let mut acc = AnthropicToolAccumulator::new();
    for event in events {
        let _ = acc.process_event(event);
    }
    acc
}

fn tool_use_stream(id: &str, name: &str, json_chunks: &[&str]) -> Vec<Value> {
    let mut events = vec![json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "tool_use",
            "id": id,
            "name": name
        }
    })];
    for chunk in json_chunks {
        events.push(json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "input_json_delta",
                "partial_json": chunk
            }
        }));
    }
    events.push(json!({
        "type": "content_block_stop",
        "index": 0
    }));
    events.push(json!({
        "type": "message_delta",
        "delta": {"stop_reason": "tool_use"}
    }));
    events
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — AnthropicContentBlock variant shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn content_block_text_variant_carries_string() {
    let block = AnthropicContentBlock::Text("hello".to_string());
    let AnthropicContentBlock::Text(s) = block else {
        panic!("expected Text variant");
    };
    assert_eq!(s, "hello");
}

#[test]
fn content_block_tool_use_variant_carries_id_name_input() {
    let block = AnthropicContentBlock::ToolUse {
        id: "id-1".to_string(),
        name: "bash".to_string(),
        input_json: "{\"command\":\"ls\"}".to_string(),
    };
    if let AnthropicContentBlock::ToolUse {
        id,
        name,
        input_json,
    } = block
    {
        assert_eq!(id, "id-1");
        assert_eq!(name, "bash");
        assert_eq!(input_json, "{\"command\":\"ls\"}");
    } else {
        panic!("expected ToolUse variant");
    }
}

#[test]
fn content_block_clone_preserves_variant_data() {
    let original = AnthropicContentBlock::ToolUse {
        id: "x".to_string(),
        name: "y".to_string(),
        input_json: "{}".to_string(),
    };
    let cloned = original.clone();
    if let (
        AnthropicContentBlock::ToolUse {
            id: a_id,
            name: a_name,
            input_json: a_json,
        },
        AnthropicContentBlock::ToolUse {
            id: b_id,
            name: b_name,
            input_json: b_json,
        },
    ) = (&original, &cloned)
    {
        assert_eq!(a_id, b_id);
        assert_eq!(a_name, b_name);
        assert_eq!(a_json, b_json);
    } else {
        panic!("both must be ToolUse");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — to_openai_tool_calls_json conversion
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn to_openai_tool_calls_json_empty_when_no_tool_use_blocks() {
    let acc = AnthropicToolAccumulator::new();
    let calls = acc.to_openai_tool_calls_json();
    assert!(calls.is_empty());
}

#[test]
fn to_openai_tool_calls_json_skips_text_blocks() {
    let mut acc = AnthropicToolAccumulator::new();
    let _ = acc.process_event(&json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "text"}
    }));
    let _ = acc.process_event(&json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": "hello"}
    }));
    let calls = acc.to_openai_tool_calls_json();
    assert!(
        calls.is_empty(),
        "text blocks MUST NOT produce tool_calls; got {calls:?}"
    );
}

#[test]
fn to_openai_tool_calls_json_emits_function_shape_for_tool_use() {
    let events = tool_use_stream("tool-id-1", "bash", &["{\"comm", "and\":\"l", "s\"}"]);
    let acc = drive(&events);
    let calls = acc.to_openai_tool_calls_json();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    // Documented OpenAI shape:
    //   {id, type: "function", function: {name, arguments}}
    assert_eq!(call["id"], "tool-id-1");
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "bash");
    assert_eq!(call["function"]["arguments"], "{\"command\":\"ls\"}");
}

#[test]
fn to_openai_tool_calls_json_arguments_field_is_string_not_object() {
    // PINS WIRE CONTRACT: arguments is a STRING (raw JSON
    // bytes), not an object. This matches OpenAI's API which
    // expects arguments as a serialized string.
    let events = tool_use_stream("id", "fn", &["{\"k\":1}"]);
    let acc = drive(&events);
    let calls = acc.to_openai_tool_calls_json();
    assert!(
        calls[0]["function"]["arguments"].is_string(),
        "arguments MUST be string per OpenAI schema; got {:?}",
        calls[0]["function"]["arguments"]
    );
}

#[test]
fn to_openai_tool_calls_json_preserves_call_order() {
    // Two tool_use blocks back-to-back.
    let mut events = tool_use_stream("first", "a", &["{}"]);
    // Bump to index 1 for the second tool_use.
    events.extend(vec![
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "second", "name": "b"}
        }),
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{}"}
        }),
        json!({"type": "content_block_stop", "index": 1}),
    ]);
    let acc = drive(&events);
    let calls = acc.to_openai_tool_calls_json();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "first");
    assert_eq!(calls[1]["id"], "second");
}

#[test]
fn to_openai_tool_calls_json_handles_empty_input_json() {
    // A tool_use block that received no input deltas (no
    // arguments). The history format must still carry a valid JSON object.
    let events = vec![
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "x", "name": "noop"}
        }),
        json!({"type": "content_block_stop", "index": 0}),
    ];
    let acc = drive(&events);
    let calls = acc.to_openai_tool_calls_json();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["function"]["arguments"], "{}");
}

#[test]
fn to_openai_tool_calls_json_filters_mixed_blocks_to_tool_uses_only() {
    // text → tool_use → text mix; converter should yield 1 call.
    let mut acc = AnthropicToolAccumulator::new();
    // text block
    let _ = acc.process_event(&json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "text"}
    }));
    let _ = acc.process_event(&json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": "I will use a tool: "}
    }));
    let _ = acc.process_event(&json!({"type": "content_block_stop", "index": 0}));
    // tool_use block
    let _ = acc.process_event(&json!({
        "type": "content_block_start",
        "index": 1,
        "content_block": {"type": "tool_use", "id": "the-tool", "name": "bash"}
    }));
    let _ = acc.process_event(&json!({
        "type": "content_block_delta",
        "delta": {"type": "input_json_delta", "partial_json": "{}"}
    }));
    let _ = acc.process_event(&json!({"type": "content_block_stop", "index": 1}));
    // another text block
    let _ = acc.process_event(&json!({
        "type": "content_block_start",
        "index": 2,
        "content_block": {"type": "text"}
    }));
    let _ = acc.process_event(&json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": " and that's done"}
    }));
    let calls = acc.to_openai_tool_calls_json();
    assert_eq!(
        calls.len(),
        1,
        "MUST emit only the tool_use; got {} calls",
        calls.len()
    );
    assert_eq!(calls[0]["id"], "the-tool");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — has_tool_use edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_tool_use_false_when_stop_reason_is_end_turn() {
    // Text-only stream with stop_reason="end_turn" MUST NOT
    // flag has_tool_use.
    let events = vec![
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text"}
        }),
        json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "hi"}
        }),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"}
        }),
    ];
    let acc = drive(&events);
    assert!(!acc.has_tool_use());
}

#[test]
fn has_tool_use_false_with_no_stop_reason_received() {
    let mut acc = AnthropicToolAccumulator::new();
    // Add a tool_use block but never observe message_delta.
    let _ = acc.process_event(&json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "tool_use", "id": "x", "name": "y"}
    }));
    assert!(
        !acc.has_tool_use(),
        "MUST require BOTH stop_reason=tool_use AND a ToolUse block"
    );
}

#[test]
fn has_tool_use_true_only_when_both_stop_reason_and_block_present() {
    let events = tool_use_stream("id", "fn", &["{}"]);
    let acc = drive(&events);
    assert!(
        acc.has_tool_use(),
        "stop_reason=tool_use + ToolUse block MUST flag has_tool_use"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — clear() reset semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clear_empties_blocks_and_resets_stop_reason() {
    let events = tool_use_stream("id", "fn", &["{}"]);
    let mut acc = drive(&events);
    assert!(!acc.blocks.is_empty());
    assert!(acc.stop_reason.is_some());
    acc.clear();
    assert!(acc.blocks.is_empty());
    assert!(acc.stop_reason.is_none());
}

#[test]
fn clear_returns_accumulator_to_new_equivalent_state() {
    let events = tool_use_stream("id", "fn", &["{}"]);
    let mut acc = drive(&events);
    acc.clear();
    let fresh = AnthropicToolAccumulator::new();
    assert_eq!(acc.blocks.len(), fresh.blocks.len());
    assert_eq!(acc.stop_reason, fresh.stop_reason);
    assert!(!acc.has_tool_use());
    assert_eq!(acc.get_text(), "");
}

#[test]
fn clear_can_be_called_multiple_times_idempotently() {
    let mut acc = AnthropicToolAccumulator::new();
    acc.clear();
    acc.clear();
    acc.clear();
    assert!(acc.blocks.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Default + new equivalence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn default_constructor_equals_new() {
    let d = AnthropicToolAccumulator::default();
    let n = AnthropicToolAccumulator::new();
    assert_eq!(d.blocks.len(), n.blocks.len());
    assert_eq!(d.stop_reason, n.stop_reason);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — get_text aggregation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_text_returns_empty_when_no_text_blocks() {
    let events = tool_use_stream("id", "fn", &["{}"]);
    let acc = drive(&events);
    assert_eq!(acc.get_text(), "");
}

#[test]
fn get_text_concatenates_multiple_text_blocks() {
    let events = vec![
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text"}
        }),
        json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "first "}
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "text"}
        }),
        json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "second"}
        }),
    ];
    let acc = drive(&events);
    let text = acc.get_text();
    assert!(text.contains("first"));
    assert!(text.contains("second"));
}
