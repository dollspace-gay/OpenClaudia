//! End-to-end tests for `tools::ToolCallAccumulator::finalize` —
//! the filter contract that drops `PartialToolCall` entries
//! with empty `id` OR empty `function_name`, plus the
//! `call_type` default (`"function"`) and field propagation.
//!
//! Sprint 193 of the verification effort. Sprint 93 had
//! the order-preservation + happy-path tests; this file
//! pins the filter rule + `call_type` default + `clear()`
//! lifecycle + `has_tool_calls` + finalize round-trip with
//! mixed valid/invalid partials.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{PartialToolCall, ToolCallAccumulator};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — finalize filter: empty id drops the entry
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finalize_drops_partial_with_empty_id() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    // No id supplied → empty id → filtered out.
    let calls = acc.finalize();
    assert!(
        calls.is_empty(),
        "PINS FILTER: empty id MUST be dropped; got {} calls",
        calls.len()
    );
}

#[test]
fn finalize_drops_partial_with_empty_function_name() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function"
        }]
    }));
    // No function.name → empty → filtered out.
    let calls = acc.finalize();
    assert!(
        calls.is_empty(),
        "PINS FILTER: empty function_name MUST be dropped"
    );
}

#[test]
fn finalize_drops_partial_with_both_id_and_name_empty() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0
        }]
    }));
    assert!(acc.finalize().is_empty());
}

#[test]
fn finalize_keeps_partial_with_both_id_and_function_name() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "call_marker_193",
            "type": "function",
            "function": {"name": "tool_marker_193", "arguments": "{}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_marker_193");
    assert_eq!(calls[0].function.name, "tool_marker_193");
}

#[test]
fn finalize_keeps_only_valid_entries_in_mixed_batch() {
    let mut acc = ToolCallAccumulator::new();
    // Slot 0: valid.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "valid_1",
            "function": {"name": "good_tool", "arguments": "{}"}
        }]
    }));
    // Slot 1: missing function name.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 1,
            "id": "invalid_1"
        }]
    }));
    // Slot 2: missing id.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 2,
            "function": {"name": "tool_no_id"}
        }]
    }));
    // Slot 3: valid.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 3,
            "id": "valid_2",
            "function": {"name": "good_2", "arguments": "{}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(
        calls.len(),
        2,
        "MUST keep only 2 valid out of 4 partials; got {}",
        calls.len()
    );
    let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
    assert!(names.contains(&"good_tool"));
    assert!(names.contains(&"good_2"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — call_type default
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finalize_defaults_call_type_to_function_when_unset() {
    // PINS DEFAULT: empty call_type → "function".
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(
        calls[0].call_type, "function",
        "MUST default call_type to 'function' when partial has empty type"
    );
}

#[test]
fn finalize_preserves_explicit_call_type_when_set() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "type": "custom-future-type",
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls[0].call_type, "custom-future-type");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Field propagation through finalize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finalize_propagates_all_four_partial_fields_to_tool_call() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "call_full",
            "type": "function",
            "function": {"name": "fname", "arguments": "{\"k\":\"v\"}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls[0].id, "call_full");
    assert_eq!(calls[0].call_type, "function");
    assert_eq!(calls[0].function.name, "fname");
    assert_eq!(calls[0].function.arguments, r#"{"k":"v"}"#);
}

#[test]
fn finalize_preserves_concatenated_arguments_from_multiple_chunks() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x", "arguments": "{\"key\":"}
        }]
    }));
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"arguments": "\"value\"}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls[0].function.arguments, r#"{"key":"value"}"#);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — has_tool_calls predicate
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_tool_calls_false_on_empty() {
    let acc = ToolCallAccumulator::new();
    assert!(!acc.has_tool_calls());
}

#[test]
fn has_tool_calls_requires_at_least_one_non_empty_id() {
    // AUTHORING DISCOVERY: has_tool_calls checks
    // `any(|tc| !tc.id.is_empty())`, NOT just len > 0.
    // A delta that pre-allocates a slot but never sets `id`
    // returns false for has_tool_calls.
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{"index": 0}]
    }));
    assert!(
        !acc.has_tool_calls(),
        "PINS: slot without id MUST NOT count as a tool call"
    );

    // Now provide an id → predicate flips to true.
    acc.process_delta(&json!({
        "tool_calls": [{"index": 0, "id": "c1"}]
    }));
    assert!(acc.has_tool_calls());
}

#[test]
fn has_tool_calls_remains_true_after_finalize() {
    // finalize is read-only — doesn't drain the accumulator.
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    let _ = acc.finalize();
    assert!(acc.has_tool_calls(), "finalize MUST NOT drain accumulator");
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — clear lifecycle
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clear_resets_to_no_tool_calls() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    assert!(acc.has_tool_calls());
    acc.clear();
    assert!(!acc.has_tool_calls());
    assert!(acc.finalize().is_empty());
}

#[test]
fn clear_on_already_empty_is_safe() {
    let mut acc = ToolCallAccumulator::new();
    acc.clear();
    acc.clear();
    assert!(!acc.has_tool_calls());
}

#[test]
fn clear_then_process_starts_fresh() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x"}
        }]
    }));
    acc.clear();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c2",
            "function": {"name": "y", "arguments": "{}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "c2");
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Determinism
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finalize_repeated_yields_same_result() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "c1",
            "function": {"name": "x", "arguments": "{}"}
        }]
    }));
    let c1 = acc.finalize();
    let c2 = acc.finalize();
    let c3 = acc.finalize();
    assert_eq!(c1.len(), c2.len());
    assert_eq!(c2.len(), c3.len());
    assert_eq!(c1[0].id, c2[0].id);
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — PartialToolCall Default + Clone
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn partial_tool_call_default_yields_empty_fields() {
    let p = PartialToolCall::default();
    assert_eq!(p.index, 0);
    assert!(p.id.is_empty());
    assert!(p.call_type.is_empty());
    assert!(p.function_name.is_empty());
    assert!(p.function_arguments.is_empty());
}

#[test]
fn partial_tool_call_clone_preserves_all_fields() {
    let original = PartialToolCall {
        index: 7,
        id: "id-marker".to_string(),
        call_type: "function".to_string(),
        function_name: "fname-marker".to_string(),
        function_arguments: "{}".to_string(),
    };
    let cloned = original.clone();
    assert_eq!(cloned.index, 7);
    assert_eq!(cloned.id, "id-marker");
    assert_eq!(cloned.call_type, "function");
    assert_eq!(cloned.function_name, "fname-marker");
    assert_eq!(cloned.function_arguments, "{}");
    // Original still usable.
    assert_eq!(original.id, "id-marker");
}
